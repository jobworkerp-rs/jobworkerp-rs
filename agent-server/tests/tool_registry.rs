use agent_server::tool_registry::{
    MAX_REGISTRY_FILE_BYTES, ResolvedInputSchema, SchemaResolutionError, ToolRegistration,
    ToolRegistry, ToolRegistryError, WorkerSchemaResolver,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, Semaphore};

type WorkerMethods = HashMap<String, Result<ResolvedInputSchema, SchemaResolutionError>>;
type SchemaKey = (i64, String);

struct ResolutionPause {
    started: Notify,
    release: Semaphore,
}

impl ResolutionPause {
    fn new() -> Self {
        Self {
            started: Notify::new(),
            release: Semaphore::new(0),
        }
    }

    async fn wait_until_started(&self) {
        self.started.notified().await;
    }

    fn release(&self) {
        self.release.add_permits(1);
    }
}

#[derive(Default)]
struct FakeSchemaResolver {
    workers: Mutex<HashMap<i64, WorkerMethods>>,
    pauses: Mutex<HashMap<SchemaKey, Arc<ResolutionPause>>>,
}

impl FakeSchemaResolver {
    fn add_worker(&self, worker_id: i64) {
        self.workers.lock().unwrap().entry(worker_id).or_default();
    }

    fn set_schema(&self, worker_id: i64, using: &str, revision: &str, schema: Value) {
        self.workers
            .lock()
            .unwrap()
            .entry(worker_id)
            .or_default()
            .insert(
                using.to_owned(),
                Ok(ResolvedInputSchema {
                    schema,
                    revision: revision.to_owned(),
                }),
            );
    }

    fn set_missing_schema(&self, worker_id: i64, using: &str) {
        self.workers
            .lock()
            .unwrap()
            .entry(worker_id)
            .or_default()
            .insert(using.to_owned(), Err(SchemaResolutionError::SchemaNotFound));
    }

    fn pause_next_resolution(&self, worker_id: i64, using: &str) -> Arc<ResolutionPause> {
        let pause = Arc::new(ResolutionPause::new());
        self.pauses
            .lock()
            .unwrap()
            .insert((worker_id, using.to_owned()), pause.clone());
        pause
    }
}

#[async_trait]
impl WorkerSchemaResolver for FakeSchemaResolver {
    async fn resolve_input_schema(
        &self,
        worker_id: i64,
        using: &str,
    ) -> Result<ResolvedInputSchema, SchemaResolutionError> {
        let pause = self
            .pauses
            .lock()
            .unwrap()
            .remove(&(worker_id, using.to_owned()));
        if let Some(pause) = pause {
            pause.started.notify_one();
            let permit = pause
                .release
                .acquire()
                .await
                .map_err(|error| SchemaResolutionError::Unavailable(error.to_string()))?;
            permit.forget();
        }

        let workers = self.workers.lock().unwrap();
        let methods = workers
            .get(&worker_id)
            .ok_or(SchemaResolutionError::WorkerNotFound)?;
        methods
            .get(using)
            .cloned()
            .ok_or(SchemaResolutionError::MethodNotFound)?
    }
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "agent-server-tool-registry-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn file(&self) -> PathBuf {
        self.0.join("registry.json")
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn registration(name: &str, worker_id: i64, using: &str) -> ToolRegistration {
    ToolRegistration::new(name, format!("Description for {name}"), worker_id, using)
}

fn install_schema(resolver: &FakeSchemaResolver, worker_id: i64, using: &str) {
    resolver.add_worker(worker_id);
    resolver.set_schema(
        worker_id,
        using,
        "revision-1",
        json!({"type": "object", "properties": {"query": {"type": "string"}}}),
    );
}

async fn registry(path: &Path, resolver: Arc<FakeSchemaResolver>) -> ToolRegistry {
    ToolRegistry::open(path, resolver).await.unwrap()
}

#[tokio::test]
async fn registration_survives_restart_and_each_snapshot_projects_the_current_schema() {
    let directory = TestDirectory::new();
    let resolver = Arc::new(FakeSchemaResolver::default());
    install_schema(&resolver, 41, "lookup");

    let registration: ToolRegistration = serde_json::from_value(json!({
        "name": "catalog_lookup",
        "description": "Search the catalog",
        "workerId": 41,
        "using": "lookup"
    }))
    .unwrap();
    assert!(registration.requires_approval);

    let first = registry(&directory.file(), resolver.clone()).await;
    first.register(registration).await.unwrap();
    let initial = first.snapshot_for_request().await.unwrap();
    assert_eq!(
        initial.get("catalog_lookup").unwrap().input_schema()["type"],
        "object"
    );
    drop(first);

    let restarted = registry(&directory.file(), resolver.clone()).await;
    assert_eq!(restarted.list().unwrap().len(), 1);
    assert!(restarted.list().unwrap()[0].requires_approval);

    resolver.set_schema(
        41,
        "lookup",
        "revision-2",
        json!({"type": "object", "required": ["query"]}),
    );
    let updated_snapshot = restarted.snapshot_for_request().await.unwrap();
    assert_eq!(
        updated_snapshot
            .get("catalog_lookup")
            .unwrap()
            .schema_revision(),
        "revision-2"
    );
    assert_eq!(
        updated_snapshot
            .get("catalog_lookup")
            .unwrap()
            .input_schema()["required"][0],
        "query"
    );
}

#[tokio::test]
async fn duplicate_and_reserved_names_are_rejected_without_changing_the_registry() {
    let directory = TestDirectory::new();
    let resolver = Arc::new(FakeSchemaResolver::default());
    install_schema(&resolver, 41, "lookup");
    let registry = registry(&directory.file(), resolver).await;
    registry
        .register(registration("catalog_lookup", 41, "lookup"))
        .await
        .unwrap();

    assert!(matches!(
        registry
            .register(registration("catalog_lookup", 41, "lookup"))
            .await,
        Err(ToolRegistryError::DuplicateName { .. })
    ));
    for reserved in ["list_skills", "search_skills", "activate_skill"] {
        assert!(matches!(
            registry
                .register(registration(reserved, 41, "lookup"))
                .await,
            Err(ToolRegistryError::ReservedName { .. })
        ));
    }
    assert_eq!(registry.list().unwrap().len(), 1);
}

#[tokio::test]
async fn persisted_duplicate_names_are_rejected_at_startup() {
    let directory = TestDirectory::new();
    let resolver = Arc::new(FakeSchemaResolver::default());
    install_schema(&resolver, 41, "lookup");
    fs::write(
        directory.file(),
        serde_json::to_vec(&json!({
            "version": 1,
            "tools": [
                {"name": "same", "description": "one", "workerId": 41, "using": "lookup"},
                {"name": "same", "description": "two", "workerId": 41, "using": "lookup"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    assert!(matches!(
        ToolRegistry::open(&directory.file(), resolver).await,
        Err(ToolRegistryError::DuplicateName { .. })
    ));

    fs::write(
        directory.file(),
        serde_json::to_vec(&json!({
            "version": 1,
            "tools": [
                {"name": "activate_skill", "description": "collision", "workerId": 41, "using": "lookup"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(matches!(
        ToolRegistry::open(
            &directory.file(),
            Arc::new({
                let resolver = FakeSchemaResolver::default();
                install_schema(&resolver, 41, "lookup");
                resolver
            })
        )
        .await,
        Err(ToolRegistryError::ReservedName { .. })
    ));
}

#[tokio::test]
async fn missing_worker_or_schema_prevents_registration() {
    let directory = TestDirectory::new();
    let resolver = Arc::new(FakeSchemaResolver::default());
    resolver.add_worker(52);
    resolver.set_missing_schema(52, "run");
    resolver.add_worker(53);
    let registry = registry(&directory.file(), resolver).await;

    assert!(matches!(
        registry
            .register(registration("no_worker", 51, "run"))
            .await,
        Err(ToolRegistryError::MissingWorker { worker_id: 51 })
    ));
    assert!(matches!(
        registry
            .register(registration("no_schema", 52, "run"))
            .await,
        Err(ToolRegistryError::MissingSchema { .. })
    ));
    assert!(matches!(
        registry
            .register(registration("no_method", 53, "absent"))
            .await,
        Err(ToolRegistryError::MissingMethod { .. })
    ));
    assert!(registry.list().unwrap().is_empty());
    assert!(!directory.file().exists());
}

#[tokio::test]
async fn dispatch_revalidation_rejects_retargeting_and_schema_revision_changes() {
    let directory = TestDirectory::new();
    let resolver = Arc::new(FakeSchemaResolver::default());
    install_schema(&resolver, 41, "lookup");
    install_schema(&resolver, 41, "lookup_v2");
    install_schema(&resolver, 42, "lookup_v2");
    let registry = registry(&directory.file(), resolver.clone()).await;
    registry
        .register(registration("catalog_lookup", 41, "lookup"))
        .await
        .unwrap();
    let offered = registry.snapshot_for_request().await.unwrap();
    let validated = registry
        .revalidate_for_dispatch(&offered, "catalog_lookup")
        .await
        .unwrap();
    assert_eq!(validated.worker_id(), 41);
    assert_eq!(validated.using(), "lookup");

    registry
        .update(registration("catalog_lookup", 41, "lookup_v2"))
        .await
        .unwrap();
    assert!(matches!(
        registry
            .revalidate_for_dispatch(&offered, "catalog_lookup")
            .await,
        Err(ToolRegistryError::StaleSnapshot { .. })
    ));

    let before_worker_retarget = registry.snapshot_for_request().await.unwrap();
    registry
        .update(registration("catalog_lookup", 42, "lookup_v2"))
        .await
        .unwrap();
    assert!(matches!(
        registry
            .revalidate_for_dispatch(&before_worker_retarget, "catalog_lookup")
            .await,
        Err(ToolRegistryError::StaleSnapshot { .. })
    ));

    let current = registry.snapshot_for_request().await.unwrap();
    resolver.set_schema(
        42,
        "lookup_v2",
        "revision-2",
        json!({"type": "object", "properties": {"url": {"type": "string"}}}),
    );
    assert!(matches!(
        registry
            .revalidate_for_dispatch(&current, "catalog_lookup")
            .await,
        Err(ToolRegistryError::StaleSnapshot { .. })
    ));
}

#[tokio::test]
async fn failed_atomic_write_keeps_the_live_registry_and_previous_file_unchanged() {
    let directory = TestDirectory::new();
    let resolver = Arc::new(FakeSchemaResolver::default());
    install_schema(&resolver, 41, "lookup");
    install_schema(&resolver, 42, "fetch");
    let path = directory.file();
    let registry = registry(&path, resolver).await;
    registry
        .register(registration("catalog_lookup", 41, "lookup"))
        .await
        .unwrap();
    let previous_file = fs::read(&path).unwrap();
    let backup = directory.0.join("registry.previous.json");

    fs::rename(&path, &backup).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(matches!(
        registry
            .register(registration("remote_fetch", 42, "fetch"))
            .await,
        Err(ToolRegistryError::Io { .. })
    ));

    let entries = registry.list().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "catalog_lookup");
    assert_eq!(fs::read(&backup).unwrap(), previous_file);
    assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
}

#[tokio::test]
async fn startup_rejects_registry_files_larger_than_the_read_bound() {
    let directory = TestDirectory::new();
    fs::write(directory.file(), vec![b' '; MAX_REGISTRY_FILE_BYTES + 1]).unwrap();

    assert!(matches!(
        ToolRegistry::open(&directory.file(), Arc::new(FakeSchemaResolver::default())).await,
        Err(ToolRegistryError::RegistryFileTooLarge { .. })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revalidation_releases_registry_lock_while_resolving_and_rejects_a_concurrent_update() {
    let directory = TestDirectory::new();
    let resolver = Arc::new(FakeSchemaResolver::default());
    install_schema(&resolver, 41, "lookup");
    install_schema(&resolver, 41, "lookup_v2");
    install_schema(&resolver, 42, "fetch");
    let registry = Arc::new(registry(&directory.file(), resolver.clone()).await);
    registry
        .register(registration("catalog_lookup", 41, "lookup"))
        .await
        .unwrap();
    let offered = registry.snapshot_for_request().await.unwrap();
    let pause = resolver.pause_next_resolution(41, "lookup");

    let revalidation = {
        let registry = registry.clone();
        tokio::spawn(async move {
            registry
                .revalidate_for_dispatch(&offered, "catalog_lookup")
                .await
        })
    };
    pause.wait_until_started().await;

    tokio::time::timeout(
        Duration::from_secs(2),
        registry.update(registration("catalog_lookup", 42, "fetch")),
    )
    .await
    .expect("an update must not block behind asynchronous schema resolution")
    .unwrap();
    pause.release();

    assert!(matches!(
        revalidation.await.unwrap(),
        Err(ToolRegistryError::StaleSnapshot { .. })
    ));

    let update_pause = resolver.pause_next_resolution(41, "lookup_v2");
    let pending_update = {
        let registry = registry.clone();
        tokio::spawn(async move {
            registry
                .update(registration("catalog_lookup", 41, "lookup_v2"))
                .await
        })
    };
    update_pause.wait_until_started().await;

    tokio::time::timeout(
        Duration::from_secs(2),
        registry.update(registration("catalog_lookup", 41, "lookup")),
    )
    .await
    .expect("a second update must not block behind asynchronous schema resolution")
    .unwrap();
    update_pause.release();

    assert!(matches!(
        pending_update.await.unwrap(),
        Err(ToolRegistryError::ConcurrentModification)
    ));
    assert_eq!(registry.list().unwrap()[0].using, "lookup");
}
