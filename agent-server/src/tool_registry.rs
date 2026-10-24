//! Agent Server-owned registrations that bind public tool names to Worker methods.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

pub const MAX_REGISTRY_FILE_BYTES: usize = 1024 * 1024;
const REGISTRY_FILE_VERSION: u32 = 1;
const TEMP_FILE_ATTEMPTS: usize = 32;

pub const RESERVED_TOOL_NAMES: [&str; 3] = ["list_skills", "search_skills", "activate_skill"];

static NEXT_TEMP_FILE_ID: AtomicU64 = AtomicU64::new(0);

fn default_requires_approval() -> bool {
    true
}

/// An admin-owned binding. The `using` string is kept verbatim for dispatch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolRegistration {
    pub name: String,
    pub description: String,
    pub worker_id: i64,
    #[serde(rename = "using")]
    pub using: String,
    #[serde(default = "default_requires_approval")]
    pub requires_approval: bool,
}

impl ToolRegistration {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        worker_id: i64,
        using: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            worker_id,
            using: using.into(),
            requires_approval: true,
        }
    }

    pub fn with_requires_approval(mut self, requires_approval: bool) -> Self {
        self.requires_approval = requires_approval;
        self
    }
}

/// The projected input schema and its resolver-owned revision token.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedInputSchema {
    pub schema: Value,
    pub revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaResolutionError {
    WorkerNotFound,
    MethodNotFound,
    SchemaNotFound,
    Unavailable(String),
}

impl fmt::Display for SchemaResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkerNotFound => formatter.write_str("worker was not found"),
            Self::MethodNotFound => formatter.write_str("worker method was not found"),
            Self::SchemaNotFound => formatter.write_str("worker method input schema was not found"),
            Self::Unavailable(message) => formatter.write_str(message),
        }
    }
}

impl StdError for SchemaResolutionError {}

/// Resolves worker method schemas without coupling the registry to a transport.
#[async_trait]
pub trait WorkerSchemaResolver: Send + Sync {
    async fn resolve_input_schema(
        &self,
        worker_id: i64,
        using: &str,
    ) -> Result<ResolvedInputSchema, SchemaResolutionError>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolSnapshot {
    registration: ToolRegistration,
    input_schema: Value,
    schema_revision: String,
}

impl ToolSnapshot {
    pub fn registration(&self) -> &ToolRegistration {
        &self.registration
    }

    pub fn input_schema(&self) -> &Value {
        &self.input_schema
    }

    pub fn schema_revision(&self) -> &str {
        &self.schema_revision
    }
}

/// A request-local view. It is rebuilt from the current registry and resolver on every call.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolRegistrySnapshot {
    tools: BTreeMap<String, ToolSnapshot>,
}

impl ToolRegistrySnapshot {
    pub fn tools(&self) -> &BTreeMap<String, ToolSnapshot> {
        &self.tools
    }

    pub fn get(&self, public_name: &str) -> Option<&ToolSnapshot> {
        self.tools.get(public_name)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DispatchTarget {
    public_name: String,
    worker_id: i64,
    using: String,
    requires_approval: bool,
    input_schema: Value,
    schema_revision: String,
}

impl DispatchTarget {
    pub fn public_name(&self) -> &str {
        &self.public_name
    }

    pub fn worker_id(&self) -> i64 {
        self.worker_id
    }

    pub fn using(&self) -> &str {
        &self.using
    }

    pub fn requires_approval(&self) -> bool {
        self.requires_approval
    }

    pub fn input_schema(&self) -> &Value {
        &self.input_schema
    }

    pub fn schema_revision(&self) -> &str {
        &self.schema_revision
    }
}

#[derive(Debug)]
pub enum ToolRegistryError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Json(serde_json::Error),
    RegistryFileTooLarge {
        limit: usize,
    },
    RegistryTooLarge {
        limit: usize,
    },
    UnsupportedFileVersion {
        version: u32,
    },
    InvalidRegistration {
        name: String,
        reason: &'static str,
    },
    ReservedName {
        name: String,
    },
    DuplicateName {
        name: String,
    },
    ToolNotFound {
        name: String,
    },
    MissingWorker {
        worker_id: i64,
    },
    MissingMethod {
        worker_id: i64,
        using: String,
    },
    MissingSchema {
        worker_id: i64,
        using: String,
    },
    InvalidSchema {
        worker_id: i64,
        using: String,
    },
    MissingSchemaRevision {
        worker_id: i64,
        using: String,
    },
    ResolverFailure {
        worker_id: i64,
        using: String,
        message: String,
    },
    StaleSnapshot {
        name: String,
    },
    ConcurrentModification,
    RevisionExhausted,
    LockPoisoned,
}

impl fmt::Display for ToolRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "failed to {operation} {}: {source}",
                path.display()
            ),
            Self::Json(source) => write!(formatter, "invalid tool registry JSON: {source}"),
            Self::RegistryFileTooLarge { limit } => {
                write!(
                    formatter,
                    "tool registry file exceeds the {limit}-byte read limit"
                )
            }
            Self::RegistryTooLarge { limit } => {
                write!(
                    formatter,
                    "tool registry exceeds the {limit}-byte write limit"
                )
            }
            Self::UnsupportedFileVersion { version } => {
                write!(
                    formatter,
                    "unsupported tool registry file version {version}"
                )
            }
            Self::InvalidRegistration { name, reason } => {
                write!(formatter, "invalid tool registration {name:?}: {reason}")
            }
            Self::ReservedName { name } => {
                write!(formatter, "tool name {name:?} is reserved by Agent Server")
            }
            Self::DuplicateName { name } => {
                write!(formatter, "tool name {name:?} is already registered")
            }
            Self::ToolNotFound { name } => write!(formatter, "tool {name:?} is not registered"),
            Self::MissingWorker { worker_id } => {
                write!(formatter, "worker {worker_id} was not found")
            }
            Self::MissingMethod { worker_id, using } => {
                write!(
                    formatter,
                    "method {using:?} was not found on worker {worker_id}"
                )
            }
            Self::MissingSchema { worker_id, using } => write!(
                formatter,
                "input schema for method {using:?} on worker {worker_id} was not found"
            ),
            Self::InvalidSchema { worker_id, using } => write!(
                formatter,
                "input schema for method {using:?} on worker {worker_id} is not a JSON Schema"
            ),
            Self::MissingSchemaRevision { worker_id, using } => write!(
                formatter,
                "input schema revision for method {using:?} on worker {worker_id} is empty"
            ),
            Self::ResolverFailure {
                worker_id,
                using,
                message,
            } => write!(
                formatter,
                "failed to resolve method {using:?} on worker {worker_id}: {message}"
            ),
            Self::StaleSnapshot { name } => {
                write!(
                    formatter,
                    "tool {name:?} changed after its request snapshot was created"
                )
            }
            Self::ConcurrentModification => {
                formatter.write_str("tool registry changed during an asynchronous operation")
            }
            Self::RevisionExhausted => {
                formatter.write_str("tool registry revision counter is exhausted")
            }
            Self::LockPoisoned => formatter.write_str("tool registry lock was poisoned"),
        }
    }
}

impl StdError for ToolRegistryError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json(source) => Some(source),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedRegistry {
    version: u32,
    tools: Vec<ToolRegistration>,
}

struct RegistryState {
    entries: BTreeMap<String, ToolRegistration>,
    revision: u64,
}

/// Persistent Agent Server registry, independent of jobworkerp FunctionSet state.
pub struct ToolRegistry {
    path: PathBuf,
    resolver: Arc<dyn WorkerSchemaResolver>,
    state: Mutex<RegistryState>,
}

impl ToolRegistry {
    /// Opens the admin-owned JSON registry and validates every loaded target/schema.
    pub async fn open(
        path: impl AsRef<Path>,
        resolver: Arc<dyn WorkerSchemaResolver>,
    ) -> Result<Self, ToolRegistryError> {
        let path = path.as_ref().to_path_buf();
        let registrations = read_registry_file(&path)?;
        let mut entries = BTreeMap::new();

        for registration in registrations {
            validate_registration(&registration)?;
            if entries.contains_key(&registration.name) {
                return Err(ToolRegistryError::DuplicateName {
                    name: registration.name,
                });
            }
            entries.insert(registration.name.clone(), registration);
        }

        for registration in entries.values() {
            resolve_registration(resolver.as_ref(), registration).await?;
        }

        Ok(Self {
            path,
            resolver,
            state: Mutex::new(RegistryState {
                entries,
                revision: 0,
            }),
        })
    }

    /// Adds a registration. Existing public names are never silently replaced.
    pub async fn register(&self, registration: ToolRegistration) -> Result<(), ToolRegistryError> {
        validate_registration(&registration)?;
        let expected_revision = {
            let state = self.lock_state()?;
            if state.entries.contains_key(&registration.name) {
                return Err(ToolRegistryError::DuplicateName {
                    name: registration.name,
                });
            }
            state.revision
        };

        resolve_registration(self.resolver.as_ref(), &registration).await?;

        let mut state = self.lock_state()?;
        if state.revision != expected_revision {
            return Err(ToolRegistryError::ConcurrentModification);
        }
        let next_revision = next_registry_revision(state.revision)?;

        let mut next = state.entries.clone();
        next.insert(registration.name.clone(), registration);
        self.persist_entries(&next)?;
        state.entries = next;
        state.revision = next_revision;
        Ok(())
    }

    /// Replaces an existing registration after validating its new target/schema.
    pub async fn update(&self, registration: ToolRegistration) -> Result<(), ToolRegistryError> {
        validate_registration(&registration)?;
        let expected = {
            let state = self.lock_state()?;
            let current = state.entries.get(&registration.name).ok_or_else(|| {
                ToolRegistryError::ToolNotFound {
                    name: registration.name.clone(),
                }
            })?;
            (state.revision, current.clone())
        };

        resolve_registration(self.resolver.as_ref(), &registration).await?;

        let mut state = self.lock_state()?;
        if state.revision != expected.0
            || state.entries.get(&registration.name) != Some(&expected.1)
        {
            return Err(ToolRegistryError::ConcurrentModification);
        }
        let next_revision = next_registry_revision(state.revision)?;

        let mut next = state.entries.clone();
        next.insert(registration.name.clone(), registration);
        self.persist_entries(&next)?;
        state.entries = next;
        state.revision = next_revision;
        Ok(())
    }

    pub fn remove(&self, public_name: &str) -> Result<(), ToolRegistryError> {
        let mut state = self.lock_state()?;
        if !state.entries.contains_key(public_name) {
            return Err(ToolRegistryError::ToolNotFound {
                name: public_name.to_owned(),
            });
        }

        let mut next = state.entries.clone();
        next.remove(public_name);
        let next_revision = next_registry_revision(state.revision)?;
        self.persist_entries(&next)?;
        state.entries = next;
        state.revision = next_revision;
        Ok(())
    }

    /// Returns the current admin registrations in stable public-name order.
    pub fn list(&self) -> Result<Vec<ToolRegistration>, ToolRegistryError> {
        let state = self.lock_state()?;
        Ok(state.entries.values().cloned().collect())
    }

    /// Builds a fresh schema projection for one request; no schema is cached in the registry.
    pub async fn snapshot_for_request(&self) -> Result<ToolRegistrySnapshot, ToolRegistryError> {
        let (expected_revision, registrations) = {
            let state = self.lock_state()?;
            (
                state.revision,
                state
                    .entries
                    .values()
                    .cloned()
                    .collect::<Vec<ToolRegistration>>(),
            )
        };
        let mut tools = BTreeMap::new();
        for registration in registrations {
            let resolved = resolve_registration(self.resolver.as_ref(), &registration).await?;
            tools.insert(
                registration.name.clone(),
                ToolSnapshot {
                    registration,
                    input_schema: resolved.schema,
                    schema_revision: resolved.revision,
                },
            );
        }
        if self.lock_state()?.revision != expected_revision {
            return Err(ToolRegistryError::ConcurrentModification);
        }
        Ok(ToolRegistrySnapshot { tools })
    }

    /// Rechecks the server-held target and schema immediately before dispatch.
    pub async fn revalidate_for_dispatch(
        &self,
        snapshot: &ToolRegistrySnapshot,
        public_name: &str,
    ) -> Result<DispatchTarget, ToolRegistryError> {
        let offered =
            snapshot
                .get(public_name)
                .ok_or_else(|| ToolRegistryError::StaleSnapshot {
                    name: public_name.to_owned(),
                })?;

        let (expected_revision, current) =
            {
                let state = self.lock_state()?;
                let current = state.entries.get(public_name).ok_or_else(|| {
                    ToolRegistryError::StaleSnapshot {
                        name: public_name.to_owned(),
                    }
                })?;
                if current.worker_id != offered.registration.worker_id
                    || current.using != offered.registration.using
                    || current.requires_approval != offered.registration.requires_approval
                {
                    return Err(ToolRegistryError::StaleSnapshot {
                        name: public_name.to_owned(),
                    });
                }
                (state.revision, current.clone())
            };

        let resolved = resolve_registration(self.resolver.as_ref(), &current).await?;
        let state = self.lock_state()?;
        let unchanged =
            state.revision == expected_revision && state.entries.get(public_name) == Some(&current);
        drop(state);
        if !unchanged {
            return Err(ToolRegistryError::StaleSnapshot {
                name: public_name.to_owned(),
            });
        }
        if resolved.revision != offered.schema_revision || resolved.schema != offered.input_schema {
            return Err(ToolRegistryError::StaleSnapshot {
                name: public_name.to_owned(),
            });
        }

        Ok(DispatchTarget {
            public_name: public_name.to_owned(),
            worker_id: current.worker_id,
            using: current.using.clone(),
            requires_approval: current.requires_approval,
            input_schema: resolved.schema,
            schema_revision: resolved.revision,
        })
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, RegistryState>, ToolRegistryError> {
        self.state
            .lock()
            .map_err(|_| ToolRegistryError::LockPoisoned)
    }

    fn persist_entries(
        &self,
        entries: &BTreeMap<String, ToolRegistration>,
    ) -> Result<(), ToolRegistryError> {
        let persisted = PersistedRegistry {
            version: REGISTRY_FILE_VERSION,
            tools: entries.values().cloned().collect(),
        };
        let contents = serde_json::to_vec(&persisted).map_err(ToolRegistryError::Json)?;
        if contents.len() > MAX_REGISTRY_FILE_BYTES {
            return Err(ToolRegistryError::RegistryTooLarge {
                limit: MAX_REGISTRY_FILE_BYTES,
            });
        }

        let parent = parent_directory(&self.path);
        fs::create_dir_all(parent).map_err(|source| ToolRegistryError::Io {
            operation: "create registry directory",
            path: parent.to_path_buf(),
            source,
        })?;

        let (temporary_path, mut temporary_file) = self.create_temporary_file(parent)?;
        if let Err(source) = temporary_file.write_all(&contents) {
            drop(temporary_file);
            let _ = fs::remove_file(&temporary_path);
            return Err(ToolRegistryError::Io {
                operation: "write temporary registry",
                path: temporary_path,
                source,
            });
        }
        if let Err(source) = temporary_file.sync_all() {
            drop(temporary_file);
            let _ = fs::remove_file(&temporary_path);
            return Err(ToolRegistryError::Io {
                operation: "sync temporary registry",
                path: temporary_path,
                source,
            });
        }
        drop(temporary_file);

        if let Err(source) = fs::rename(&temporary_path, &self.path) {
            let _ = fs::remove_file(&temporary_path);
            return Err(ToolRegistryError::Io {
                operation: "replace registry file",
                path: self.path.clone(),
                source,
            });
        }
        Ok(())
    }

    fn create_temporary_file(&self, parent: &Path) -> Result<(PathBuf, File), ToolRegistryError> {
        let mut last_collision_path = parent.join(".agent-server-tool-registry.tmp");
        for _ in 0..TEMP_FILE_ATTEMPTS {
            let id = NEXT_TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let temporary_path = parent.join(format!(
                ".agent-server-tool-registry-{}-{id}.tmp",
                std::process::id()
            ));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary_path)
            {
                Ok(file) => return Ok((temporary_path, file)),
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                    last_collision_path = temporary_path;
                }
                Err(source) => {
                    return Err(ToolRegistryError::Io {
                        operation: "create temporary registry",
                        path: temporary_path,
                        source,
                    });
                }
            }
        }

        Err(ToolRegistryError::Io {
            operation: "create temporary registry",
            path: last_collision_path,
            source: io::Error::new(
                io::ErrorKind::AlreadyExists,
                "could not allocate a unique temporary file",
            ),
        })
    }
}

fn parent_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn next_registry_revision(revision: u64) -> Result<u64, ToolRegistryError> {
    revision
        .checked_add(1)
        .ok_or(ToolRegistryError::RevisionExhausted)
}

fn read_registry_file(path: &Path) -> Result<Vec<ToolRegistration>, ToolRegistryError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(ToolRegistryError::Io {
                operation: "open registry file",
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let mut contents = Vec::new();
    file.take(MAX_REGISTRY_FILE_BYTES as u64 + 1)
        .read_to_end(&mut contents)
        .map_err(|source| ToolRegistryError::Io {
            operation: "read registry file",
            path: path.to_path_buf(),
            source,
        })?;
    if contents.len() > MAX_REGISTRY_FILE_BYTES {
        return Err(ToolRegistryError::RegistryFileTooLarge {
            limit: MAX_REGISTRY_FILE_BYTES,
        });
    }

    let persisted: PersistedRegistry =
        serde_json::from_slice(&contents).map_err(ToolRegistryError::Json)?;
    if persisted.version != REGISTRY_FILE_VERSION {
        return Err(ToolRegistryError::UnsupportedFileVersion {
            version: persisted.version,
        });
    }
    Ok(persisted.tools)
}

fn validate_registration(registration: &ToolRegistration) -> Result<(), ToolRegistryError> {
    if registration.name.trim().is_empty() {
        return Err(ToolRegistryError::InvalidRegistration {
            name: registration.name.clone(),
            reason: "public name must not be empty",
        });
    }
    if RESERVED_TOOL_NAMES.contains(&registration.name.as_str()) {
        return Err(ToolRegistryError::ReservedName {
            name: registration.name.clone(),
        });
    }
    if registration.description.trim().is_empty() {
        return Err(ToolRegistryError::InvalidRegistration {
            name: registration.name.clone(),
            reason: "description must not be empty",
        });
    }
    if registration.worker_id <= 0 {
        return Err(ToolRegistryError::InvalidRegistration {
            name: registration.name.clone(),
            reason: "worker id must be positive",
        });
    }
    if registration.using.trim().is_empty() {
        return Err(ToolRegistryError::InvalidRegistration {
            name: registration.name.clone(),
            reason: "using method must not be empty",
        });
    }
    Ok(())
}

async fn resolve_registration(
    resolver: &dyn WorkerSchemaResolver,
    registration: &ToolRegistration,
) -> Result<ResolvedInputSchema, ToolRegistryError> {
    let resolved = resolver
        .resolve_input_schema(registration.worker_id, &registration.using)
        .await
        .map_err(|error| match error {
            SchemaResolutionError::WorkerNotFound => ToolRegistryError::MissingWorker {
                worker_id: registration.worker_id,
            },
            SchemaResolutionError::MethodNotFound => ToolRegistryError::MissingMethod {
                worker_id: registration.worker_id,
                using: registration.using.clone(),
            },
            SchemaResolutionError::SchemaNotFound => ToolRegistryError::MissingSchema {
                worker_id: registration.worker_id,
                using: registration.using.clone(),
            },
            SchemaResolutionError::Unavailable(message) => ToolRegistryError::ResolverFailure {
                worker_id: registration.worker_id,
                using: registration.using.clone(),
                message,
            },
        })?;

    if resolved.schema.is_null() {
        return Err(ToolRegistryError::MissingSchema {
            worker_id: registration.worker_id,
            using: registration.using.clone(),
        });
    }
    if !resolved.schema.is_object() && !resolved.schema.is_boolean() {
        return Err(ToolRegistryError::InvalidSchema {
            worker_id: registration.worker_id,
            using: registration.using.clone(),
        });
    }
    if resolved.revision.trim().is_empty() {
        return Err(ToolRegistryError::MissingSchemaRevision {
            worker_id: registration.worker_id,
            using: registration.using.clone(),
        });
    }
    Ok(resolved)
}
