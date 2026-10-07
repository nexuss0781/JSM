use std::{
    any::Any,
    cell::RefCell,
    collections::BTreeMap,
    fmt::{self, Display},
};

use resolvo::{
    Candidates, Condition, ConditionId, Dependencies,
    DependencyProvider as ResolvoDependencyProvider, Interner, KnownDependencies, NameId, Problem,
    SolvableId, Solver, SolverCache, StringId, UnsolvableOrCancelled, VersionSetId,
    VersionSetUnionId,
    utils::{Pool, VersionSet as ResolvoVersionSet},
};

use super::{
    Candidate, Conflict, PackageName, Provider, ResolveError, ResolvedPackage, Resolver, Spec,
    Version, matches_spec, package_id, sort_candidates, spec_text,
};

/// One immutable candidate stored in Resolvo's pool.
#[derive(Clone, Debug)]
struct SolverCandidate(Candidate);

impl Display for SolverCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0.version, formatter)
    }
}

/// The exact available versions satisfying one JSM npm spec for one package node.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SolverVersionSet {
    description: String,
    versions: Vec<String>,
}

impl Display for SolverVersionSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.description)
    }
}

impl ResolvoVersionSet for SolverVersionSet {
    type V = SolverCandidate;
}

#[derive(Clone)]
struct LazyNode {
    package: PackageName,
    path: String,
    /// Ancestor nodes exclude this node, matching the resolver's cycle behavior.
    ancestors: Vec<NameId>,
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

struct LazyProviderAdapter<'resolver, 'cache, P>
where
    P: Provider + Sync,
    P::Error: fmt::Debug + fmt::Display + Send + 'static,
{
    resolver: &'resolver Resolver<P>,
    cache: RefCell<&'cache mut BTreeMap<PackageName, Vec<Candidate>>>,
    pool: Pool<SolverVersionSet>,
    nodes: RefCell<BTreeMap<NameId, LazyNode>>,
    node_paths: RefCell<BTreeMap<String, NameId>>,
    solvables: RefCell<BTreeMap<NameId, Vec<SolvableId>>>,
    edges: RefCell<BTreeMap<(NameId, Version), BTreeMap<PackageName, NameId>>>,
    pending_error: RefCell<Option<ResolveError<P::Error>>>,
    root_node: NameId,
}

impl<'resolver, 'cache, P> LazyProviderAdapter<'resolver, 'cache, P>
where
    P: Provider + Sync,
    P::Error: fmt::Debug + fmt::Display + Send + 'static,
{
    fn new(
        resolver: &'resolver Resolver<P>,
        cache: &'cache mut BTreeMap<PackageName, Vec<Candidate>>,
        root_node_path: String,
        root_package: PackageName,
    ) -> Self {
        let pool = Pool::new();
        let root_node = pool.intern_package_name(root_node_path.clone());
        Self {
            resolver,
            cache: RefCell::new(cache),
            pool,
            nodes: RefCell::new(BTreeMap::from([(
                root_node,
                LazyNode {
                    package: root_package,
                    path: root_node_path.clone(),
                    ancestors: Vec::new(),
                },
            )])),
            node_paths: RefCell::new(BTreeMap::from([(root_node_path, root_node)])),
            solvables: RefCell::new(BTreeMap::new()),
            edges: RefCell::new(BTreeMap::new()),
            pending_error: RefCell::new(None),
            root_node,
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

    fn node(&self, name: NameId) -> Result<LazyNode, LazyProviderError<P::Error>> {
        self.nodes
            .borrow()
            .get(&name)
            .cloned()
            .ok_or_else(|| LazyProviderError::Internal(format!("unknown resolver node {name:?}")))
    }

    fn child_node(
        &self,
        parent: NameId,
        dependency: &PackageName,
    ) -> Result<NameId, LazyProviderError<P::Error>> {
        let parent_node = self.node(parent)?;
        {
            let nodes = self.nodes.borrow();
            if let Some(ancestor) = parent_node.ancestors.iter().rev().find(|ancestor| {
                nodes
                    .get(*ancestor)
                    .is_some_and(|node| node.package == *dependency)
            }) {
                return Ok(*ancestor);
            }
        }

        let path = format!("{}/{}", parent_node.path, dependency);
        if let Some(name) = self.node_paths.borrow().get(&path).copied() {
            return Ok(name);
        }
        let name = self.pool.intern_package_name(path.clone());
        let mut ancestors = parent_node.ancestors;
        ancestors.push(parent);
        self.nodes
            .borrow_mut()
            .entry(name)
            .or_insert_with(|| LazyNode {
                package: dependency.clone(),
                path: path.clone(),
                ancestors,
            });
        self.node_paths.borrow_mut().insert(path, name);
        Ok(name)
    }

    fn version_set_for(
        &self,
        name: NameId,
        spec: &Spec,
    ) -> Result<VersionSetId, LazyProviderError<P::Error>> {
        let package = self.node(name)?.package;
        let candidates = self.candidates_for(&package)?;
        let mut versions = candidates
            .iter()
            .filter(|candidate| matches_spec(spec, candidate, &candidate.version))
            .map(|candidate| candidate.version.to_string())
            .collect::<Vec<_>>();
        versions.sort();
        versions.dedup();
        Ok(self.pool.intern_version_set(
            name,
            SolverVersionSet {
                description: spec_text(spec),
                versions,
            },
        ))
    }

    fn candidate_solvables(
        &self,
        name: NameId,
    ) -> Result<Vec<SolvableId>, LazyProviderError<P::Error>> {
        if let Some(solvables) = self.solvables.borrow().get(&name).cloned() {
            return Ok(solvables);
        }
        let node = self.node(name)?;
        let candidates = self.candidates_for(&node.package)?;
        let solvables = candidates
            .into_iter()
            .map(|candidate| self.pool.intern_solvable(name, SolverCandidate(candidate)))
            .collect::<Vec<_>>();
        self.solvables.borrow_mut().insert(name, solvables.clone());
        Ok(solvables)
    }

    fn set_pending_error(&self, error: ResolveError<P::Error>) {
        let mut pending = self.pending_error.borrow_mut();
        if pending.is_none() {
            *pending = Some(error);
        }
    }

    fn take_pending_error(&self) -> Option<ResolveError<P::Error>> {
        self.pending_error.borrow_mut().take()
    }

    fn map_node_error(&self, node: &LazyNode, error: LazyProviderError<P::Error>) {
        let mapped = match error {
            LazyProviderError::Provider { package, source } => {
                ResolveError::Provider { package, source }
            }
            LazyProviderError::Internal(message) => ResolveError::Conflict(Conflict {
                package: node.package.clone(),
                constraints: Vec::new(),
                available_versions: Vec::new(),
                reason: message,
            }),
        };
        self.set_pending_error(mapped);
    }

    fn map_dependency_error(
        &self,
        parent: &LazyNode,
        candidate: &Candidate,
        error: LazyProviderError<P::Error>,
    ) -> ResolveError<P::Error> {
        match error {
            LazyProviderError::Provider { package, source } => ResolveError::DependencyProvider {
                parent: parent.package.clone(),
                version: candidate.version.clone(),
                dependency: package,
                path: parent.path.clone(),
                source,
            },
            LazyProviderError::Internal(message) => ResolveError::Conflict(Conflict {
                package: parent.package.clone(),
                constraints: Vec::new(),
                available_versions: Vec::new(),
                reason: message,
            }),
        }
    }
}

impl<P> Interner for LazyProviderAdapter<'_, '_, P>
where
    P: Provider + Sync,
    P::Error: fmt::Debug + fmt::Display + Send + 'static,
{
    type NameId = NameId;
    type SolvableId = SolvableId;

    fn display_solvable(&self, solvable: SolvableId) -> impl Display + '_ {
        let record = self.pool.resolve_solvable(solvable);
        let package = self
            .nodes
            .borrow()
            .get(&record.name)
            .map(|node| node.package.to_string())
            .unwrap_or_else(|| self.pool.resolve_package_name(record.name).clone());
        format!("{package}@{}", record.record.0.version)
    }

    fn display_name(&self, name: NameId) -> impl Display + '_ {
        self.nodes
            .borrow()
            .get(&name)
            .map(|node| node.package.to_string())
            .unwrap_or_else(|| self.pool.resolve_package_name(name).clone())
    }

    fn display_version_set(&self, version_set: VersionSetId) -> impl Display + '_ {
        self.pool
            .resolve_version_set(version_set)
            .description
            .clone()
    }

    fn display_string(&self, string_id: StringId) -> impl Display + '_ {
        self.pool.resolve_string(string_id).to_owned()
    }

    fn version_set_name(&self, version_set: VersionSetId) -> NameId {
        self.pool.resolve_version_set_package_name(version_set)
    }

    fn solvable_name(&self, solvable: SolvableId) -> NameId {
        self.pool.resolve_solvable(solvable).name
    }

    fn version_sets_in_union(
        &self,
        version_set_union: VersionSetUnionId,
    ) -> impl Iterator<Item = VersionSetId> {
        self.pool.resolve_version_set_union(version_set_union)
    }

    fn resolve_condition(&self, condition: ConditionId) -> Condition {
        self.pool.resolve_condition(condition).clone()
    }
}

impl<P> ResolvoDependencyProvider for LazyProviderAdapter<'_, '_, P>
where
    P: Provider + Sync,
    P::Error: fmt::Debug + fmt::Display + Send + 'static,
{
    async fn filter_candidates(
        &self,
        candidates: &[SolvableId],
        version_set: VersionSetId,
        inverse: bool,
    ) -> Vec<SolvableId> {
        let allowed = &self.pool.resolve_version_set(version_set).versions;
        candidates
            .iter()
            .copied()
            .filter(|solvable| {
                let version = self
                    .pool
                    .resolve_solvable(*solvable)
                    .record
                    .0
                    .version
                    .to_string();
                allowed.binary_search(&version).is_ok() != inverse
            })
            .collect()
    }

    async fn get_candidates(&self, name: NameId) -> Option<Candidates<SolvableId>> {
        let node = self.nodes.borrow().get(&name).cloned()?;
        let candidates = match self.candidate_solvables(name) {
            Ok(candidates) => candidates,
            Err(error) => {
                self.map_node_error(&node, error);
                Vec::new()
            }
        };
        Some(Candidates {
            candidates,
            ..Candidates::default()
        })
    }

    async fn sort_candidates(&self, _solver: &SolverCache<Self>, solvables: &mut [SolvableId]) {
        solvables.sort_by(|left, right| {
            let left = &self.pool.resolve_solvable(*left).record.0;
            let right = &self.pool.resolve_solvable(*right).record.0;
            left.deprecated
                .is_some()
                .cmp(&right.deprecated.is_some())
                .then_with(|| right.version.cmp(&left.version))
                .then_with(|| format!("{left:?}").cmp(&format!("{right:?}")))
        });
    }

    async fn get_dependencies(&self, solvable: SolvableId) -> Dependencies {
        let record = self.pool.resolve_solvable(solvable);
        let name = record.name;
        let candidate = record.record.0.clone();
        let parent = match self.node(name) {
            Ok(parent) => parent,
            Err(error) => {
                self.set_pending_error(ResolveError::Conflict(Conflict {
                    package: self
                        .nodes
                        .borrow()
                        .get(&self.root_node)
                        .map(|node| node.package.clone())
                        .expect("root resolver node is always present"),
                    constraints: Vec::new(),
                    available_versions: Vec::new(),
                    reason: error.to_string(),
                }));
                return Dependencies::Unknown(self.pool.intern_string(error.to_string()));
            }
        };
        let dependency_names = candidate.dependencies.keys().cloned().collect::<Vec<_>>();
        if let Err(error) = self.candidates_for_many(&dependency_names) {
            let message = error.to_string();
            let mapped = self.map_dependency_error(&parent, &candidate, error);
            self.set_pending_error(mapped);
            return Dependencies::Unknown(self.pool.intern_string(message));
        }

        let mut requirements = Vec::new();
        let mut edges = BTreeMap::new();
        for (dependency, spec) in &candidate.dependencies {
            let child = match self.child_node(name, dependency) {
                Ok(child) => child,
                Err(error) => {
                    let message = error.to_string();
                    let mapped = self.map_dependency_error(&parent, &candidate, error);
                    self.set_pending_error(mapped);
                    return Dependencies::Unknown(self.pool.intern_string(message));
                }
            };
            let version_set = match self.version_set_for(child, spec) {
                Ok(version_set) => version_set,
                Err(error) => {
                    let message = error.to_string();
                    let mapped = self.map_dependency_error(&parent, &candidate, error);
                    self.set_pending_error(mapped);
                    return Dependencies::Unknown(self.pool.intern_string(message));
                }
            };
            requirements.push(version_set.into());
            edges.insert(dependency.clone(), child);
        }
        self.edges
            .borrow_mut()
            .insert((name, candidate.version), edges);
        Dependencies::Known(KnownDependencies {
            requirements,
            constrains: Vec::new(),
        })
    }

    fn should_cancel_with_value(&self) -> Option<Box<dyn Any>> {
        self.pending_error
            .borrow()
            .is_some()
            .then(|| Box::new(()) as Box<dyn Any>)
    }
}

fn root_error<E: fmt::Display>(
    name: &PackageName,
    spec: &Spec,
    available_versions: &[Version],
    error: LazyProviderError<E>,
) -> ResolveError<E> {
    match error {
        LazyProviderError::Provider { package, source } => {
            ResolveError::Provider { package, source }
        }
        LazyProviderError::Internal(message) => ResolveError::Conflict(Conflict {
            package: name.clone(),
            constraints: vec![spec_text(spec)],
            available_versions: available_versions.to_vec(),
            reason: message,
        }),
    }
}

pub(super) fn solve_branch<P, E>(
    resolver: &Resolver<P>,
    name: &PackageName,
    spec: &Spec,
    cache: &mut BTreeMap<PackageName, Vec<Candidate>>,
) -> Result<(String, BTreeMap<String, ResolvedPackage>), ResolveError<E>>
where
    P: Provider<Error = E> + Sync,
    E: fmt::Debug + fmt::Display + Send + 'static,
{
    let root_candidates = resolver.candidates(name, cache)?;
    let available_versions = root_candidates
        .iter()
        .map(|candidate| candidate.version.clone())
        .collect::<Vec<_>>();
    let root_node_path = format!("branch/{name}");
    let adapter = LazyProviderAdapter::new(resolver, cache, root_node_path, name.clone());
    let root_node = adapter.root_node;
    let root_version_set = adapter
        .version_set_for(root_node, spec)
        .map_err(|error| root_error(name, spec, &available_versions, error))?;
    let problem = Problem::new().requirements(vec![root_version_set.into()]);
    let mut solver = Solver::new(adapter);
    let selected_result = solver.solve(problem);
    let pending_error = solver.provider().take_pending_error();
    if let Some(error) = pending_error {
        return Err(error);
    }
    let cache_snapshot = (**solver.provider().cache.borrow()).clone();

    let selected = match selected_result {
        Ok(selected) => selected,
        Err(UnsolvableOrCancelled::Unsolvable(conflict)) => {
            let culprit = cache_snapshot
                .iter()
                .find_map(|(_, candidates)| {
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
                reason: conflict.display_user_friendly(&solver).to_string(),
            }));
        }
        Err(UnsolvableOrCancelled::Cancelled(_)) => {
            return Err(ResolveError::Conflict(Conflict {
                package: name.clone(),
                constraints: vec![format!("root {name} requires {}", spec_text(spec))],
                available_versions,
                reason: "dependency resolution was cancelled".into(),
            }));
        }
    };

    let provider = solver.provider();
    let nodes = provider.nodes.borrow().clone();
    let edges = provider.edges.borrow().clone();
    let selected_by_node = selected
        .into_iter()
        .map(|solvable| (provider.pool.resolve_solvable(solvable).name, solvable))
        .collect::<BTreeMap<_, _>>();

    let mut packages = BTreeMap::new();
    for (node_name, solvable) in &selected_by_node {
        let Some(node) = nodes.get(node_name) else {
            return Err(ResolveError::Conflict(Conflict {
                package: name.clone(),
                constraints: vec![spec_text(spec)],
                available_versions: available_versions.clone(),
                reason: format!("Resolvo selected unknown resolver node {node_name:?}"),
            }));
        };
        let candidate = &provider.pool.resolve_solvable(*solvable).record.0;
        let id = package_id(&node.package, &candidate.version);
        packages.entry(id).or_insert(ResolvedPackage {
            name: node.package.clone(),
            version: candidate.version.clone(),
            dependencies: candidate.dependencies.clone(),
            optional_dependencies: candidate.optional_dependencies.clone(),
            resolved_dependencies: BTreeMap::new(),
            resolved_optional_dependencies: BTreeMap::new(),
            dist_tags: candidate.dist_tags.clone(),
        });
    }

    for (node_name, solvable) in &selected_by_node {
        let node = &nodes[node_name];
        let candidate = &provider.pool.resolve_solvable(*solvable).record.0;
        let id = package_id(&node.package, &candidate.version);
        let package = packages
            .get_mut(&id)
            .expect("selected package was inserted");
        if let Some(node_edges) = edges.get(&(*node_name, candidate.version.clone())) {
            for dependency in candidate.dependencies.keys() {
                let Some(child) = node_edges.get(dependency) else {
                    continue;
                };
                let Some(child_solvable) = selected_by_node.get(child) else {
                    continue;
                };
                let child_node = &nodes[child];
                let child_version = &provider
                    .pool
                    .resolve_solvable(*child_solvable)
                    .record
                    .0
                    .version;
                package.resolved_dependencies.insert(
                    dependency.clone(),
                    package_id(&child_node.package, child_version),
                );
            }
        }
    }

    let id = selected_by_node
        .get(&root_node)
        .map(|solvable| {
            package_id(
                name,
                &provider.pool.resolve_solvable(*solvable).record.0.version,
            )
        })
        .ok_or_else(|| {
            ResolveError::Conflict(Conflict {
                package: name.clone(),
                constraints: vec![spec_text(spec)],
                available_versions: Vec::new(),
                reason: "Resolvo returned no root package".into(),
            })
        })?;
    Ok((id, packages))
}
