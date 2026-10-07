//! Deterministic dependency resolver for registry packages.
//!
//! Resolution is performed per dependency edge, with package instances keyed by
//! `name@version`. This permits nested versions when two parts of a graph require
//! incompatible ranges while keeping a deterministic highest-version preference.

use std::{cell::RefCell, cmp::Reverse, collections::BTreeMap, fmt};

use jsm_core::{DependencySpec, DistTag, PackageName, Spec, Version};
use pubgrub::{
    Dependencies, DependencyProvider as PubGrubDependencyProvider, PackageResolutionStatistics,
    Ranges, resolve as pubgrub_resolve,
};
use thiserror::Error;

/// Crate identifier used by workspace-boundary checks.
pub const CRATE_NAME: &str = "jsm-resolver";

/// Metadata for one publishable package version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub version: Version,
    pub dependencies: BTreeMap<PackageName, Spec>,
    pub optional_dependencies: BTreeMap<PackageName, Spec>,
    /// Dist-tags are repeated on candidates because registries return them with metadata.
    pub dist_tags: BTreeMap<DistTag, Version>,
    /// A deprecation message. Deprecated versions remain usable but are a soft preference.
    pub deprecated: Option<String>,
}

impl Candidate {
    pub fn new(version: Version) -> Self {
        Self {
            version,
            dependencies: BTreeMap::new(),
            optional_dependencies: BTreeMap::new(),
            dist_tags: BTreeMap::new(),
            deprecated: None,
        }
    }
}

fn sort_candidates(candidates: &mut [Candidate]) {
    candidates.sort_by(|a, b| {
        a.deprecated
            .is_some()
            .cmp(&b.deprecated.is_some())
            .then_with(|| b.version.cmp(&a.version))
            .then_with(|| format!("{:?}", a).cmp(&format!("{:?}", b)))
    });
}

/// Provider boundary. Implementations may fetch lazily; the resolver never depends on
/// response order or timing.
pub trait Provider {
    type Error: fmt::Display;
    fn candidates(&self, package: &PackageName) -> Result<Vec<Candidate>, Self::Error>;

    /// Fetch several distinct packuments concurrently while preserving input order.
    /// The default has an eight-worker cap; providers may override it when they have
    /// a transport-native batch implementation.
    fn candidates_many(&self, packages: &[PackageName]) -> Vec<Result<Vec<Candidate>, Self::Error>>
    where
        Self: Sync,
        Self::Error: Send,
    {
        if packages.len() <= 1 {
            return packages
                .iter()
                .map(|package| self.candidates(package))
                .collect();
        }
        let worker_count = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(4)
            .clamp(2, 8)
            .min(packages.len());
        let mut results = Vec::with_capacity(packages.len());
        std::thread::scope(|scope| {
            let handles = (0..worker_count)
                .map(|worker| {
                    scope.spawn(move || {
                        (worker..packages.len())
                            .step_by(worker_count)
                            .map(|index| (index, self.candidates(&packages[index])))
                            .collect::<Vec<_>>()
                    })
                })
                .collect::<Vec<_>>();
            for handle in handles {
                results.extend(handle.join().expect("candidate metadata worker panicked"));
            }
        });
        results.sort_by_key(|(index, _)| *index);
        results.into_iter().map(|(_, result)| result).collect()
    }

    /// Return true only when this error means the package itself does not exist.
    fn is_missing_package(&self, _error: &Self::Error) -> bool {
        false
    }
}

#[derive(Debug)]
enum LazyProviderError<E> {
    Provider { package: PackageName, source: E },
    Internal(String),
}

impl<E: fmt::Display> fmt::Display for LazyProviderError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { package, source } => {
                write!(formatter, "provider failed for {package}: {source}")
            }
            Self::Internal(message) => formatter.write_str(message),
        }
    }
}

impl<E: fmt::Debug + fmt::Display + 'static> std::error::Error for LazyProviderError<E> {}

#[derive(Clone)]
struct LazyNode {
    package: PackageName,
    /// Ancestor nodes exclude this node, matching the resolver's cycle behavior.
    ancestors: Vec<String>,
}

struct LazyProviderAdapter<'resolver, 'cache, P>
where
    P: Provider + Sync,
    P::Error: fmt::Debug + fmt::Display + Send + 'static,
{
    resolver: &'resolver Resolver<P>,
    cache: RefCell<&'cache mut BTreeMap<PackageName, Vec<Candidate>>>,
    nodes: RefCell<BTreeMap<String, LazyNode>>,
    edges: RefCell<BTreeMap<(String, Version), BTreeMap<PackageName, String>>>,
    root_node: String,
    root_range: Ranges<Version>,
}

impl<'resolver, 'cache, P> LazyProviderAdapter<'resolver, 'cache, P>
where
    P: Provider + Sync,
    P::Error: fmt::Debug + fmt::Display + Send + 'static,
{
    fn new(
        resolver: &'resolver Resolver<P>,
        cache: &'cache mut BTreeMap<PackageName, Vec<Candidate>>,
        root_node: String,
        root_package: PackageName,
        root_range: Ranges<Version>,
    ) -> Self {
        Self {
            resolver,
            cache: RefCell::new(cache),
            nodes: RefCell::new(BTreeMap::from([(
                root_node.clone(),
                LazyNode {
                    package: root_package,
                    ancestors: Vec::new(),
                },
            )])),
            edges: RefCell::new(BTreeMap::new()),
            root_node,
            root_range,
        }
    }

    fn candidates_for(
        &self,
        package: &PackageName,
    ) -> Result<Vec<Candidate>, LazyProviderError<P::Error>> {
        if let Some(candidates) = self.cache.borrow().get(package).cloned() {
            return Ok(candidates);
        }
        let result = {
            let mut cache = self.cache.borrow_mut();
            self.resolver.candidates(package, &mut cache)
        };
        match result {
            Ok(candidates) => Ok(candidates),
            Err(ResolveError::Provider { source, .. })
                if self.resolver.provider.is_missing_package(&source) =>
            {
                self.cache.borrow_mut().insert(package.clone(), Vec::new());
                Ok(Vec::new())
            }
            Err(ResolveError::Provider { source, .. }) => Err(LazyProviderError::Provider {
                package: package.clone(),
                source,
            }),
            Err(error) => Err(LazyProviderError::Internal(error.to_string())),
        }
    }

    fn candidates_for_many(
        &self,
        packages: &[PackageName],
    ) -> Result<(), LazyProviderError<P::Error>> {
        let cached = self.cache.borrow();
        let missing = packages
            .iter()
            .filter(|package| !cached.contains_key(*package))
            .cloned()
            .collect::<Vec<_>>();
        drop(cached);
        if missing.is_empty() {
            return Ok(());
        }

        let fetched = self.resolver.provider.candidates_many(&missing);
        if fetched.len() != missing.len() {
            return Err(LazyProviderError::Internal(format!(
                "provider returned {} results for {} metadata requests",
                fetched.len(),
                missing.len()
            )));
        }
        let mut cache = self.cache.borrow_mut();
        for (package, result) in missing.into_iter().zip(fetched) {
            match result {
                Ok(mut candidates) => {
                    sort_candidates(&mut candidates);
                    cache.insert(package, candidates);
                }
                Err(source) if self.resolver.provider.is_missing_package(&source) => {
                    cache.insert(package, Vec::new());
                }
                Err(source) => {
                    return Err(LazyProviderError::Provider { package, source });
                }
            }
        }
        Ok(())
    }

    fn package_for_node(&self, node: &str) -> Result<PackageName, LazyProviderError<P::Error>> {
        self.nodes
            .borrow()
            .get(node)
            .map(|node| node.package.clone())
            .ok_or_else(|| LazyProviderError::Internal(format!("unknown resolver node `{node}`")))
    }

    fn child_node(
        &self,
        parent: &str,
        dependency: &PackageName,
    ) -> Result<String, LazyProviderError<P::Error>> {
        let mut nodes = self.nodes.borrow_mut();
        let parent_node = nodes.get(parent).cloned().ok_or_else(|| {
            LazyProviderError::Internal(format!("unknown resolver node `{parent}`"))
        })?;
        if let Some(ancestor) = parent_node.ancestors.iter().rev().find(|ancestor| {
            nodes
                .get(*ancestor)
                .is_some_and(|node| node.package == *dependency)
        }) {
            return Ok(ancestor.clone());
        }
        let child = format!("{parent}/{dependency}");
        let mut ancestors = parent_node.ancestors;
        ancestors.push(parent.to_owned());
        nodes.entry(child.clone()).or_insert_with(|| LazyNode {
            package: dependency.clone(),
            ancestors,
        });
        Ok(child)
    }

    fn range_for(
        &self,
        package: &PackageName,
        spec: &Spec,
    ) -> Result<Ranges<Version>, LazyProviderError<P::Error>> {
        let mut range = Ranges::empty();
        for candidate in self.candidates_for(package)? {
            if matches_spec(spec, &candidate, &candidate.version) {
                range = range.union(&Ranges::singleton(candidate.version));
            }
        }
        Ok(range)
    }
}

impl<'resolver, 'cache, P> PubGrubDependencyProvider for LazyProviderAdapter<'resolver, 'cache, P>
where
    P: Provider + Sync,
    P::Error: fmt::Debug + fmt::Display + Send + 'static,
{
    type P = String;
    type V = Version;
    type VS = Ranges<Version>;
    type Priority = (u32, Reverse<usize>);
    type M = String;
    type Err = LazyProviderError<P::Error>;

    fn prioritize(
        &self,
        package: &Self::P,
        _range: &Self::VS,
        package_statistics: &PackageResolutionStatistics,
    ) -> Self::Priority {
        let package_name = self
            .nodes
            .borrow()
            .get(package)
            .map(|node| node.package.clone());
        let version_count = package_name
            .and_then(|name| self.cache.borrow().get(&name).map(Vec::len))
            .unwrap_or_default();
        (package_statistics.conflict_count(), Reverse(version_count))
    }

    fn choose_version(
        &self,
        package: &Self::P,
        range: &Self::VS,
    ) -> Result<Option<Self::V>, Self::Err> {
        if package == "__root" {
            let root_version = Version::parse("0.0.0").expect("static PubGrub root version");
            return Ok(range.contains(&root_version).then_some(root_version));
        }
        let package_name = self.package_for_node(package)?;
        let candidates = self.candidates_for(&package_name)?;
        let matches_range = |candidate: &&Candidate| range.contains(&candidate.version);
        let preferred = candidates
            .iter()
            .filter(matches_range)
            .filter(|candidate| candidate.deprecated.is_none())
            .max_by(|left, right| left.version.cmp(&right.version))
            .or_else(|| {
                candidates
                    .iter()
                    .filter(|candidate| range.contains(&candidate.version))
                    .max_by(|left, right| left.version.cmp(&right.version))
            })
            .map(|candidate| candidate.version.clone());
        Ok(preferred)
    }

    fn get_dependencies(
        &self,
        package: &Self::P,
        version: &Self::V,
    ) -> Result<Dependencies<Self::P, Self::VS, Self::M>, Self::Err> {
        if package == "__root" {
            return Ok(Dependencies::Available(
                [(self.root_node.clone(), self.root_range.clone())]
                    .into_iter()
                    .collect(),
            ));
        }
        let package_name = self.package_for_node(package)?;
        let candidates = self.candidates_for(&package_name)?;
        let Some(candidate) = candidates
            .into_iter()
            .find(|candidate| candidate.version == *version)
        else {
            return Ok(Dependencies::Unavailable(format!(
                "candidate {package_name}@{version} is unavailable"
            )));
        };
        let dependency_names = candidate.dependencies.keys().cloned().collect::<Vec<_>>();
        self.candidates_for_many(&dependency_names)?;
        let mut dependencies = Vec::new();
        let mut edges = BTreeMap::new();
        for (dependency, spec) in candidate.dependencies {
            let child = self.child_node(package, &dependency)?;
            let range = self.range_for(&dependency, &spec)?;
            dependencies.push((child.clone(), range));
            edges.insert(dependency, child);
        }
        self.edges
            .borrow_mut()
            .insert((package.clone(), version.clone()), edges);
        Ok(Dependencies::Available(dependencies.into_iter().collect()))
    }
}

/// One selected package instance and its exact child edges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPackage {
    pub name: PackageName,
    pub version: Version,
    pub dependencies: BTreeMap<PackageName, Spec>,
    pub optional_dependencies: BTreeMap<PackageName, Spec>,
    pub resolved_dependencies: BTreeMap<PackageName, String>,
    pub resolved_optional_dependencies: BTreeMap<PackageName, String>,
    pub dist_tags: BTreeMap<DistTag, Version>,
}

/// Deterministic graph keyed by exact `name@version` package identity.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Resolution {
    pub root_dependencies: BTreeMap<PackageName, String>,
    pub packages: BTreeMap<String, ResolvedPackage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub package: PackageName,
    pub constraints: Vec<String>,
    pub available_versions: Vec<Version>,
    pub reason: String,
}

#[derive(Debug, Error)]
pub enum ResolveError<E: fmt::Display> {
    #[error("provider failed for {package}: {source}")]
    Provider { package: PackageName, source: E },
    #[error(
        "provider failed for dependency {dependency} of {parent}@{version} on path `{path}`: {source}"
    )]
    DependencyProvider {
        parent: PackageName,
        version: Version,
        dependency: PackageName,
        path: String,
        source: E,
    },
    #[error("resolution conflict for {0:?}")]
    Conflict(Conflict),
}

/// Resolver using highest-version-first, with non-deprecated versions preferred.
pub struct Resolver<P> {
    provider: P,
}

impl<P> Resolver<P> {
    pub fn new(provider: P) -> Self {
        Self { provider }
    }

    pub fn resolve<E>(&self, roots: &[DependencySpec]) -> Result<Resolution, ResolveError<E>>
    where
        P: Provider<Error = E> + Sync,
        E: fmt::Debug + fmt::Display + Send + 'static,
    {
        self.resolve_with_optional(roots, &[])
    }

    pub fn resolve_with_optional<E>(
        &self,
        roots: &[DependencySpec],
        optional_roots: &[DependencySpec],
    ) -> Result<Resolution, ResolveError<E>>
    where
        P: Provider<Error = E> + Sync,
        E: fmt::Debug + fmt::Display + Send + 'static,
    {
        let mut cache = BTreeMap::<PackageName, Vec<Candidate>>::new();
        let mut resolution = Resolution::default();
        let mut root_specs = BTreeMap::<PackageName, String>::new();
        for root in roots {
            let (id, packages) = self.solve_branch(root.name(), root.spec(), &mut cache)?;
            if let Some(previous) = resolution.root_dependencies.get(root.name())
                && previous != &id
            {
                return Err(ResolveError::Conflict(Conflict {
                    package: root.name().clone(),
                    constraints: vec![
                        root_specs.get(root.name()).cloned().unwrap_or_default(),
                        format!("root {} requires {}", root.name(), spec_text(root.spec())),
                    ],
                    available_versions: cache
                        .get(root.name())
                        .into_iter()
                        .flatten()
                        .map(|c| c.version.clone())
                        .collect(),
                    reason: "root dependency sections select incompatible package instances".into(),
                }));
            }
            root_specs.insert(
                root.name().clone(),
                format!("root {} requires {}", root.name(), spec_text(root.spec())),
            );
            resolution.root_dependencies.insert(root.name().clone(), id);
            resolution.packages.extend(packages);
        }

        let mut optional_queue = optional_roots
            .iter()
            .cloned()
            .map(|dependency| (None, dependency))
            .collect::<Vec<_>>();
        for (id, package) in &resolution.packages {
            optional_queue.extend(package.optional_dependencies.iter().map(|(name, spec)| {
                (
                    Some(id.clone()),
                    DependencySpec::new(name.clone(), spec.clone()),
                )
            }));
        }
        let mut attempted = std::collections::BTreeSet::<(Option<String>, String)>::new();
        while let Some((parent, dependency)) = optional_queue.pop() {
            let key = (parent.clone(), dependency.name().as_str().to_owned());
            if !attempted.insert(key) {
                continue;
            }
            if parent
                .as_ref()
                .is_some_and(|parent_id| !resolution.packages.contains_key(parent_id))
                || (parent.is_none()
                    && resolution.root_dependencies.contains_key(dependency.name()))
            {
                continue;
            }
            let (id, packages) =
                match self.solve_branch(dependency.name(), dependency.spec(), &mut cache) {
                    Ok(value) => value,
                    Err(_) => continue,
                };
            if let Some(parent_id) = parent {
                if let Some(package) = resolution.packages.get_mut(&parent_id) {
                    package
                        .resolved_optional_dependencies
                        .insert(dependency.name().clone(), id.clone());
                }
            } else {
                resolution
                    .root_dependencies
                    .insert(dependency.name().clone(), id.clone());
            }
            let new_ids = packages
                .keys()
                .filter(|package_id| !resolution.packages.contains_key(*package_id))
                .cloned()
                .collect::<Vec<_>>();
            for (package_id, package) in packages {
                resolution.packages.entry(package_id).or_insert(package);
            }
            for package_id in new_ids {
                if let Some(package) = resolution.packages.get(&package_id) {
                    optional_queue.extend(package.optional_dependencies.iter().map(
                        |(name, spec)| {
                            (
                                Some(package_id.clone()),
                                DependencySpec::new(name.clone(), spec.clone()),
                            )
                        },
                    ));
                }
            }
        }
        Ok(resolution)
    }

    pub fn resolve_map<E>(
        &self,
        roots: &BTreeMap<PackageName, Spec>,
    ) -> Result<Resolution, ResolveError<E>>
    where
        P: Provider<Error = E> + Sync,
        E: fmt::Debug + fmt::Display + Send + 'static,
    {
        self.resolve(
            &roots
                .iter()
                .map(|(n, s)| DependencySpec::new(n.clone(), s.clone()))
                .collect::<Vec<_>>(),
        )
    }

    fn candidates<E>(
        &self,
        name: &PackageName,
        cache: &mut BTreeMap<PackageName, Vec<Candidate>>,
    ) -> Result<Vec<Candidate>, ResolveError<E>>
    where
        P: Provider<Error = E>,
        E: fmt::Display,
    {
        if !cache.contains_key(name) {
            let mut cs =
                self.provider
                    .candidates(name)
                    .map_err(|source| ResolveError::Provider {
                        package: name.clone(),
                        source,
                    })?;
            cs.sort_by(|a, b| {
                a.deprecated
                    .is_some()
                    .cmp(&b.deprecated.is_some())
                    .then_with(|| b.version.cmp(&a.version))
                    .then_with(|| format!("{:?}", a).cmp(&format!("{:?}", b)))
            });
            cache.insert(name.clone(), cs);
        }
        Ok(cache[name].clone())
    }

    fn solve_branch<E>(
        &self,
        name: &PackageName,
        spec: &Spec,
        cache: &mut BTreeMap<PackageName, Vec<Candidate>>,
    ) -> Result<(String, BTreeMap<String, ResolvedPackage>), ResolveError<E>>
    where
        P: Provider<Error = E> + Sync,
        E: fmt::Debug + fmt::Display + Send + 'static,
    {
        let root_version = Version::parse("0.0.0").unwrap();
        let root_node = format!("branch/{name}");
        // Load only the root packument up front. The PubGrub adapter fetches child
        // metadata when a candidate version is selected, avoiding expansion of
        // every version's full dependency tree.
        self.candidates(name, cache)?;
        let root_range = self.pub_range(name, spec, cache, false)?;
        let adapter =
            LazyProviderAdapter::new(self, cache, root_node.clone(), name.clone(), root_range);
        let selected_result = pubgrub_resolve(&adapter, "__root".to_owned(), root_version.clone());
        let cache_snapshot = (**adapter.cache.borrow()).clone();
        let nodes = adapter.nodes.borrow().clone();
        let edges = adapter.edges.borrow().clone();
        drop(adapter);

        let available_versions = cache_snapshot
            .get(name)
            .into_iter()
            .flatten()
            .map(|candidate| candidate.version.clone())
            .collect::<Vec<_>>();
        let selected = match selected_result {
            Ok(selected) => selected,
            Err(pubgrub::PubGrubError::NoSolution(derivation)) => {
                let culprit = cache_snapshot
                    .iter()
                    .find_map(|(_package, candidates)| {
                        candidates
                            .iter()
                            .flat_map(|candidate| {
                                candidate
                                    .dependencies
                                    .iter()
                                    .chain(candidate.optional_dependencies.iter())
                            })
                            .find(|(dependency, spec)| {
                                cache_snapshot.get(*dependency).is_none_or(|available| {
                                    !available.iter().any(|candidate| {
                                        matches_spec(spec, candidate, &candidate.version)
                                    })
                                })
                            })
                            .map(|(dependency, _)| dependency.clone())
                    })
                    .unwrap_or_else(|| name.clone());
                return Err(ResolveError::Conflict(Conflict {
                    package: culprit,
                    constraints: vec![format!("root {name} requires {}", spec_text(spec))],
                    available_versions,
                    reason: format!("PubGrub derivation: {derivation:?}"),
                }));
            }
            Err(pubgrub::PubGrubError::ErrorChoosingVersion {
                source: LazyProviderError::Provider { package, source },
                ..
            }) => return Err(ResolveError::Provider { package, source }),
            Err(pubgrub::PubGrubError::ErrorRetrievingDependencies {
                package: parent_node,
                version,
                source:
                    LazyProviderError::Provider {
                        package: dependency,
                        source,
                    },
            }) => {
                let parent = nodes
                    .get(&parent_node)
                    .map(|node| node.package.clone())
                    .unwrap_or_else(|| name.clone());
                return Err(ResolveError::DependencyProvider {
                    parent,
                    version,
                    dependency,
                    path: parent_node,
                    source,
                });
            }
            Err(error) => {
                return Err(ResolveError::Conflict(Conflict {
                    package: name.clone(),
                    constraints: vec![format!("root {name} requires {}", spec_text(spec))],
                    available_versions,
                    reason: format!("PubGrub provider error: {error:?}"),
                }));
            }
        };
        let mut packages = BTreeMap::new();
        for (node, version) in selected.iter() {
            if node == "__root" {
                continue;
            }
            let Some(node_info) = nodes.get(node) else {
                return Err(ResolveError::Conflict(Conflict {
                    package: name.clone(),
                    constraints: vec![spec_text(spec)],
                    available_versions: available_versions.clone(),
                    reason: format!("PubGrub selected unknown resolver node `{node}`"),
                }));
            };
            let package = &node_info.package;
            let candidate = cache_snapshot[package]
                .iter()
                .find(|c| c.version == *version)
                .ok_or_else(|| {
                    ResolveError::Conflict(Conflict {
                        package: package.clone(),
                        constraints: vec![spec_text(spec)],
                        available_versions: available_versions.clone(),
                        reason: format!(
                            "PubGrub selected unavailable package instance {package}@{version}"
                        ),
                    })
                })?;
            let id = package_id(package, version);
            packages.entry(id).or_insert(ResolvedPackage {
                name: package.clone(),
                version: version.clone(),
                dependencies: candidate.dependencies.clone(),
                optional_dependencies: candidate.optional_dependencies.clone(),
                resolved_dependencies: BTreeMap::new(),
                resolved_optional_dependencies: BTreeMap::new(),
                dist_tags: candidate.dist_tags.clone(),
            });
        }
        for (node, version) in selected.iter() {
            if node == "__root" {
                continue;
            }
            let node_info = &nodes[node];
            let package = &node_info.package;
            let id = package_id(package, version);
            let candidate = cache_snapshot[package]
                .iter()
                .find(|c| c.version == *version)
                .unwrap();
            let pkg = packages.get_mut(&id).unwrap();
            if let Some(node_edges) = edges.get(&(node.clone(), version.clone())) {
                for dep in candidate.dependencies.keys() {
                    let Some(child) = node_edges.get(dep) else {
                        continue;
                    };
                    let Some(child_node) = nodes.get(child) else {
                        continue;
                    };
                    if let Some(cv) = selected.get(child) {
                        pkg.resolved_dependencies
                            .insert(dep.clone(), package_id(&child_node.package, cv));
                    }
                }
            }
        }
        let id = selected
            .get(&root_node)
            .map(|v| package_id(name, v))
            .ok_or_else(|| {
                ResolveError::Conflict(Conflict {
                    package: name.clone(),
                    constraints: vec![spec_text(spec)],
                    available_versions: vec![],
                    reason: "PubGrub returned no root package".into(),
                })
            })?;
        Ok((id, packages))
    }

    fn pub_range<E>(
        &self,
        name: &PackageName,
        spec: &Spec,
        cache: &mut BTreeMap<PackageName, Vec<Candidate>>,
        missing_child_is_empty: bool,
    ) -> Result<Ranges<Version>, ResolveError<E>>
    where
        P: Provider<Error = E>,
        E: fmt::Display,
    {
        let mut out = Ranges::empty();
        let candidates = match self.candidates(name, cache) {
            Ok(candidates) => candidates,
            Err(ResolveError::Provider { source, .. })
                if missing_child_is_empty && self.provider.is_missing_package(&source) =>
            {
                cache.insert(name.clone(), Vec::new());
                return Ok(out);
            }
            Err(error) => return Err(error),
        };
        for c in candidates {
            if matches_spec(spec, &c, &c.version) {
                out = out.union(&Ranges::singleton(c.version));
            }
        }
        Ok(out)
    }
}

fn package_id(name: &PackageName, version: &Version) -> String {
    format!("{name}@{version}")
}
fn spec_text(spec: &Spec) -> String {
    match spec {
        Spec::Registry(r) => r.to_string(),
        Spec::Tag(t) => t.to_string(),
        other => format!("{other:?}"),
    }
}
fn matches_spec(spec: &Spec, candidate: &Candidate, version: &Version) -> bool {
    match spec {
        Spec::Registry(r) => r.matches(version),
        Spec::Tag(t) => candidate.dist_tags.get(t).is_some_and(|v| v == version),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsm_core::Range;
    use proptest::prop_assert;
    use std::{
        collections::BTreeMap,
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    #[derive(Default)]
    struct Fake(BTreeMap<String, Vec<Candidate>>);
    impl Provider for Fake {
        type Error = String;
        fn candidates(&self, n: &PackageName) -> Result<Vec<Candidate>, String> {
            Ok(self.0.get(n.as_str()).cloned().unwrap_or_default())
        }
    }
    fn n(s: &str) -> PackageName {
        PackageName::new(s).unwrap()
    }
    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }
    fn r(s: &str) -> Spec {
        Spec::Registry(Range::new(s).unwrap())
    }
    fn c(ver: &str, deps: &[(&str, Spec)]) -> Candidate {
        let mut candidate = Candidate::new(v(ver));
        candidate.dependencies = deps
            .iter()
            .map(|(name, spec)| (PackageName::new(*name).unwrap(), spec.clone()))
            .collect();
        candidate
    }
    fn resolver(cases: &[(&str, Vec<Candidate>)]) -> Resolver<Fake> {
        Resolver::new(Fake(
            cases
                .iter()
                .map(|(name, candidates)| ((*name).into(), candidates.clone()))
                .collect(),
        ))
    }
    fn root(name: &str, spec: Spec) -> DependencySpec {
        DependencySpec::new(n(name), spec)
    }

    struct ParallelFake {
        active: AtomicUsize,
        max_active: AtomicUsize,
    }

    impl Provider for ParallelFake {
        type Error = std::convert::Infallible;

        fn candidates(&self, package: &PackageName) -> Result<Vec<Candidate>, Self::Error> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(25));
            self.active.fetch_sub(1, Ordering::SeqCst);
            let index = package
                .as_str()
                .strip_prefix("parallel-")
                .expect("test package prefix")
                .parse::<u64>()
                .expect("test package index");
            Ok(vec![Candidate::new(v(&format!("1.0.{index}")))])
        }
    }

    #[test]
    fn candidate_batch_fetches_concurrently_with_stable_result_order() {
        let provider = ParallelFake {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        };
        let packages = (0..8)
            .map(|index| n(&format!("parallel-{index}")))
            .collect::<Vec<_>>();
        let results = provider.candidates_many(&packages);

        assert_eq!(results.len(), packages.len());
        assert!(provider.max_active.load(Ordering::SeqCst) > 1);
        assert!(provider.max_active.load(Ordering::SeqCst) <= 8);
        for (index, result) in results.into_iter().enumerate() {
            assert_eq!(result.unwrap()[0].version, v(&format!("1.0.{index}")));
        }
    }

    struct ErroringFake(BTreeMap<String, Vec<Candidate>>);
    impl Provider for ErroringFake {
        type Error = String;
        fn candidates(&self, package: &PackageName) -> Result<Vec<Candidate>, String> {
            if package.as_str() == "unavailable-optional" {
                Err("registry returned 404".into())
            } else if package.as_str() == "network-unavailable" {
                Err("connection reset".into())
            } else {
                Ok(self.0.get(package.as_str()).cloned().unwrap_or_default())
            }
        }

        fn is_missing_package(&self, error: &String) -> bool {
            error == "registry returned 404"
        }
    }

    #[test]
    fn unavailable_optional_dependencies_are_skipped_but_available_ones_are_resolved() {
        let mut root_candidate = c("1.0.0", &[]);
        root_candidate
            .optional_dependencies
            .insert(n("available-optional"), r("^1"));
        root_candidate
            .optional_dependencies
            .insert(n("missing-optional"), r("*"));
        let resolution = resolver(&[
            ("root", vec![root_candidate]),
            ("available-optional", vec![c("1.4.0", &[])]),
        ])
        .resolve(&[root("root", r("*"))])
        .unwrap();

        let resolved_root = &resolution.packages["root@1.0.0"];
        assert_eq!(
            resolved_root.resolved_optional_dependencies[&n("available-optional")],
            "available-optional@1.4.0"
        );
        assert!(
            !resolved_root
                .resolved_optional_dependencies
                .contains_key(&n("missing-optional"))
        );
        assert!(!resolution.packages.contains_key("missing-optional@1.0.0"));
    }

    #[test]
    fn provider_errors_for_optional_roots_do_not_fail_required_resolution() {
        let provider = ErroringFake(BTreeMap::from([("root".into(), vec![c("1.0.0", &[])])]));
        let resolution = Resolver::new(provider)
            .resolve_with_optional::<String>(
                &[root("root", r("*"))],
                &[root("unavailable-optional", r("*"))],
            )
            .unwrap();
        assert!(resolution.root_dependencies.contains_key(&n("root")));
        assert!(
            !resolution
                .root_dependencies
                .contains_key(&n("unavailable-optional"))
        );
    }

    #[test]
    fn required_provider_error_identifies_the_declaring_package_and_version() {
        let provider = ErroringFake(BTreeMap::from([(
            "root".into(),
            vec![c("1.0.0", &[("network-unavailable", r("*"))])],
        )]));
        let error = Resolver::new(provider)
            .resolve::<String>(&[root("root", r("*"))])
            .unwrap_err()
            .to_string();
        assert!(error.contains("dependency network-unavailable of root@1.0.0"));
        assert!(error.contains("branch/root"));
    }

    #[test]
    fn missing_transitive_package_invalidates_only_the_candidate_that_requires_it() {
        let provider = ErroringFake(BTreeMap::from([(
            "root".into(),
            vec![
                c("2.0.0", &[("unavailable-optional", r("*"))]),
                c("1.0.0", &[]),
            ],
        )]));
        let resolution = Resolver::new(provider)
            .resolve::<String>(&[root("root", r("*"))])
            .unwrap();
        assert_eq!(resolution.root_dependencies[&n("root")], "root@1.0.0");
    }

    #[test]
    fn transitive_dependencies_choose_highest_compatible_version() {
        let resolution = resolver(&[
            ("a", vec![c("1.0.0", &[("b", r("^1"))])]),
            ("b", vec![c("1.5.0", &[]), c("1.2.0", &[])]),
        ])
        .resolve(&[root("a", r("*"))])
        .unwrap();
        assert_eq!(resolution.packages["b@1.5.0"].version, v("1.5.0"));
    }

    #[test]
    fn deprecated_versions_are_softly_deprioritized() {
        let mut deprecated = c("2.0.0", &[]);
        deprecated.deprecated = Some("use the maintained release".into());
        let resolution = resolver(&[("a", vec![deprecated.clone(), c("1.9.0", &[])])])
            .resolve(&[root("a", r("*"))])
            .unwrap();
        assert_eq!(resolution.root_dependencies[&n("a")], "a@1.9.0");

        let exact = resolver(&[("a", vec![deprecated])])
            .resolve(&[root("a", r("=2.0.0"))])
            .unwrap();
        assert_eq!(exact.root_dependencies[&n("a")], "a@2.0.0");
    }

    #[test]
    fn incompatible_ranges_get_distinct_nested_package_instances() {
        let resolution = resolver(&[
            ("a", vec![c("1.0.0", &[("b", r("^1"))])]),
            ("c", vec![c("1.0.0", &[("b", r("^2"))])]),
            ("b", vec![c("2.4.0", &[]), c("1.8.0", &[])]),
        ])
        .resolve(&[root("a", r("*")), root("c", r("*"))])
        .unwrap();
        assert_eq!(resolution.packages["b@1.8.0"].version, v("1.8.0"));
        assert_eq!(resolution.packages["b@2.4.0"].version, v("2.4.0"));
        assert_ne!(
            resolution.packages["a@1.0.0"].resolved_dependencies[&n("b")],
            resolution.packages["c@1.0.0"].resolved_dependencies[&n("b")]
        );
    }

    #[test]
    fn cycles_are_safe_and_keep_exact_package_ids() {
        let resolution = resolver(&[
            ("a", vec![c("1.0.0", &[("b", r("1"))])]),
            ("b", vec![c("1.0.0", &[("a", r("1"))])]),
        ])
        .resolve(&[root("a", r("1"))])
        .unwrap();
        assert_eq!(
            resolution.packages["a@1.0.0"].resolved_dependencies[&n("b")],
            "b@1.0.0"
        );
        assert_eq!(
            resolution.packages["b@1.0.0"].resolved_dependencies[&n("a")],
            "a@1.0.0"
        );
    }

    #[test]
    fn conflict_is_structured() {
        let error = resolver(&[
            ("a", vec![c("1.0.0", &[("b", r("1"))])]),
            ("b", vec![c("2.0.0", &[])]),
        ])
        .resolve(&[root("a", r("1"))])
        .unwrap_err();
        match error {
            ResolveError::Conflict(conflict) => assert_eq!(conflict.package, n("b")),
            _ => panic!(),
        }
    }

    #[test]
    fn tags_and_prereleases() {
        let mut stable = c("1.0.0", &[]);
        stable
            .dist_tags
            .insert(DistTag::new("latest").unwrap(), v("1.0.0"));
        let mut pre = c("2.0.0-alpha.1", &[]);
        pre.dist_tags
            .insert(DistTag::new("next").unwrap(), v("2.0.0-alpha.1"));
        let resolution = resolver(&[("a", vec![stable, pre])])
            .resolve(&[root("a", Spec::Tag(DistTag::new("next").unwrap()))])
            .unwrap();
        assert_eq!(resolution.root_dependencies[&n("a")], "a@2.0.0-alpha.1");
        assert!(
            resolver(&[("a", vec![c("2.0.0-alpha.1", &[])])])
                .resolve(&[root("a", r("^2"))])
                .is_err()
        );
    }

    #[test]
    fn deterministic_order_and_selection() {
        let resolver = resolver(&[("a", vec![c("1.0.0", &[]), c("2.0.0", &[])])]);
        let roots = vec![root("a", r("*"))];
        assert_eq!(
            resolver.resolve(&roots).unwrap(),
            resolver.resolve(&roots).unwrap()
        );
    }

    proptest::proptest! {
        #[test]
        fn selected_candidates_satisfy_root_constraint(versions in proptest::collection::vec(0u8..10, 1..8)) {
            let candidates = versions.iter().map(|minor| c(&format!("1.{minor}.0"), &[])).collect::<Vec<_>>();
            let resolution = resolver(&[("a", candidates)]).resolve(&[root("a", r("^1"))]).unwrap();
            let selected = &resolution.packages[&resolution.root_dependencies[&n("a")]].version;
            prop_assert!(Range::new("^1").unwrap().matches(selected));
            prop_assert!(selected.to_string().starts_with("1."));
        }

        #[test]
        fn every_transitive_instance_satisfies_its_declaring_parent_range(majors in proptest::collection::vec(1u8..=4, 1..8)) {
            let mut packages = Vec::<(String, Vec<Candidate>)>::new();
            let mut roots = Vec::new();
            let mut declarations = Vec::new();
            for (index, major) in majors.iter().enumerate() {
                let name = format!("parent-{index}");
                let mut parent = c("1.0.0", &[]);
                parent.dependencies.insert(n("shared"), r(&format!("^{major}")));
                packages.push((name.clone(), vec![parent]));
                roots.push(root(&name, r("*")));
                declarations.push((name, *major));
            }
            let shared = (1..=4)
                .flat_map(|major| [0, 5].map(move |minor| c(&format!("{major}.{minor}.0"), &[])))
                .collect();
            packages.push(("shared".to_owned(), shared));
            let provider = packages
                .iter()
                .map(|(name, candidates)| (name.as_str(), candidates.clone()))
                .collect::<Vec<_>>();
            let resolution = resolver(&provider).resolve(&roots).unwrap();
            for (name, major) in declarations {
                let parent = &resolution.packages[&format!("{name}@1.0.0")];
                let child_id = &parent.resolved_dependencies[&n("shared")];
                let child = &resolution.packages[child_id];
                let constraint = format!("^{}", major);
                let range = Range::new(&constraint).unwrap();
                prop_assert!(range.matches(&child.version));
            }
        }
    }

    #[test]
    fn provider_candidate_order_does_not_change_pubgrub_result() {
        let forward = resolver(&[
            ("a", vec![c("1.0.0", &[("b", r("^1"))])]),
            ("b", vec![c("1.0.0", &[]), c("1.5.0", &[])]),
        ]);
        let reverse = resolver(&[
            ("a", vec![c("1.0.0", &[("b", r("^1"))])]),
            ("b", vec![c("1.5.0", &[]), c("1.0.0", &[])]),
        ]);
        assert_eq!(
            forward.resolve(&[root("a", r("*"))]).unwrap(),
            reverse.resolve(&[root("a", r("*"))]).unwrap()
        );
    }

    #[test]
    fn conflict_explanations_are_repeatable() {
        let resolver = resolver(&[
            ("a", vec![c("1.0.0", &[("b", r("^2"))])]),
            ("b", vec![c("1.0.0", &[])]),
        ]);
        let roots = vec![root("a", r("*"))];
        let first = format!("{:?}", resolver.resolve(&roots).unwrap_err());
        let second = format!("{:?}", resolver.resolve(&roots).unwrap_err());
        assert_eq!(first, second);
    }
}
