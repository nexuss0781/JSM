use std::{fmt::Write as _, path::PathBuf, time::SystemTime};

use clap::{Parser, Subcommand};
use miette::{IntoDiagnostic, Result};
use tracing::{
    Event, Subscriber, debug,
    field::{Field, Visit},
    info, info_span,
};
use tracing_subscriber::{
    EnvFilter,
    fmt::{
        FmtContext,
        format::{FormatEvent, FormatFields, Writer},
    },
    layer::{Layer, SubscriberExt},
    registry::LookupSpan,
    util::SubscriberInitExt,
};

use jsm_core::redact;

#[derive(Debug, Parser)]
#[command(
    name = "jsm",
    version,
    about = "Rust JavaScript package manager — Phase 0 foundations"
)]
struct Cli {
    /// Enable debug-level diagnostic output.
    #[arg(long, conflicts_with = "quiet")]
    verbose: bool,
    /// Suppress informational and debug-level logs.
    #[arg(long, conflicts_with = "verbose")]
    quiet: bool,
    /// Write a Chrome-trace-compatible timeline to this file.
    #[arg(long, value_name = "FILE")]
    trace: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Emit a representative resolve/fetch/extract/write/link/build trace (Phase 0 only).
    #[command(name = "phase0-demo")]
    Phase0Demo,
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
    let filter = filter_for(&cli);
    let formatting = tracing_subscriber::fmt::layer()
        .event_format(RedactingEventFormat)
        .with_writer(std::io::stderr)
        .with_filter(filter);

    // Chrome traces contain stage names and timings, not arbitrary field values.
    // The formatter redacts all displayed event values at the logging boundary.
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

    match cli.command {
        Some(Command::Phase0Demo) => phase0_demo(),
        None => {
            println!("JSM Phase 0 foundation build. Run `jsm --help` for available commands.");
            Ok(())
        }
    }?;
    drop(trace_guard);
    Ok(())
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
