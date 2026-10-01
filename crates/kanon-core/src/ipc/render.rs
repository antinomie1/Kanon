//! `BotApiService` rendering RPC: text and SVG to PNG.

use tonic::{Request, Response, Status};

use kanon_proto::v1::{RenderImageRequest, RenderImageResponse};

use super::CoreApiService;

impl CoreApiService {
    /// Renders text or SVG into a PNG in the plugin's data directory.
    pub(super) async fn render_image_rpc(
        &self,
        _request: Request<RenderImageRequest>,
    ) -> Result<Response<RenderImageResponse>, Status> {
        Err(Status::unimplemented("render_image is not implemented yet"))
    }
}
