//! `BotApiService` storage RPCs: the central per-plugin key-value store.
//!
//! Each request names the plugin whose namespace it uses; the SDKs fill it in from the plugin's
//! own manifest. Namespaces keep plugins from colliding, not from each other: plugins run with
//! the node's privileges and could read `data/` directly, so the store adds no access control a
//! plugin could not already bypass.
//!
//! Every operation runs on the blocking pool, so a slow disk never stalls the gRPC runtime.

use std::sync::Arc;
use std::time::Duration;

use tonic::{Request, Response, Status};

use kanon_proto::v1::{
    DeleteStorageRequest, DeleteStorageResponse, GetStorageRequest, GetStorageResponse,
    ListStorageRequest, ListStorageResponse, SetStorageRequest, SetStorageResponse,
};
use kanon_storage::{KvError, KvStore};

use super::CoreApiService;

/// Maps a store failure onto its status: bad input is the caller's, everything else the core's.
fn kv_status(err: KvError) -> Status {
    match err {
        KvError::PluginId(_) | KvError::Key(_) => Status::invalid_argument(err.to_string()),
        KvError::ValueTooLarge(_) => Status::resource_exhausted(err.to_string()),
        KvError::Database(_) | KvError::Io(_) => Status::internal(err.to_string()),
    }
}

impl CoreApiService {
    /// Runs one store operation on the blocking pool.
    async fn run_kv<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&KvStore) -> Result<T, KvError> + Send + 'static,
    ) -> Result<T, Status> {
        let kv: Arc<KvStore> = self
            .kv
            .clone()
            .ok_or_else(|| Status::unavailable("This core has no key-value store"))?;
        tokio::task::spawn_blocking(move || operation(&kv))
            .await
            .map_err(|err| Status::internal(format!("key-value operation aborted: {err}")))?
            .map_err(kv_status)
    }

    /// Stores a value in the plugin's namespace; `ttl_seconds` 0 keeps it until deleted.
    pub(super) async fn set_storage_rpc(
        &self,
        request: Request<SetStorageRequest>,
    ) -> Result<Response<SetStorageResponse>, Status> {
        let req = request.into_inner();
        let ttl = match req.ttl_seconds {
            0 => None,
            secs if secs > 0 => Some(Duration::from_secs(secs.unsigned_abs())),
            _ => return Err(Status::invalid_argument("ttl_seconds must not be negative")),
        };
        self.run_kv(move |kv| kv.set(&req.plugin_id, &req.key, &req.value, ttl))
            .await?;
        Ok(Response::new(SetStorageResponse { success: true }))
    }

    /// Reads a value from the plugin's namespace; an expired key is not found.
    pub(super) async fn get_storage_rpc(
        &self,
        request: Request<GetStorageRequest>,
    ) -> Result<Response<GetStorageResponse>, Status> {
        let req = request.into_inner();
        let value = self
            .run_kv(move |kv| kv.get(&req.plugin_id, &req.key))
            .await?;
        Ok(Response::new(GetStorageResponse {
            found: value.is_some(),
            value: value.unwrap_or_default(),
        }))
    }

    /// Removes a key from the plugin's namespace.
    pub(super) async fn delete_storage_rpc(
        &self,
        request: Request<DeleteStorageRequest>,
    ) -> Result<Response<DeleteStorageResponse>, Status> {
        let req = request.into_inner();
        let deleted = self
            .run_kv(move |kv| kv.delete(&req.plugin_id, &req.key))
            .await?;
        Ok(Response::new(DeleteStorageResponse { deleted }))
    }

    /// Lists the plugin's live keys under a prefix.
    pub(super) async fn list_storage_rpc(
        &self,
        request: Request<ListStorageRequest>,
    ) -> Result<Response<ListStorageResponse>, Status> {
        let req = request.into_inner();
        let keys = self
            .run_kv(move |kv| kv.list(&req.plugin_id, &req.prefix))
            .await?;
        Ok(Response::new(ListStorageResponse { keys }))
    }
}
