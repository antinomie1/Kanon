//! `BotApiService` storage RPCs: the central per-plugin key-value store.

use tonic::{Request, Response, Status};

use kanon_proto::v1::{
    DeleteStorageRequest, DeleteStorageResponse, GetStorageRequest, GetStorageResponse,
    ListStorageRequest, ListStorageResponse, SetStorageRequest, SetStorageResponse,
};

use super::CoreApiService;

impl CoreApiService {
    /// Stores a value in the plugin's namespace of the central KV store.
    pub(super) async fn set_storage_rpc(
        &self,
        _request: Request<SetStorageRequest>,
    ) -> Result<Response<SetStorageResponse>, Status> {
        Err(Status::unimplemented("set_storage is not implemented yet"))
    }

    /// Reads a value from the plugin's namespace of the central KV store.
    pub(super) async fn get_storage_rpc(
        &self,
        _request: Request<GetStorageRequest>,
    ) -> Result<Response<GetStorageResponse>, Status> {
        Err(Status::unimplemented("get_storage is not implemented yet"))
    }

    /// Removes a key from the plugin's namespace of the central KV store.
    pub(super) async fn delete_storage_rpc(
        &self,
        _request: Request<DeleteStorageRequest>,
    ) -> Result<Response<DeleteStorageResponse>, Status> {
        Err(Status::unimplemented(
            "delete_storage is not implemented yet",
        ))
    }

    /// Lists the plugin's live keys under a prefix.
    pub(super) async fn list_storage_rpc(
        &self,
        _request: Request<ListStorageRequest>,
    ) -> Result<Response<ListStorageResponse>, Status> {
        Err(Status::unimplemented("list_storage is not implemented yet"))
    }
}
