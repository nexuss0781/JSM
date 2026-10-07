use std::{
    fmt::Write as _,
    path::PathBuf,
    thread,
    time::{Duration, SystemTime},
};

use clap::{Parser, Subcommand, ValueEnum};
use jsm_cli::config::{Config as CliConfig, Credential, Layer as ConfigLayer, LayeredConfig};
use jsm_cli::manifest::{DependencyType as ManifestDependencyType, Manifest, SaveMode};
use jsm_cli::progress::OperationProgress;
use jsm_fetch::{CancellationToken, extract_tarball_with_options};
use jsm_linker::{LinkOptions, link};
use jsm_lockfile::{Importer, Lockfile, Package, package_instance_key, peer_context_hash};
use jsm_registry::{PackageMetadata, Packument, Registry, RegistryConfig, RegistryError};
use jsm_resolver::{Candidate, Provider, Resolver};
use jsm_store::Store;
use miette::{IntoDiagnostic, Result, miette};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::{self, Read},
    path::Path,
    process::{Child, Command as ProcessCommand, ExitStatus, Stdio},
};
use tracing::{
    Event, Subscriber, debug,
    field::{Field, Visit},
    info, info_span,
};
use tracing_subscriber::{
    EnvFilter,
    fmt::{FmtContext, FormatEvent, FormatFields, format::Writer},
    layer::{Layer, SubscriberExt},
    registry::LookupSpan,
    util::SubscriberInitExt,
};

use jsm_core::{DependencySpec, DistTag, Integrity, PackageName, Range, Spec, Version, redact};

#[derive(Debug, Parser, Clone)]
#[command(name = "jsm", version, about = "Rust JavaScript package manager")]
struct Cli {
    #[arg(long, global = true, default_value = ".")]
    cwd: PathBuf,
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true)]
    quiet: bool,
    #[arg(long, global = true, conflicts_with = "quiet")]
    verbose: bool,
    #[arg(long, global = true)]
    offline: bool,
    #[arg(long, global = true)]
    prefer_offline: bool,
    #[arg(long, global = true)]
    store_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    registry: Option<String>,
    #[arg(long, global = true)]
    no_color: bool,
    #[arg(long, global = true)]
    non_interactive: bool,
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Write a Chrome trace for diagnostic runs.
    #[arg(long, value_name = "FILE")]
    trace: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum FrozenLockfileMode {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Subcommand)]
enum Command {
    /// Emit the Phase 0 observability sample (hidden from normal command listings).
    #[command(name = "phase0-demo", hide = true)]
    Phase0Demo,
    Init {
        #[arg(short = 'y', long)]
        yes: bool,
    },
    Add {
        packages: Vec<String>,
        #[arg(short = 'D', conflicts_with_all = ["optional", "peer"])]
        dev: bool,
        #[arg(short = 'O', conflicts_with_all = ["dev", "peer"])]
        optional: bool,
        #[arg(short = 'P', conflicts_with_all = ["dev", "optional"])]
        peer: bool,
        #[arg(long = "save-exact", alias = "exact")]
        exact: bool,
        #[arg(long = "save-prefix", default_value_t = '^')]
        save_prefix: char,
    },
    #[command(aliases = ["i", "up"])]
    Install {
        #[arg(
            long,
            alias = "frozen",
            value_enum,
            value_name = "MODE",
            num_args = 0..=1,
            default_missing_value = "always",
            require_equals = true,
            help = "Freeze lockfile updates (auto follows CI; bare flag means always)"
        )]
        frozen_lockfile: Option<FrozenLockfileMode>,
        #[arg(long)]
        prod: bool,
        #[arg(long)]
        no_lockfile: bool,
    },
    #[command(alias = "rm")]
    Remove {
        packages: Vec<String>,
    },
    Run {
        script: String,
        args: Vec<String>,
    },
    Exec {
        command: String,
        args: Vec<String>,
    },
    #[command(alias = "ls")]
    List,
    Why {
        package: String,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}
#[derive(Debug, Clone, Subcommand)]
enum ConfigCommand {
    Get {
        key: String,
        #[arg(long, conflicts_with = "project")]
        global: bool,
        #[arg(long)]
        project: bool,
    },
    Set {
        key: String,
        value: String,
        #[arg(long, conflicts_with = "project")]
        global: bool,
        #[arg(long)]
        project: bool,
    },
    List {
        #[arg(long, conflicts_with = "project")]
        global: bool,
        #[arg(long)]
        project: bool,
    },
    Delete {
        key: String,
        #[arg(long, conflicts_with = "project")]
        global: bool,
        #[arg(long)]
        project: bool,
    },
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("{message}")]
struct CommandExit {
    code: i32,
    message: String,
}

struct RedactingEventFormat;
#[derive(Default)]
struct EventFields(String);
impl Visit for EventFields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        let _ = write!(&mut self.0, "{}={value:?}", field.name());
    }
}
impl<S, N> FormatEvent<S, N> for RedactingEventFormat
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        context: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let mut fields = EventFields::default();
        event.record(&mut fields);
        let safe_fields = redact(&fields.0);
        write!(
            writer,
            "{:?} {} {} ",
            SystemTime::now(),
            event.metadata().level(),
            event.metadata().target()
        )?;
        if let Some(scope) = context.event_scope() {
            for span in scope.from_root() {
                write!(writer, "{}:", span.name())?;
            }
            writer.write_char(' ')?;
        }
        writeln!(writer, "{safe_fields}")
    }
}
fn filter_for(cli: &Cli) -> EnvFilter {
    if cli.quiet {
        EnvFilter::new("error")
    } else if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::try_from_env("JSM_LOG").unwrap_or_else(|_| EnvFilter::new("info"))
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cancellation = CancellationToken::new();
    let signal_cancellation = cancellation.clone();
    ctrlc::set_handler(move || signal_cancellation.cancel()).into_diagnostic()?;
    let formatting = tracing_subscriber::fmt::layer()
        .event_format(RedactingEventFormat)
        .with_writer(std::io::stderr)
        .with_filter(filter_for(&cli));
    let trace_guard = if let Some(trace_path) = &cli.trace {
        let (chrome_layer, guard) = tracing_chrome::ChromeLayerBuilder::new()
            .file(trace_path)
            .include_args(false)
            .build();
        tracing_subscriber::registry()
            .with(formatting)
            .with(chrome_layer)
            .try_init()
            .into_diagnostic()?;
        Some(guard)
    } else {
        tracing_subscriber::registry()
            .with(formatting)
            .try_init()
            .into_diagnostic()?;
        None
    };
    let result = run(cli, &cancellation);
    drop(trace_guard);
    if cancellation.is_cancelled() {
        eprintln!("operation interrupted");
        std::process::exit(130);
    }
    if let Err(error) = result {
        if let Some(exit) = error.downcast_ref::<CommandExit>() {
            std::process::exit(exit.code);
        }
        return Err(error);
    }
    Ok(())
}

fn run(cli: Cli, cancellation: &CancellationToken) -> Result<()> {
    ensure_not_cancelled(cancellation)?;
    let cwd = fs::canonicalize(&cli.cwd).into_diagnostic()?;
    let command = cli.command.clone().unwrap_or(Command::Install {
        frozen_lockfile: None,
        prod: false,
        no_lockfile: false,
    });
    if let Command::Config { command } = command {
        return config_cmd(&cwd, command, cli.json);
    }
    match command {
        Command::Phase0Demo => phase0_demo(),
        Command::Init { .. } => init(&cwd, cli.json),
        Command::Add {
            packages,
            dev,
            optional,
            peer,
            exact,
            save_prefix,
        } => {
            let mut progress = OperationProgress::start(
                "Adding and installing packages",
                cli.quiet,
                cli.json,
                cli.non_interactive || ci_enabled(),
            );
            add(
                &cli,
                &cwd,
                AddOptions {
                    packages,
                    dev,
                    optional,
                    peer,
                    exact,
                    save_prefix,
                },
                cancellation,
                &mut progress,
            )
        }
        Command::Install {
            frozen_lockfile,
            prod,
            no_lockfile,
        } => {
            let mut progress = OperationProgress::start(
                "Resolving, fetching, and linking dependencies",
                cli.quiet,
                cli.json,
                cli.non_interactive || ci_enabled(),
            );
            let frozen = match frozen_lockfile {
                Some(FrozenLockfileMode::Always) => true,
                Some(FrozenLockfileMode::Never) => false,
                Some(FrozenLockfileMode::Auto) | None => ci_enabled(),
            };
            install(
                &cli,
                &cwd,
                frozen,
                prod,
                no_lockfile,
                cancellation,
                &mut progress,
            )
        }
        Command::Remove { packages } => {
            let mut progress = OperationProgress::start(
                "Removing and updating packages",
                cli.quiet,
                cli.json,
                cli.non_interactive || ci_enabled(),
            );
            remove(&cli, &cwd, packages, cli.json, cancellation, &mut progress)
        }
        Command::List => list(&cwd, cli.json),
        Command::Why { package } => why(&cwd, &package, cli.json),
        Command::Run { script, args } => run_script(&cwd, &script, &args, cli.json, cancellation),
        Command::Exec { command, args } => run_exec(&cwd, &command, &args, cli.json, cancellation),
        Command::Config { .. } => unreachable!(),
    }
}

fn ensure_not_cancelled(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        return Err(CommandExit {
            code: 130,
            message: "operation interrupted".into(),
        }
        .into());
    }
    Ok(())
}

fn ci_enabled() -> bool {
    ["CI", "JSM_CI"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .any(|value| ci_value_enabled(&value))
}

fn ci_value_enabled(value: &str) -> bool {
    !value.trim().is_empty()
        && !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        )
}

fn phase0_demo() -> Result<()> {
    let install = info_span!("install", mode = "phase0-demo");
    let _install = install.enter();
    for stage in ["resolve", "fetch", "extract", "write", "link", "build"] {
        let _stage = match stage {
            "resolve" => info_span!("resolve").entered(),
            "fetch" => info_span!("fetch").entered(),
            "extract" => info_span!("extract").entered(),
            "write" => info_span!("write").entered(),
            "link" => info_span!("link").entered(),
            _ => info_span!("build").entered(),
        };
        info!(stage, "phase 0 instrumentation sample");
        if stage == "resolve" {
            debug!(
                password = "phase0-hidden-canary",
                "redaction regression canary"
            );
        }
    }
    println!("Phase 0 tracing demo completed; no package was installed.");
    Ok(())
}
fn manifest_path(cwd: &Path) -> PathBuf {
    cwd.join("package.json")
}
fn read_manifest(cwd: &Path) -> Result<Value> {
    serde_json::from_str(&fs::read_to_string(manifest_path(cwd)).into_diagnostic()?)
        .into_diagnostic()
}
fn init(cwd: &Path, out_json: bool) -> Result<()> {
    let p = manifest_path(cwd);
    if !p.exists() {
        fs::write(
            &p,
            "{\n  \"name\": \"project\",\n  \"version\": \"1.0.0\"\n}\n",
        )
        .into_diagnostic()?;
    }
    if out_json {
        emit_json("init", json!({"command":"init","created":p.exists()}));
    } else {
        println!("Initialized project in {}", cwd.display());
    }
    Ok(())
}
struct AddOptions {
    packages: Vec<String>,
    dev: bool,
    optional: bool,
    peer: bool,
    exact: bool,
    save_prefix: char,
}

fn add(
    cli: &Cli,
    cwd: &Path,
    options: AddOptions,
    cancellation: &CancellationToken,
    progress: &mut OperationProgress,
) -> Result<()> {
    let AddOptions {
        packages,
        dev,
        optional,
        peer,
        exact,
        save_prefix,
    } = options;
    if packages.is_empty() {
        return Err(miette!("add requires at least one package spec"));
    }
    let path = manifest_path(cwd);
    let original = fs::read_to_string(&path).into_diagnostic()?;
    let mut manifest =
        Manifest::parse(path.display().to_string(), original.clone()).into_diagnostic()?;
    let (kind, field) = if dev {
        (ManifestDependencyType::DevDependencies, "devDependencies")
    } else if optional {
        (
            ManifestDependencyType::OptionalDependencies,
            "optionalDependencies",
        )
    } else if peer {
        (ManifestDependencyType::PeerDependencies, "peerDependencies")
    } else {
        (ManifestDependencyType::Dependencies, "dependencies")
    };
    let mut added = Vec::new();
    for raw in packages {
        let (name, version) = raw
            .rsplit_once('@')
            .filter(|(n, v)| !n.is_empty() && !v.is_empty())
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .unwrap_or((raw, "latest".into()));
        PackageName::new(name.clone()).into_diagnostic()?;
        let preserve = version == "latest"
            || version.starts_with(['>', '<', '=', '*'])
            || version.contains("||")
            || version.contains('x')
            || DistTag::new(version.clone()).is_ok() && Range::new(&version).is_err();
        let mode = if exact {
            SaveMode::Exact
        } else if preserve {
            SaveMode::Preserve
        } else {
            SaveMode::Prefix
        };
        manifest
            .add_dependency(kind, &name, &version, mode, save_prefix)
            .into_diagnostic()?;
        let spec = manifest
            .dependency(kind, &name)
            .ok_or_else(|| miette!("failed to add dependency `{name}`"))?;
        parse_spec(spec)?;
        added.push(name);
    }
    fs::write(&path, manifest.source()).into_diagnostic()?;
    let mut silent = cli.clone();
    silent.json = false;
    silent.quiet = true;
    if let Err(error) = install(&silent, cwd, false, false, false, cancellation, progress) {
        fs::write(&path, original).into_diagnostic()?;
        return Err(error);
    }
    if cli.json {
        emit_json(
            "add",
            json!({"command":"add","field":field,"packages":added}),
        );
    } else if !cli.quiet {
        println!("updated {}", field);
    }
    Ok(())
}
fn effective_config(cli: &Cli, cwd: &Path) -> Result<CliConfig> {
    let mut layers = LayeredConfig::default();
    layers.set(
        ConfigLayer::Defaults,
        CliConfig {
            registry: Some(RegistryConfig::default().default_registry),
            ..Default::default()
        },
    );
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    if let Some(home) = home
        && let Some(c) = load_level(&home)?
    {
        layers.set(ConfigLayer::User, c);
    }
    if let Some(root) = find_workspace_root(cwd)
        && let Some(c) = load_level(&root)?
    {
        layers.set(ConfigLayer::Workspace, c);
    }
    let project_layer = if let Some(config_path) = &cli.config {
        let path = if config_path.is_absolute() {
            config_path.clone()
        } else {
            cwd.join(config_path)
        };
        if !path.is_file() {
            return Err(miette!(
                "configuration file does not exist: {}",
                path.display()
            ));
        }
        Some(load_config_file(&path)?)
    } else {
        load_level(cwd)?
    };
    if let Some(c) = project_layer {
        layers.set(ConfigLayer::Project, c);
    }
    let mut environment = LayeredConfig::try_from_environment().into_diagnostic()?;
    if environment.registry.is_none() {
        environment.registry = std::env::var("npm_config_registry")
            .ok()
            .or_else(|| std::env::var("NPM_CONFIG_REGISTRY").ok());
    }
    if environment.proxy.is_none() {
        environment.proxy = std::env::var("npm_config_https_proxy")
            .ok()
            .or_else(|| std::env::var("npm_config_proxy").ok());
    }
    if environment.no_proxy.is_none() {
        environment.no_proxy = std::env::var("NO_PROXY")
            .ok()
            .or_else(|| std::env::var("no_proxy").ok());
    }
    layers.set(ConfigLayer::Environment, environment);
    layers.set(
        ConfigLayer::Cli,
        CliConfig {
            registry: cli.registry.clone(),
            store_dir: cli
                .store_dir
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            ..Default::default()
        },
    );
    Ok(layers.resolve())
}

fn registry(config: &CliConfig, cli: &Cli, cwd: &Path) -> Result<Registry> {
    let mut c = RegistryConfig::default();
    c.default_registry = config.registry.clone().unwrap_or(c.default_registry);
    c.scoped_registries = config.scopes.clone().into_iter().collect();
    if let Some(a) = config.default_auth.clone() {
        c.default_auth = Some(to_registry_auth(a));
    }
    c.scoped_auth = config
        .scoped_auth
        .clone()
        .into_iter()
        .map(|(s, a)| (s, to_registry_auth(a)))
        .collect();
    c.registry_auth = config
        .registry_auth
        .clone()
        .into_iter()
        .map(|(registry, auth)| (registry, to_registry_auth(auth)))
        .collect();
    c.proxy = config.proxy.clone();
    c.no_proxy = config.no_proxy.clone();
    c.strict_ssl = config.strict_ssl.unwrap_or(true);
    c.custom_ca_bundle = config.cafile.as_deref().map(|value| {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }
    });
    c.offline = cli.offline;
    c.prefer_offline = cli.prefer_offline || config.prefer_offline.unwrap_or(false);
    Registry::new(c).into_diagnostic()
}
fn load_level(dir: &Path) -> Result<Option<CliConfig>> {
    let mut result = None;
    for name in [".npmrc", ".jsmrc", "jsm.toml"] {
        let path = dir.join(name);
        if path.is_file() {
            let next = load_config_file(&path)?;
            let mut merged: CliConfig = result.take().unwrap_or_default();
            if next.registry.is_some() {
                merged.registry = next.registry;
            }
            if next.proxy.is_some() {
                merged.proxy = next.proxy;
            }
            if next.no_proxy.is_some() {
                merged.no_proxy = next.no_proxy;
            }
            if next.prefer_offline.is_some() {
                merged.prefer_offline = next.prefer_offline;
            }
            if next.strict_ssl.is_some() {
                merged.strict_ssl = next.strict_ssl;
            }
            if next.cafile.is_some() {
                merged.cafile = next.cafile;
            }
            if next.store_dir.is_some() {
                merged.store_dir = next.store_dir;
            }
            if next.default_auth.is_some() {
                merged.default_auth = next.default_auth;
            }
            merged.scopes.extend(next.scopes);
            merged.scoped_auth.extend(next.scoped_auth);
            merged.registry_auth.extend(next.registry_auth);
            result = Some(merged);
        }
    }
    Ok(result)
}
fn find_workspace_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors().find_map(|ancestor| {
        let marked = ancestor.join("jsm-workspace.yaml").is_file()
            || ancestor.join(".jsm-workspace").is_file();
        let has_workspaces = ancestor
            .join("package.json")
            .is_file()
            .then(|| fs::read_to_string(ancestor.join("package.json")).ok())
            .flatten()
            .and_then(|contents| serde_json::from_str::<Value>(&contents).ok())
            .is_some_and(|manifest| manifest.get("workspaces").is_some());
        (marked || has_workspaces).then(|| ancestor.to_path_buf())
    })
}
fn load_config_file(path: &Path) -> Result<CliConfig> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if name == ".npmrc" {
        CliConfig::from_npmrc(path).into_diagnostic()
    } else if name == ".jsmrc" {
        CliConfig::from_jsmrc(path).into_diagnostic()
    } else {
        CliConfig::from_jsm_toml(path).into_diagnostic()
    }
}
fn to_registry_auth(a: Credential) -> jsm_registry::Auth {
    match a {
        Credential::Bearer(v) => jsm_registry::Auth::Bearer(v),
        Credential::Basic { username, password } => {
            jsm_registry::Auth::Basic { username, password }
        }
    }
}

#[derive(Clone)]
struct RegistryProvider {
    registry: Registry,
}

impl Provider for RegistryProvider {
    type Error = RegistryError;

    fn candidates(
        &self,
        package: &PackageName,
    ) -> std::result::Result<Vec<Candidate>, Self::Error> {
        let packument = self.registry.packument(package.as_str())?;
        validate_packument_identity(package, &packument)?;
        let mut tags = BTreeMap::new();
        for (tag, version) in packument.dist_tags {
            let tag = DistTag::new(tag.clone())
                .map_err(|_| RegistryError::InvalidMetadata(format!("invalid dist-tag `{tag}`")))?;
            let version = Version::parse(&version).map_err(|_| {
                RegistryError::InvalidMetadata(format!("invalid dist-tag version `{version}`"))
            })?;
            tags.insert(tag, version);
        }
        let mut out = Vec::new();
        for (raw_version, metadata) in packument.versions {
            let version_text = metadata.version.as_deref().unwrap_or(&raw_version);
            let Ok(version) = Version::parse(version_text) else {
                continue;
            };
            let Some(dependencies) = parse_candidate_dependencies(&metadata.dependencies) else {
                debug!(package = %package, version = %version, "skipping registry candidate with unsupported dependency metadata");
                continue;
            };
            let Some(optional_dependencies) =
                parse_candidate_dependencies(&metadata.optional_dependencies)
            else {
                debug!(package = %package, version = %version, "skipping registry candidate with unsupported optional dependency metadata");
                continue;
            };
            let Some(peer_dependencies) = parse_candidate_dependencies(&metadata.peer_dependencies)
            else {
                debug!(package = %package, version = %version, "skipping registry candidate with unsupported peer dependency metadata");
                continue;
            };
            let mut dependencies = dependencies;
            let mut optional_dependencies = optional_dependencies;
            for optional in optional_dependencies.keys() {
                dependencies.remove(optional);
            }
            for (peer, spec) in peer_dependencies {
                let optional_peer = metadata
                    .peer_dependencies_meta
                    .get(peer.as_str())
                    .is_some_and(|meta| meta.optional);
                // npm does not automatically install optional peer dependencies.
                if !optional_peer {
                    dependencies.entry(peer).or_insert(spec);
                }
            }
            optional_dependencies.retain(|name, _| !dependencies.contains_key(name));
            let mut candidate = Candidate::new(version);
            candidate.dependencies = dependencies;
            candidate.optional_dependencies = optional_dependencies;
            candidate.dist_tags = tags.clone();
            candidate.deprecated = metadata.deprecated;
            out.push(candidate);
        }
        Ok(out)
    }

    fn is_missing_package(&self, error: &Self::Error) -> bool {
        matches!(error, RegistryError::Http(404))
    }
}

fn validate_packument_identity(
    package: &PackageName,
    packument: &Packument,
) -> std::result::Result<(), RegistryError> {
    if let Some(name) = packument.name.as_deref()
        && name != package.as_str()
    {
        return Err(RegistryError::InvalidMetadata(format!(
            "packument name `{name}` does not match requested package `{package}`"
        )));
    }
    for (version_key, metadata) in &packument.versions {
        if let Some(name) = metadata.name.as_deref()
            && name != package.as_str()
        {
            return Err(RegistryError::InvalidMetadata(format!(
                "version `{version_key}` declares package `{name}` instead of `{package}`"
            )));
        }
        if let Some(version) = metadata.version.as_deref()
            && version != version_key
        {
            return Err(RegistryError::InvalidMetadata(format!(
                "version key `{version_key}` disagrees with declared version `{version}`"
            )));
        }
    }
    Ok(())
}

fn parse_candidate_dependencies(
    raw: &HashMap<String, String>,
) -> Option<BTreeMap<PackageName, Spec>> {
    raw.iter()
        .map(|(name, spec)| Some((PackageName::new(name.clone()).ok()?, parse_spec(spec).ok()?)))
        .collect()
}

fn parse_spec(raw: &str) -> Result<Spec> {
    let raw = raw.trim();
    if raw.starts_with("http:")
        || raw.starts_with("https:")
        || raw.starts_with("git")
        || raw.starts_with("file:")
        || raw.starts_with("workspace:")
    {
        return Err(miette!("unsupported dependency spec `{raw}`"));
    }
    if let Ok(tag) = DistTag::new(raw.to_owned())
        && !raw
            .chars()
            .any(|c| c.is_ascii_digit() || matches!(c, '^' | '~' | '<' | '>' | '=' | '*' | '|'))
    {
        return Ok(Spec::Tag(tag));
    }
    Ok(Spec::Registry(Range::new(raw).into_diagnostic()?))
}

fn manifest_importer(m: &Value, prod: bool) -> Result<(Importer, Vec<DependencySpec>)> {
    let mut importer = Importer::default();
    let mut roots = Vec::new();
    for (field, target) in [
        ("dependencies", &mut importer.dependencies),
        ("devDependencies", &mut importer.dev_dependencies),
        ("optionalDependencies", &mut importer.optional_dependencies),
    ] {
        if prod && field != "dependencies" {
            continue;
        }
        if let Some(obj) = m.get(field).and_then(Value::as_object) {
            for (name, value) in obj {
                let raw = value
                    .as_str()
                    .ok_or_else(|| miette!("{field}.{name} must be a string"))?;
                let package = PackageName::new(name.clone()).into_diagnostic()?;
                let spec = parse_spec(raw)?;
                target.insert(name.clone(), raw.to_owned());
                if field != "optionalDependencies" {
                    roots.push(DependencySpec::new(package, spec));
                }
            }
        }
    }
    Ok((importer, roots))
}

fn resolve_lock(reg: &Registry, importer: &Importer, roots: &[DependencySpec]) -> Result<Lockfile> {
    let optional_roots = importer
        .optional_dependencies
        .iter()
        .map(|(name, spec)| {
            Ok(DependencySpec::new(
                PackageName::new(name.clone()).into_diagnostic()?,
                parse_spec(spec)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let resolution = Resolver::new(RegistryProvider {
        registry: reg.clone(),
    })
    .resolve_with_optional::<RegistryError>(roots, &optional_roots)
    .into_diagnostic()?;
    let mut lock = Lockfile::new("jsm 0.1.0");
    let mut metadata_by_id = BTreeMap::<String, PackageMetadata>::new();
    let mut key_map = BTreeMap::<String, String>::new();
    for (id, resolved) in &resolution.packages {
        let metadata = reg
            .packument(resolved.name.as_str())
            .into_diagnostic()?
            .versions
            .remove(&resolved.version.to_string())
            .ok_or_else(|| {
                miette!(
                    "metadata missing for {}@{}",
                    resolved.name,
                    resolved.version
                )
            })?;
        let integrity = Integrity::new(metadata.dist.integrity.clone().ok_or_else(|| {
            miette!(
                "missing integrity for {}@{}",
                resolved.name,
                resolved.version
            )
        })?)
        .into_diagnostic()?;
        let peer_context = BTreeMap::new();
        let key = package_instance_key(
            resolved.name.as_str(),
            &resolved.version.to_string(),
            integrity.as_str(),
            &peer_context,
        );
        key_map.insert(id.clone(), key);
        metadata_by_id.insert(id.clone(), metadata);
    }
    let root_ids = resolution
        .root_dependencies
        .iter()
        .map(|(name, id)| {
            let key = key_map
                .get(id)
                .cloned()
                .ok_or_else(|| miette!("unresolved dependency {name}"))?;
            Ok((name.as_str().to_owned(), key))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut locked_importer = importer.clone();
    for name in importer.dependencies.keys() {
        locked_importer.resolved_dependencies.insert(
            name.clone(),
            root_ids
                .get(name)
                .cloned()
                .ok_or_else(|| miette!("unresolved dependency {name}"))?,
        );
    }
    for name in importer.dev_dependencies.keys() {
        locked_importer.resolved_dev_dependencies.insert(
            name.clone(),
            root_ids
                .get(name)
                .cloned()
                .ok_or_else(|| miette!("unresolved dependency {name}"))?,
        );
    }
    for name in importer.optional_dependencies.keys() {
        if let Some(id) = root_ids.get(name) {
            locked_importer
                .resolved_optional_dependencies
                .insert(name.clone(), id.clone());
        }
    }
    lock.importers.insert(".".into(), locked_importer);
    for (id, resolved) in resolution.packages {
        let name = resolved.name.as_str();
        let version = resolved.version.to_string();
        let metadata = metadata_by_id
            .remove(&id)
            .ok_or_else(|| miette!("metadata missing for {name}@{version}"))?;
        let integrity = Integrity::new(
            metadata
                .dist
                .integrity
                .clone()
                .ok_or_else(|| miette!("missing integrity for {name}@{version}"))?,
        )
        .into_diagnostic()?;
        let resolution_url = metadata
            .dist
            .tarball
            .clone()
            .ok_or_else(|| miette!("missing tarball for {name}@{version}"))?;
        let peer_context = BTreeMap::new();
        let dependencies = resolved
            .resolved_dependencies
            .iter()
            .map(|(dependency, child)| {
                key_map
                    .get(child)
                    .cloned()
                    .map(|key| (dependency.as_str().to_owned(), key))
                    .ok_or_else(|| miette!("unresolved dependency {dependency}"))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let optional_dependencies = resolved
            .resolved_optional_dependencies
            .iter()
            .map(|(dependency, child)| {
                key_map
                    .get(child)
                    .cloned()
                    .map(|key| (dependency.as_str().to_owned(), key))
                    .ok_or_else(|| miette!("unresolved optional dependency {dependency}"))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let key = key_map
            .get(&id)
            .cloned()
            .ok_or_else(|| miette!("missing v2 package key for {name}@{version}"))?;
        lock.packages.insert(
            key,
            Package {
                name: name.to_owned(),
                version,
                peer_context_hash: peer_context_hash(&peer_context),
                peer_context,
                resolution: resolution_url,
                integrity: integrity.to_string(),
                dependencies,
                optional_dependencies,
                peer_dependencies: metadata.peer_dependencies.into_iter().collect(),
                engines: metadata.engines.into_iter().collect(),
                os: metadata.os,
                cpu: metadata.cpu,
                libc: metadata.libc,
                has_scripts: !metadata.scripts.is_empty(),
                ..Default::default()
            },
        );
    }
    Ok(lock)
}

fn install(
    cli: &Cli,
    cwd: &Path,
    frozen: bool,
    prod: bool,
    no_lockfile: bool,
    cancellation: &CancellationToken,
    progress: &mut OperationProgress,
) -> Result<()> {
    ensure_not_cancelled(cancellation)?;
    let manifest = read_manifest(cwd)?;
    let (importer, roots) = manifest_importer(&manifest, prod)?;
    let lock_path = cwd.join("jsm.lock");
    let config = effective_config(cli, cwd)?;
    let reg = registry(&config, cli, cwd)?;
    let lock = if no_lockfile {
        resolve_lock(&reg, &importer, &roots)?
    } else if lock_path.exists() {
        let existing = Lockfile::read(&lock_path).map_err(|error| CommandExit {
            code: 6,
            message: error.to_string(),
        })?;
        existing.validate_version().map_err(|error| CommandExit {
            code: 6,
            message: error.to_string(),
        })?;
        let expected = BTreeMap::from([(String::from("."), importer.clone())]);
        if existing.is_stale(&expected) {
            if frozen {
                return Err(CommandExit {
                    code: 6,
                    message: "frozen install: lockfile is stale".into(),
                }
                .into());
            }
            resolve_lock(&reg, &importer, &roots)?
        } else if existing.lockfile_version == jsm_lockfile::LEGACY_LOCKFILE_VERSION {
            if frozen {
                existing
                    .migrate_peer_free_v1()
                    .map_err(|error| CommandExit {
                        code: 6,
                        message: error.to_string(),
                    })?
            } else {
                resolve_lock(&reg, &importer, &roots)?
            }
        } else {
            existing
        }
    } else {
        if frozen {
            return Err(CommandExit {
                code: 6,
                message: "frozen install requires jsm.lock".into(),
            }
            .into());
        }
        resolve_lock(&reg, &importer, &roots)?
    };
    ensure_not_cancelled(cancellation)?;
    let configured_store = cli
        .store_dir
        .clone()
        .or_else(|| config.store_dir.as_deref().map(PathBuf::from));
    let store = Store::open_for_project(cwd, configured_store.as_deref()).into_diagnostic()?;
    for (id, package) in &lock.packages {
        ensure_not_cancelled(cancellation)?;
        let name = PackageName::new(package.name.clone()).into_diagnostic()?;
        let version = Version::parse(&package.version).into_diagnostic()?;
        let version_text = version.to_string();
        let integrity = Integrity::new(package.integrity.clone()).into_diagnostic()?;
        if !store.has_package(name.as_str(), &version_text, integrity.as_str()) {
            progress.package_started(&format!("Fetching {name}@{version_text}"));
            let body = reg
                .download_tarball_resumable(name.as_str(), &package.resolution)
                .into_diagnostic()?;
            let mut fetch_progress = |event: jsm_fetch::ProgressEvent| {
                progress.update_detail(&format!(
                    "Extracting {name}@{version_text}: {} files, {} bytes",
                    event.entries, event.uncompressed_bytes
                ));
                debug!(
                    package = %name,
                    entries = event.entries,
                    uncompressed_bytes = event.uncompressed_bytes,
                    "package extraction progress"
                );
            };
            extract_tarball_with_options(
                &store,
                name.as_str(),
                &version_text,
                integrity.as_str(),
                body,
                jsm_fetch::ExtractOptions {
                    limits: Default::default(),
                    cancellation,
                    progress: Some(&mut fetch_progress),
                },
            )
            .map_err(|error| miette!("failed to extract package `{id}`: {error}"))?;
        }
    }
    ensure_not_cancelled(cancellation)?;
    link(
        cwd,
        &store,
        &lock,
        LinkOptions {
            include_dev: !prod,
            ..Default::default()
        },
    )
    .into_diagnostic()?;
    if !no_lockfile && !frozen {
        lock.write(&lock_path).into_diagnostic()?;
    }
    if cli.json {
        emit_json(
            "install",
            json!({"command":"install","packages":lock.packages.len(),"lockfile":!no_lockfile}),
        );
    } else if !cli.quiet {
        println!("installed {} packages", lock.packages.len());
    }
    Ok(())
}

fn remove(
    cli: &Cli,
    cwd: &Path,
    packages: Vec<String>,
    out: bool,
    cancellation: &CancellationToken,
    progress: &mut OperationProgress,
) -> Result<()> {
    if packages.is_empty() {
        return Err(miette!("remove requires at least one package name"));
    }
    let path = manifest_path(cwd);
    let original = fs::read_to_string(&path).into_diagnostic()?;
    let mut manifest =
        Manifest::parse(path.display().to_string(), original.clone()).into_diagnostic()?;
    for kind in [
        ManifestDependencyType::Dependencies,
        ManifestDependencyType::DevDependencies,
        ManifestDependencyType::OptionalDependencies,
        ManifestDependencyType::PeerDependencies,
    ] {
        for package in &packages {
            manifest
                .remove_dependency(kind, package)
                .into_diagnostic()?;
        }
    }
    fs::write(&path, manifest.source()).into_diagnostic()?;
    let mut silent = cli.clone();
    silent.json = false;
    silent.quiet = true;
    if let Err(error) = install(&silent, cwd, false, false, false, cancellation, progress) {
        fs::write(manifest_path(cwd), original).into_diagnostic()?;
        return Err(error);
    }
    if out {
        emit_json("remove", json!({"command":"remove","packages":packages}));
    } else {
        println!("removed {}", packages.join(", "));
    }
    Ok(())
}
fn list(cwd: &Path, out: bool) -> Result<()> {
    let m = read_manifest(cwd)?;
    let mut v = Vec::new();
    for f in ["dependencies", "devDependencies", "optionalDependencies"] {
        if let Some(o) = m.get(f).and_then(Value::as_object) {
            for (n, x) in o {
                v.push(json!({"name":n,"spec":x,"type":f}));
            }
        }
    }
    if out {
        emit_json("list", json!({"command":"list","packages":v}));
    } else {
        for x in v {
            println!("{} {}", x["name"], x["spec"]);
        }
    }
    Ok(())
}
fn why(cwd: &Path, p: &str, out: bool) -> Result<()> {
    let m = read_manifest(cwd)?;
    let found = [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ]
    .iter()
    .any(|f| {
        m.get(*f)
            .and_then(Value::as_object)
            .is_some_and(|o| o.contains_key(p))
    });
    if out {
        emit_json("why", json!({"command":"why","package":p,"direct":found}));
    } else {
        println!(
            "{} is {}direct dependency",
            p,
            if found { "a " } else { "not a " }
        );
    }
    Ok(())
}
fn emit_json(command: &str, mut value: Value) {
    if let Some(object) = value.as_object_mut() {
        object.insert("schema".into(), Value::String(format!("jsm.v1.{command}")));
    }
    println!("{value}");
}

fn run_script(
    cwd: &Path,
    script: &str,
    args: &[String],
    out_json: bool,
    cancellation: &CancellationToken,
) -> Result<()> {
    let manifest = read_manifest(cwd)?;
    let command_text = manifest
        .get("scripts")
        .and_then(Value::as_object)
        .and_then(|scripts| scripts.get(script))
        .and_then(Value::as_str)
        .ok_or_else(|| miette!("script not found or not a string: {script}"))?;
    let mut command = ProcessCommand::new(if cfg!(windows) { "cmd" } else { "sh" });
    #[cfg(windows)]
    {
        let mut command_line = command_text.to_owned();
        for arg in args {
            command_line.push(' ');
            command_line.push_str(&format!("\"{}\"", arg.replace('"', "\\\"")));
        }
        command.args(["/d", "/s", "/c"]).arg(command_line);
    }
    #[cfg(not(windows))]
    {
        command.args(["-c", command_text, "jsm-run"]).args(args);
    }
    command.current_dir(cwd).env("npm_lifecycle_event", script);
    command.env("PATH", project_path(cwd)?);
    finish_child(command, out_json, "run", Some(script), cancellation)
}

fn run_exec(
    cwd: &Path,
    name: &str,
    args: &[String],
    out_json: bool,
    cancellation: &CancellationToken,
) -> Result<()> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(miette!("invalid executable name `{name}`"));
    }
    let bin_dir = cwd.join("node_modules").join(".bin");
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;

        let target_file = bin_dir.join(format!("{name}.jsm-bin.json"));
        if target_file.is_file() {
            let encoded_target: Vec<u16> =
                serde_json::from_slice(&fs::read(&target_file).into_diagnostic()?)
                    .into_diagnostic()?;
            let target = PathBuf::from(std::ffi::OsString::from_wide(&encoded_target));
            if !target.is_absolute() {
                return Err(miette!(
                    "generated bin target must be absolute: {}",
                    target.display()
                ));
            }
            if !target.is_file() {
                return Err(miette!(
                    "generated bin target does not exist: {}",
                    target.display()
                ));
            }
            let mut command = ProcessCommand::new("node");
            command
                .args([
                    "-e",
                    "const target = process.env.JSM_BIN_TARGET; process.argv = [process.execPath, target, ...process.argv.slice(1)]; require('module').runMain();",
                    "--",
                ])
                .args(args)
                .current_dir(cwd)
                .env("JSM_BIN_TARGET", &target)
                .env("PATH", project_path(cwd)?);
            return finish_child(command, out_json, "exec", Some(name), cancellation);
        }
    }
    #[cfg(windows)]
    let candidates = [
        bin_dir.join(format!("{name}.ps1")),
        bin_dir.join(format!("{name}.cmd")),
        bin_dir.join(format!("{name}.exe")),
        bin_dir.join(name),
    ];
    #[cfg(not(windows))]
    let candidates = [bin_dir.join(name)];
    let executable = candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| miette!("executable not found in node_modules/.bin: {name}"))?;
    #[cfg(windows)]
    let mut command = if executable
        .extension()
        .is_some_and(|extension| extension == "ps1")
    {
        let mut command = ProcessCommand::new("powershell.exe");
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&executable);
        command
    } else if executable
        .extension()
        .is_some_and(|extension| extension == "cmd")
    {
        let mut command = ProcessCommand::new("cmd.exe");
        command.args(["/D", "/C", "call"]).arg(&executable);
        command
    } else {
        ProcessCommand::new(&executable)
    };
    #[cfg(not(windows))]
    let mut command = ProcessCommand::new(&executable);
    command.args(args).current_dir(cwd);
    command.env("PATH", project_path(cwd)?);
    finish_child(command, out_json, "exec", Some(name), cancellation)
}

fn project_path(cwd: &Path) -> Result<std::ffi::OsString> {
    let mut paths = vec![cwd.join("node_modules").join(".bin")];
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }
    std::env::join_paths(paths).into_diagnostic()
}

fn finish_child(
    mut command: ProcessCommand,
    out_json: bool,
    kind: &str,
    name: Option<&str>,
    cancellation: &CancellationToken,
) -> Result<()> {
    ensure_not_cancelled(cancellation)?;
    let (status, stdout, stderr) = if out_json {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .into_diagnostic()?;
        let stdout = child.stdout.take().expect("piped stdout is present");
        let stderr = child.stderr.take().expect("piped stderr is present");
        let stdout_reader = thread::spawn(move || read_all(stdout));
        let stderr_reader = thread::spawn(move || read_all(stderr));
        let status = wait_child(&mut child, cancellation)?;
        (
            status,
            Some(read_child_output(stdout_reader)?),
            Some(read_child_output(stderr_reader)?),
        )
    } else {
        let mut child = command.spawn().into_diagnostic()?;
        let status = wait_child(&mut child, cancellation)?;
        (status, None, None)
    };
    let code = if cancellation.is_cancelled() {
        Some(130)
    } else {
        status.code()
    };
    if out_json {
        emit_json(
            kind,
            json!({
                "command": kind,
                "name": name,
                "exitCode": code,
                "stdout": stdout,
                "stderr": stderr,
            }),
        );
    }
    if status.success() {
        Ok(())
    } else {
        let exit_code = child_exit_code(status);
        Err(CommandExit {
            code: exit_code,
            message: format!("{kind} command exited with status {exit_code}"),
        }
        .into())
    }
}

fn read_all(mut reader: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn read_child_output(reader: thread::JoinHandle<io::Result<Vec<u8>>>) -> Result<String> {
    let bytes = reader
        .join()
        .map_err(|_| miette!("child output reader thread panicked"))?
        .into_diagnostic()?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn wait_child(child: &mut Child, cancellation: &CancellationToken) -> Result<ExitStatus> {
    loop {
        if cancellation.is_cancelled() {
            let _ = child.kill();
            return child.wait().into_diagnostic();
        }
        if let Some(status) = child.try_wait().into_diagnostic()? {
            return Ok(status);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
fn child_exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

#[cfg(not(unix))]
fn child_exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}
fn config_file(cwd: &Path, global: bool) -> PathBuf {
    if global {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.to_path_buf())
            .join("jsm.toml")
    } else {
        cwd.join("jsm.toml")
    }
}
fn config_cmd(cwd: &Path, c: ConfigCommand, out: bool) -> Result<()> {
    let (action, key, value, global, project) = match c {
        ConfigCommand::Get {
            key,
            global,
            project,
        } => ("get", Some(key), None, global, project),
        ConfigCommand::Set {
            key,
            value,
            global,
            project,
        } => ("set", Some(key), Some(value), global, project),
        ConfigCommand::List { global, project } => ("list", None, None, global, project),
        ConfigCommand::Delete {
            key,
            global,
            project,
        } => ("delete", Some(key), None, global, project),
    };
    if global && project {
        return Err(miette!("choose only one config scope"));
    }
    let p = config_file(cwd, global);
    let mut cfg = if p.is_file() {
        CliConfig::from_jsm_toml(&p).into_diagnostic()?
    } else {
        CliConfig::default()
    };
    match action {
        "get" => {
            let k = key.unwrap();
            if out {
                emit_json(
                    "config",
                    json!({"command":"config","action":"get","scope":if global{"global"}else{"project"},"key":k,"value":cfg.get(&k)}),
                );
            } else if let Some(v) = cfg.get(&k) {
                println!("{v}");
            }
        }
        "list" => {
            let vals = cfg.list();
            if out {
                emit_json(
                    "config",
                    json!({"command":"config","action":"list","scope":if global{"global"}else{"project"},"values":vals}),
                );
            } else {
                for (k, v) in vals {
                    println!("{k}={v}");
                }
            }
        }
        "set" => {
            let k = key.unwrap();
            cfg.set_key(&k, &value.unwrap()).into_diagnostic()?;
            write_config_file(&p, &cfg)?;
            if out {
                emit_json(
                    "config",
                    json!({"command":"config","action":"set","scope":if global{"global"}else{"project"},"key":k}),
                );
            }
        }
        "delete" => {
            let k = key.unwrap();
            if cfg.delete_key(&k) {
                write_config_file(&p, &cfg)?;
            }
            if out {
                emit_json(
                    "config",
                    json!({"command":"config","action":"delete","scope":if global{"global"}else{"project"},"key":k}),
                );
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}
fn write_config_file(path: &Path, cfg: &CliConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).into_diagnostic()?;
    }
    let text = cfg.to_toml();
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .into_diagnostic()?;
        file.write_all(text.as_bytes()).into_diagnostic()?;
        file.sync_all().into_diagnostic()?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).into_diagnostic()?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::write(path, text).into_diagnostic()
    }
}

#[cfg(test)]
mod phase1_cli_mode_tests {
    use super::*;

    #[test]
    fn ci_detection_accepts_truthy_values_and_rejects_false_values() {
        for value in ["1", "true", "yes", "on", " CI "] {
            assert!(ci_value_enabled(value), "expected `{value}` to enable CI");
        }
        for value in ["", "0", "false", "no", "off", " False "] {
            assert!(!ci_value_enabled(value), "expected `{value}` to disable CI");
        }
    }

    #[test]
    fn frozen_lockfile_mode_supports_auto_always_never_and_bare_flag() {
        let auto = Cli::try_parse_from(["jsm", "install", "--frozen-lockfile=auto"]).unwrap();
        assert!(matches!(
            auto.command,
            Some(Command::Install {
                frozen_lockfile: Some(FrozenLockfileMode::Auto),
                ..
            })
        ));

        let always = Cli::try_parse_from(["jsm", "install", "--frozen-lockfile"]).unwrap();
        assert!(matches!(
            always.command,
            Some(Command::Install {
                frozen_lockfile: Some(FrozenLockfileMode::Always),
                ..
            })
        ));

        let never = Cli::try_parse_from(["jsm", "install", "--frozen-lockfile=never"]).unwrap();
        assert!(matches!(
            never.command,
            Some(Command::Install {
                frozen_lockfile: Some(FrozenLockfileMode::Never),
                ..
            })
        ));
    }

    #[test]
    fn registry_metadata_identity_must_match_requested_package_and_version() {
        let package = PackageName::new("metadata-pkg").unwrap();
        let mut versions = HashMap::new();
        versions.insert(
            "1.0.0".into(),
            PackageMetadata {
                name: Some("metadata-pkg".into()),
                version: Some("1.0.0".into()),
                ..Default::default()
            },
        );
        let valid = Packument {
            name: Some("metadata-pkg".into()),
            versions,
            ..Default::default()
        };
        assert!(validate_packument_identity(&package, &valid).is_ok());

        let mut wrong_packument_name = valid.clone();
        wrong_packument_name.name = Some("other-pkg".into());
        assert!(matches!(
            validate_packument_identity(&package, &wrong_packument_name),
            Err(RegistryError::InvalidMetadata(_))
        ));

        let mut wrong_version_name = valid.clone();
        wrong_version_name.versions.get_mut("1.0.0").unwrap().name = Some("other-pkg".into());
        assert!(matches!(
            validate_packument_identity(&package, &wrong_version_name),
            Err(RegistryError::InvalidMetadata(_))
        ));

        let mut wrong_version = valid;
        wrong_version.versions.get_mut("1.0.0").unwrap().version = Some("2.0.0".into());
        assert!(matches!(
            validate_packument_identity(&package, &wrong_version),
            Err(RegistryError::InvalidMetadata(_))
        ));
    }
}
