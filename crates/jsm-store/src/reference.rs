use super::{Store, StoreError};
use fs4::FileExt;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const SCHEMA_VERSION: i32 = 2;
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(60);

/// Package metadata supplied when a successful install publishes its project references.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackageReference {
    pub name: String,
    pub version: String,
    pub integrity: String,
    pub logical_size: u64,
    pub physical_size: u64,
    pub file_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredPackage {
    pub name: String,
    pub version: String,
    pub integrity: String,
    pub first_stored_at: i64,
    pub last_used_at: i64,
    pub logical_size: u64,
    pub physical_size: u64,
    pub file_count: u64,
    pub reference_count: u64,
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectReference {
    pub project_id: String,
    pub path: PathBuf,
    pub lockfile_hash: String,
    pub last_install_at: i64,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageProject {
    pub project_id: String,
    pub path: PathBuf,
    pub lockfile_hash: String,
    pub last_install_at: i64,
    pub stale: bool,
}

/// Handle to a shared (install) or exclusive (maintenance) advisory store lease.
/// Closing the handle releases the kernel lock, including after process termination.
#[derive(Debug)]
pub struct StoreLease {
    _file: File,
    pub path: PathBuf,
}

/// SQLite metadata index. Connections are deliberately short-lived; filesystem and
/// network operations must never be performed while a SQL write transaction is open.
#[derive(Debug, Clone)]
pub struct ReferenceRegistry {
    database: PathBuf,
    locks: PathBuf,
}

impl ReferenceRegistry {
    pub fn open(store_root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let root = store_root.as_ref();
        let index = root.join("index");
        let locks = root.join("locks");
        fs::create_dir_all(&index)?;
        fs::create_dir_all(&locks)?;
        let _schema_lease = acquire_lock(
            locks.join("reference-schema.lock"),
            false,
            DEFAULT_LOCK_TIMEOUT,
        )?;
        let registry = Self {
            database: index.join("store.db"),
            locks,
        };
        let mut connection = registry.connect()?;
        registry.migrate(&mut connection)?;
        Ok(registry)
    }

    pub fn database_path(&self) -> &Path {
        &self.database
    }

    fn connect(&self) -> Result<Connection, StoreError> {
        let connection = Connection::open(&self.database)?;
        connection.busy_timeout(Duration::from_secs(15))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        // DELETE journaling is intentionally retained: unlike WAL, it does not
        // require shared-memory coordination and is the safer default on filesystems
        // whose multi-host locking behavior cannot be assumed.
        connection.pragma_update(None, "journal_mode", "DELETE")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        Ok(connection)
    }

    fn migrate(&self, connection: &mut Connection) -> Result<(), StoreError> {
        let version: i32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::Invalid(format!(
                "reference registry schema {version} is newer than supported {SCHEMA_VERSION}"
            )));
        }
        if version == 0 {
            let transaction = connection.transaction()?;
            transaction.execute_batch(
                "CREATE TABLE projects (
                    project_id TEXT PRIMARY KEY NOT NULL,
                    root_path TEXT NOT NULL UNIQUE,
                    lockfile_hash TEXT NOT NULL,
                    last_install_at INTEGER NOT NULL,
                    stale INTEGER NOT NULL DEFAULT 0 CHECK (stale IN (0, 1))
                );
                CREATE TABLE packages (
                    name TEXT NOT NULL,
                    version TEXT NOT NULL,
                    integrity TEXT NOT NULL,
                    first_stored_at INTEGER NOT NULL,
                    last_used_at INTEGER NOT NULL,
                    logical_size INTEGER NOT NULL CHECK (logical_size >= 0),
                    physical_size INTEGER NOT NULL CHECK (physical_size >= 0),
                    file_count INTEGER NOT NULL CHECK (file_count >= 0),
                    pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
                    PRIMARY KEY (name, version, integrity)
                );
                CREATE TABLE project_packages (
                    project_id TEXT NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
                    name TEXT NOT NULL,
                    version TEXT NOT NULL,
                    integrity TEXT NOT NULL,
                    PRIMARY KEY (project_id, name, version, integrity),
                    FOREIGN KEY (name, version, integrity)
                      REFERENCES packages(name, version, integrity) ON DELETE CASCADE
                );
                CREATE INDEX project_packages_by_package
                  ON project_packages(name, version, integrity);
                CREATE INDEX packages_by_last_use ON packages(last_used_at);
                PRAGMA user_version = 1;",
            )?;
            transaction.commit()?;
        }
        let migrated_version: i32 =
            connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if migrated_version == 1 {
            let transaction = connection.transaction()?;
            transaction.execute_batch(
                "CREATE TABLE corrupt_packages (
                    name TEXT NOT NULL,
                    version TEXT NOT NULL,
                    integrity TEXT NOT NULL,
                    detected_at INTEGER NOT NULL,
                    reason TEXT NOT NULL,
                    PRIMARY KEY (name, version, integrity)
                );
                PRAGMA user_version = 2;",
            )?;
            transaction.commit()?;
        }
        Ok(())
    }

    /// Atomically replace one project's complete reference set after its link and
    /// lockfile commit. The project ID marker follows directory moves.
    pub fn register_install(
        &self,
        project_root: impl AsRef<Path>,
        lockfile_hash: &str,
        packages: &[PackageReference],
    ) -> Result<ProjectReference, StoreError> {
        let root = fs::canonicalize(project_root.as_ref())?;
        let project_id = project_id(&root)?;
        let root_text = root.to_string_lossy().into_owned();
        let now = now_epoch_seconds();
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO projects(project_id, root_path, lockfile_hash, last_install_at, stale)
             VALUES (?1, ?2, ?3, ?4, 0)
             ON CONFLICT(project_id) DO UPDATE SET root_path=excluded.root_path,
               lockfile_hash=excluded.lockfile_hash, last_install_at=excluded.last_install_at,
               stale=0",
            params![project_id, root_text, lockfile_hash, now],
        )?;
        transaction.execute(
            "DELETE FROM project_packages WHERE project_id = ?1",
            params![project_id],
        )?;
        for package in packages {
            let logical_size = to_sql_size(package.logical_size)?;
            let physical_size = to_sql_size(package.physical_size)?;
            let file_count = to_sql_size(package.file_count)?;
            transaction.execute(
                "INSERT INTO packages(name, version, integrity, first_stored_at, last_used_at,
                    logical_size, physical_size, file_count, pinned)
                 VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7, 0)
                 ON CONFLICT(name, version, integrity) DO UPDATE SET
                    last_used_at=excluded.last_used_at,
                    logical_size=excluded.logical_size,
                    physical_size=excluded.physical_size,
                    file_count=excluded.file_count",
                params![
                    package.name,
                    package.version,
                    package.integrity,
                    now,
                    logical_size,
                    physical_size,
                    file_count
                ],
            )?;
            transaction.execute(
                "INSERT INTO project_packages(project_id, name, version, integrity)
                 VALUES (?1, ?2, ?3, ?4)",
                params![project_id, package.name, package.version, package.integrity],
            )?;
        }
        transaction.commit()?;
        Ok(ProjectReference {
            project_id,
            path: root,
            lockfile_hash: lockfile_hash.to_owned(),
            last_install_at: now,
            stale: false,
        })
    }

    pub fn packages(&self) -> Result<Vec<StoredPackage>, StoreError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT p.name, p.version, p.integrity, p.first_stored_at, p.last_used_at,
                    p.logical_size, p.physical_size, p.file_count,
                    (SELECT COUNT(*) FROM project_packages r
                      WHERE r.name=p.name AND r.version=p.version AND r.integrity=p.integrity),
                    p.pinned
             FROM packages p ORDER BY p.name COLLATE BINARY, p.version COLLATE BINARY,
                    p.integrity COLLATE BINARY",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(StoredPackage {
                name: row.get(0)?,
                version: row.get(1)?,
                integrity: row.get(2)?,
                first_stored_at: row.get(3)?,
                last_used_at: row.get(4)?,
                logical_size: from_sql_size(row.get(5)?),
                physical_size: from_sql_size(row.get(6)?),
                file_count: from_sql_size(row.get(7)?),
                reference_count: from_sql_size(row.get(8)?),
                pinned: row.get::<_, i64>(9)? != 0,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn package(
        &self,
        name: &str,
        version: &str,
        integrity: &str,
    ) -> Result<Option<StoredPackage>, StoreError> {
        Ok(self.packages()?.into_iter().find(|package| {
            package.name == name && package.version == version && package.integrity == integrity
        }))
    }

    pub fn record_package(&self, package: &PackageReference) -> Result<(), StoreError> {
        let connection = self.connect()?;
        let now = now_epoch_seconds();
        connection.execute(
            "INSERT INTO packages(name, version, integrity, first_stored_at, last_used_at,
                logical_size, physical_size, file_count, pinned)
             VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7, 0)
             ON CONFLICT(name, version, integrity) DO UPDATE SET
                last_used_at=excluded.last_used_at,
                logical_size=excluded.logical_size,
                physical_size=excluded.physical_size,
                file_count=excluded.file_count",
            params![
                package.name,
                package.version,
                package.integrity,
                now,
                to_sql_size(package.logical_size)?,
                to_sql_size(package.physical_size)?,
                to_sql_size(package.file_count)?
            ],
        )?;
        Ok(())
    }

    pub fn usage(
        &self,
        name: &str,
        version: &str,
        integrity: &str,
    ) -> Result<Vec<UsageProject>, StoreError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT p.project_id, p.root_path, p.lockfile_hash, p.last_install_at, p.stale
               FROM projects p JOIN project_packages r USING(project_id)
              WHERE r.name=?1 AND r.version=?2 AND r.integrity=?3
              ORDER BY p.root_path COLLATE BINARY",
        )?;
        let rows = statement.query_map(params![name, version, integrity], |row| {
            Ok(UsageProject {
                project_id: row.get(0)?,
                path: PathBuf::from(row.get::<_, String>(1)?),
                lockfile_hash: row.get(2)?,
                last_install_at: row.get(3)?,
                stale: row.get::<_, i64>(4)? != 0,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn projects(&self) -> Result<Vec<ProjectReference>, StoreError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT project_id, root_path, lockfile_hash, last_install_at, stale
               FROM projects ORDER BY root_path COLLATE BINARY",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(ProjectReference {
                project_id: row.get(0)?,
                path: PathBuf::from(row.get::<_, String>(1)?),
                lockfile_hash: row.get(2)?,
                last_install_at: row.get(3)?,
                stale: row.get::<_, i64>(4)? != 0,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn project_for_path(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<Option<ProjectReference>, StoreError> {
        let root = fs::canonicalize(path.as_ref())?;
        let connection = self.connect()?;
        Ok(connection
            .query_row(
                "SELECT project_id, root_path, lockfile_hash, last_install_at, stale
               FROM projects WHERE root_path=?1",
                params![root.to_string_lossy()],
                |row| {
                    Ok(ProjectReference {
                        project_id: row.get(0)?,
                        path: PathBuf::from(row.get::<_, String>(1)?),
                        lockfile_hash: row.get(2)?,
                        last_install_at: row.get(3)?,
                        stale: row.get::<_, i64>(4)? != 0,
                    })
                },
            )
            .optional()?)
    }

    pub fn packages_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<PackageReference>, StoreError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT p.name, p.version, p.integrity, p.logical_size, p.physical_size, p.file_count
               FROM packages p JOIN project_packages r
                 ON (p.name=r.name AND p.version=r.version AND p.integrity=r.integrity)
              WHERE r.project_id=?1
              ORDER BY p.name COLLATE BINARY, p.version COLLATE BINARY, p.integrity COLLATE BINARY",
        )?;
        let rows = statement.query_map(params![project_id], |row| {
            Ok(PackageReference {
                name: row.get(0)?,
                version: row.get(1)?,
                integrity: row.get(2)?,
                logical_size: from_sql_size(row.get(3)?),
                physical_size: from_sql_size(row.get(4)?),
                file_count: from_sql_size(row.get(5)?),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn remove_project_at(&self, path: impl AsRef<Path>) -> Result<(), StoreError> {
        let root = fs::canonicalize(path.as_ref()).unwrap_or_else(|_| path.as_ref().to_path_buf());
        let connection = self.connect()?;
        connection.execute(
            "DELETE FROM projects WHERE root_path=?1",
            params![root.to_string_lossy()],
        )?;
        Ok(())
    }

    /// Mark missing or lockfile-mismatched projects stale without releasing
    /// references. A missing path may have been moved; explicit forgetting is
    /// required before its package content can become eligible for pruning.
    pub fn detect_stale_projects(&self) -> Result<Vec<ProjectReference>, StoreError> {
        let projects = self.projects()?;
        let observations = projects
            .iter()
            .map(|project| {
                let lock_path = project.path.join("jsm.lock");
                let current_hash = fs::read(&lock_path)
                    .ok()
                    .map(|contents| format!("{:x}", Sha256::digest(contents)))
                    .or_else(|| {
                        fs::read(project.path.join("node_modules/.jsm/state.json"))
                            .ok()
                            .and_then(|bytes| {
                                serde_json::from_slice::<serde_json::Value>(&bytes).ok()
                            })
                            .and_then(|state| {
                                state.get("lockfile_hash")?.as_str().map(str::to_owned)
                            })
                    });
                let deleted = !project.path.exists();
                let stale =
                    deleted || current_hash.as_deref() != Some(project.lockfile_hash.as_str());
                (project.project_id.clone(), stale)
            })
            .collect::<Vec<_>>();
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        for (project_id, stale) in observations {
            transaction.execute(
                "UPDATE projects SET stale=?2 WHERE project_id=?1",
                params![project_id, if stale { 1i64 } else { 0i64 }],
            )?;
            // A missing path may have been moved rather than deleted. Retain its
            // references until an explicit forget operation releases them.
        }
        transaction.commit()?;
        self.projects()
    }

    pub fn set_pinned(
        &self,
        name: &str,
        version: &str,
        integrity: &str,
        pinned: bool,
    ) -> Result<bool, StoreError> {
        let connection = self.connect()?;
        let changed = connection.execute(
            "UPDATE packages SET pinned=?4 WHERE name=?1 AND version=?2 AND integrity=?3",
            params![name, version, integrity, i64::from(pinned)],
        )?;
        Ok(changed != 0)
    }

    pub fn unreferenced_unpinned(&self) -> Result<Vec<StoredPackage>, StoreError> {
        Ok(self
            .packages()?
            .into_iter()
            .filter(|package| package.reference_count == 0 && !package.pinned)
            .collect())
    }

    pub fn mark_corrupt(
        &self,
        name: &str,
        version: &str,
        integrity: &str,
    ) -> Result<(), StoreError> {
        let connection = self.connect()?;
        connection.execute(
            "INSERT INTO corrupt_packages(name, version, integrity, detected_at, reason)
             VALUES (?1, ?2, ?3, ?4, 'verification failed')
             ON CONFLICT(name, version, integrity) DO UPDATE SET
               detected_at=excluded.detected_at, reason=excluded.reason",
            params![name, version, integrity, now_epoch_seconds()],
        )?;
        Ok(())
    }

    pub fn clear_corrupt(
        &self,
        name: &str,
        version: &str,
        integrity: &str,
    ) -> Result<(), StoreError> {
        let connection = self.connect()?;
        connection.execute(
            "DELETE FROM corrupt_packages WHERE name=?1 AND version=?2 AND integrity=?3",
            params![name, version, integrity],
        )?;
        Ok(())
    }

    pub fn is_corrupt(
        &self,
        name: &str,
        version: &str,
        integrity: &str,
    ) -> Result<bool, StoreError> {
        let connection = self.connect()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM corrupt_packages WHERE name=?1 AND version=?2 AND integrity=?3",
            params![name, version, integrity],
            |row| row.get(0),
        )?;
        Ok(count != 0)
    }

    pub fn remove_package_record(
        &self,
        name: &str,
        version: &str,
        integrity: &str,
        force: bool,
    ) -> Result<bool, StoreError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        let references: u64 = transaction.query_row(
            "SELECT COUNT(*) FROM project_packages WHERE name=?1 AND version=?2 AND integrity=?3",
            params![name, version, integrity],
            |row| row.get::<_, i64>(0).map(from_sql_size),
        )?;
        if references > 0 && !force {
            return Err(StoreError::Invalid(format!(
                "cannot remove {name}@{version}: referenced by {references} project(s); use --force only if those projects may be broken"
            )));
        }
        transaction.execute(
            "DELETE FROM packages WHERE name=?1 AND version=?2 AND integrity=?3",
            params![name, version, integrity],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn delete_project(&self, project_id: &str) -> Result<(), StoreError> {
        let connection = self.connect()?;
        connection.execute(
            "DELETE FROM projects WHERE project_id=?1",
            params![project_id],
        )?;
        Ok(())
    }

    /// Open a bounded-time maintenance lease. Installers take a shared lease;
    /// GC, quarantine, and destructive verify operations require the exclusive one.
    pub fn maintenance_lease(&self, shared: bool) -> Result<StoreLease, StoreError> {
        let path = self.locks.join("maintenance.lock");
        acquire_lock(path, shared, DEFAULT_LOCK_TIMEOUT)
    }

    /// Single-flight lock keyed by the canonical package identity.
    pub fn package_lease(&self, identity: &str) -> Result<StoreLease, StoreError> {
        let key = format!("package:{identity}");
        let hash = format!("{:x}", Sha256::digest(key.as_bytes()));
        acquire_lock(
            self.locks.join(format!("package-{hash}.lock")),
            false,
            DEFAULT_LOCK_TIMEOUT,
        )
    }

    pub fn project_lease(&self, project_id: &str) -> Result<StoreLease, StoreError> {
        let hash = format!("{:x}", Sha256::digest(project_id.as_bytes()));
        acquire_lock(
            self.locks.join(format!("project-{hash}.lock")),
            false,
            DEFAULT_LOCK_TIMEOUT,
        )
    }
}

impl Store {
    pub fn reference_registry(&self) -> Result<ReferenceRegistry, StoreError> {
        ReferenceRegistry::open(&self.root)
    }

    pub fn maintenance_lease(&self, shared: bool) -> Result<StoreLease, StoreError> {
        self.reference_registry()?.maintenance_lease(shared)
    }

    pub fn package_lease(&self, identity: &str) -> Result<StoreLease, StoreError> {
        self.reference_registry()?.package_lease(identity)
    }
}

fn acquire_lock(path: PathBuf, shared: bool, timeout: Duration) -> Result<StoreLease, StoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    let start = Instant::now();
    loop {
        let lock_result = if shared {
            FileExt::try_lock_shared(&file)
        } else {
            FileExt::try_lock(&file)
        };
        match lock_result {
            Ok(()) => break,
            Err(fs4::TryLockError::WouldBlock) => {
                if start.elapsed() >= timeout {
                    let owner =
                        read_lock_owner(&mut file).unwrap_or_else(|| "unknown process".into());
                    return Err(StoreError::Invalid(format!(
                        "timed out waiting for store lock {} held by {owner}; retry after the active operation finishes",
                        path.display()
                    )));
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(fs4::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
    if !shared {
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        writeln!(
            file,
            "pid={} acquired_at={}",
            std::process::id(),
            now_epoch_seconds()
        )?;
        file.sync_all()?;
    }
    Ok(StoreLease { _file: file, path })
}

fn read_lock_owner(file: &mut File) -> Option<String> {
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut text = String::new();
    file.read_to_string(&mut text).ok()?;
    (!text.trim().is_empty()).then(|| text.trim().to_owned())
}

fn project_id(root: &Path) -> Result<String, StoreError> {
    let marker = root.join(".jsm-project-id");
    if let Ok(contents) = fs::read_to_string(&marker) {
        let id = contents.trim();
        if id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(id.to_ascii_lowercase());
        }
    }
    let entropy = format!(
        "{}:{}:{}:{}",
        root.display(),
        std::process::id(),
        now_epoch_seconds(),
        PROJECT_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let id = format!("{:x}", Sha256::digest(entropy.as_bytes()));
    super::atomic_write(&marker, format!("{id}\n").as_bytes())?;
    Ok(id)
}

static PROJECT_ID_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn hash_file(path: impl AsRef<Path>) -> Result<String, StoreError> {
    let bytes = fs::read(path)?;
    Ok(hash_bytes(&bytes))
}

fn now_epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}

fn to_sql_size(size: u64) -> Result<i64, StoreError> {
    i64::try_from(size)
        .map_err(|_| StoreError::Invalid("store metadata size exceeds SQLite integer range".into()))
}

fn from_sql_size(size: i64) -> u64 {
    u64::try_from(size).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(name: &str, version: &str) -> PackageReference {
        PackageReference {
            name: name.into(),
            version: version.into(),
            integrity: format!("sha512-{name}-{version}"),
            logical_size: 10,
            physical_size: 8,
            file_count: 2,
        }
    }

    #[test]
    fn registry_transactions_track_add_remove_move_and_delete_ground_truth() {
        let temp = tempfile::tempdir().unwrap();
        let registry = ReferenceRegistry::open(temp.path().join("store")).unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::write(first.join("jsm.lock"), "first").unwrap();
        fs::write(second.join("jsm.lock"), "second").unwrap();
        let a = package("a", "1.0.0");
        let b = package("b", "2.0.0");
        registry
            .register_install(&first, &hash_bytes(b"first"), &[a.clone(), b.clone()])
            .unwrap();
        let second_ref = registry
            .register_install(&second, &hash_bytes(b"second"), std::slice::from_ref(&a))
            .unwrap();
        assert_eq!(
            registry
                .package("a", "1.0.0", &a.integrity)
                .unwrap()
                .unwrap()
                .reference_count,
            2
        );
        assert_eq!(
            registry
                .package("b", "2.0.0", &b.integrity)
                .unwrap()
                .unwrap()
                .reference_count,
            1
        );

        // Simulate a successful update removing b, then move first while preserving its identity.
        registry
            .register_install(&first, &hash_bytes(b"updated"), std::slice::from_ref(&a))
            .unwrap();
        assert_eq!(
            registry
                .package("b", "2.0.0", &b.integrity)
                .unwrap()
                .unwrap()
                .reference_count,
            0
        );
        let moved = temp.path().join("moved");
        fs::rename(&first, &moved).unwrap();
        let stale = registry.detect_stale_projects().unwrap();
        assert!(
            stale
                .iter()
                .any(|project| project.path == first && project.stale)
        );
        assert_eq!(
            registry
                .package("a", "1.0.0", &a.integrity)
                .unwrap()
                .unwrap()
                .reference_count,
            2
        );
        let moved_ref = registry
            .register_install(&moved, &hash_bytes(b"updated"), std::slice::from_ref(&a))
            .unwrap();
        assert_eq!(moved_ref.path, fs::canonicalize(&moved).unwrap());
        assert_eq!(registry.usage("a", "1.0.0", &a.integrity).unwrap().len(), 2);

        fs::remove_dir_all(&second).unwrap();
        let stale = registry.detect_stale_projects().unwrap();
        assert_eq!(stale.len(), 2);
        assert!(
            stale
                .iter()
                .any(|project| project.project_id == second_ref.project_id && project.stale)
        );
        assert_eq!(
            registry
                .package("a", "1.0.0", &a.integrity)
                .unwrap()
                .unwrap()
                .reference_count,
            2
        );
        registry.delete_project(&second_ref.project_id).unwrap();
        assert_eq!(registry.projects().unwrap().len(), 1);
        assert_eq!(
            registry
                .package("a", "1.0.0", &a.integrity)
                .unwrap()
                .unwrap()
                .reference_count,
            1
        );
    }

    #[test]
    fn package_reference_replacement_is_atomic_and_pins_survive_updates() {
        let temp = tempfile::tempdir().unwrap();
        let registry = ReferenceRegistry::open(temp.path().join("store")).unwrap();
        let project = temp.path().join("project");
        fs::create_dir_all(&project).unwrap();
        let a = package("a", "1.0.0");
        let b = package("b", "1.0.0");
        registry
            .register_install(&project, "hash-1", std::slice::from_ref(&a))
            .unwrap();
        registry
            .set_pinned("a", "1.0.0", &a.integrity, true)
            .unwrap();
        registry
            .register_install(&project, "hash-2", std::slice::from_ref(&b))
            .unwrap();
        assert_eq!(
            registry
                .package("a", "1.0.0", &a.integrity)
                .unwrap()
                .unwrap()
                .reference_count,
            0
        );
        assert!(
            registry
                .package("a", "1.0.0", &a.integrity)
                .unwrap()
                .unwrap()
                .pinned
        );
        assert!(registry.unreferenced_unpinned().unwrap().is_empty());
    }

    #[test]
    fn advisory_maintenance_leases_are_shared_but_exclude_exclusive_maintenance() {
        let temp = tempfile::tempdir().unwrap();
        let registry = ReferenceRegistry::open(temp.path().join("store")).unwrap();
        let first = registry.maintenance_lease(true).unwrap();
        let second = registry.maintenance_lease(true).unwrap();
        let path = first.path.clone();
        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        assert!(matches!(
            FileExt::try_lock(&contender),
            Err(fs4::TryLockError::WouldBlock)
        ));
        drop(first);
        drop(second);
        assert!(FileExt::try_lock(&contender).is_ok());
    }
}
