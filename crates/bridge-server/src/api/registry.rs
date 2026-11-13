//! Runtime resources have exactly the same lifetime as their registered tenant.
use super::*;
use bridge_core::chains::{ChainCoordinator, RuntimeFactory};
use std::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub type CoordinatorTask = tokio::task::JoinHandle<Result<(), ChainError>>;
pub type InitialSpaces = HashMap<Uuid, (ChainController, CoordinatorTask)>;

struct Space {
    controller: ChainController,
    streams: Arc<Semaphore>,
    task: JoinHandle<Result<(), ChainError>>,
}
pub(super) struct Registry {
    spaces: RwLock<HashMap<Uuid, Space>>,
}
impl Registry {
    pub fn new(controllers: InitialSpaces) -> Self {
        Self {
            spaces: RwLock::new(
                controllers
                    .into_iter()
                    .map(|(id, (controller, task))| {
                        (
                            id,
                            Space {
                                controller,
                                streams: Arc::new(Semaphore::new(8)),
                                task,
                            },
                        )
                    })
                    .collect(),
            ),
        }
    }
    pub fn controller(&self, id: Uuid) -> Result<ChainController, ApiError> {
        self.spaces
            .read()
            .unwrap()
            .get(&id)
            .map(|s| s.controller.clone())
            .ok_or(ApiError::unavailable())
    }
    pub fn streams(&self, id: Uuid) -> Result<Arc<Semaphore>, ApiError> {
        self.spaces
            .read()
            .unwrap()
            .get(&id)
            .map(|s| s.streams.clone())
            .ok_or(ApiError::unavailable())
    }
    pub fn controllers(&self) -> Vec<ChainController> {
        self.spaces
            .read()
            .unwrap()
            .values()
            .map(|s| s.controller.clone())
            .collect()
    }
    pub fn add(&self, tenant: Uuid, factory: RuntimeFactory, stop: &CancellationToken) {
        let (controller, task) =
            ChainCoordinator::spawn(factory.store.clone(), factory, tenant, stop.child_token());
        let previous = self.spaces.write().unwrap().insert(
            tenant,
            Space {
                controller,
                streams: Arc::new(Semaphore::new(8)),
                task,
            },
        );
        assert!(previous.is_none(), "tenant already registered");
    }
    pub async fn remove(&self, tenant: Uuid) -> Result<(), ApiError> {
        let space = self.spaces.write().unwrap().remove(&tenant);
        if let Some(space) = space {
            space.controller.shutdown().cancel();
            space
                .task
                .await
                .map_err(|_| ApiError::internal())?
                .map_err(ApiError::from)?;
        }
        Ok(())
    }
    pub async fn shutdown(&self) -> Result<(), ApiError> {
        let spaces: Vec<_> = self
            .spaces
            .write()
            .unwrap()
            .drain()
            .map(|(_, s)| s)
            .collect();
        for space in &spaces {
            space.controller.shutdown().cancel();
        }
        let mut failed = false;
        for space in spaces {
            failed |= !matches!(space.task.await, Ok(Ok(())));
        }
        if failed {
            Err(ApiError::internal())
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::{
        auth::AuthService,
        storage::SqliteStore,
        upstream::Upstream,
        vault::{EncryptionKey, Vault},
    };
    use sqlx::sqlite::SqliteConnectOptions;
    #[tokio::test]
    async fn removal_is_immediate_but_shutdown_waits_for_dynamic_worker_completion() {
        let store = Arc::new(
            SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
                .await
                .unwrap(),
        );
        let tenant = Uuid::new_v4();
        let vault = Vault::new(store.clone(), EncryptionKey::from_bytes(&[35; 32]).unwrap());
        vault.create_tenant_with_seed(tenant).await.unwrap();
        let upstream = Upstream::new(None).unwrap();
        let factory = RuntimeFactory {
            store,
            auth: Arc::new(AuthService::new(vault.clone(), upstream.clone())),
            vault,
            upstream,
        };
        let registry = Arc::new(Registry::new(HashMap::new()));
        registry.add(tenant, factory, &CancellationToken::new());
        let controller = registry.controller(tenant).unwrap();
        let (release, wait) = tokio::sync::oneshot::channel();
        {
            let mut spaces = registry.spaces.write().unwrap();
            let space = spaces.remove(&tenant).unwrap();
            let task = tokio::spawn(async move {
                let _ = wait.await;
                space.task.await.unwrap()
            });
            spaces.insert(
                tenant,
                Space {
                    controller: space.controller,
                    streams: space.streams,
                    task,
                },
            );
        }
        let other = registry.clone();
        let finishing = tokio::spawn(async move { other.shutdown().await.map_err(|_| ()) });
        controller.shutdown().cancelled().await;
        assert!(registry.controller(tenant).is_err());
        assert!(
            !finishing.is_finished(),
            "must join, not detach, the dynamic worker"
        );
        release.send(()).unwrap();
        assert!(finishing.await.unwrap().is_ok());
    }
}
