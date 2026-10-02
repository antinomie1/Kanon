//! `BotApiService` rendering RPC: text and SVG to PNG (see [`crate::render`]).
//!
//! The PNG is written into the plugin's own data directory (`<id>/render/`) and its path
//! returned, ready for an image segment's `file_path`. Files are named by a hash of what was
//! rendered, so rendering the same card twice reuses one file, and files older than
//! [`RENDER_RETENTION`] are swept on each render: they only need to live until the message
//! carrying them has been delivered.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};
use tonic::{Request, Response, Status};

use kanon_proto::v1::render_image_request::Source;
use kanon_proto::v1::{RenderImageRequest, RenderImageResponse};

use super::CoreApiService;
use crate::render::{DEFAULT_TEXT_WIDTH, RenderError, render_svg, render_text};

/// How long rendered images are kept.
pub const RENDER_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

fn render_status(err: RenderError) -> Status {
    match err {
        RenderError::Invalid(_) | RenderError::Svg(_) => Status::invalid_argument(err.to_string()),
        RenderError::NoFonts => Status::failed_precondition(err.to_string()),
        RenderError::Encode(_) => Status::internal(err.to_string()),
    }
}

impl CoreApiService {
    /// Renders text or SVG into a PNG in the plugin's data directory.
    pub(super) async fn render_image_rpc(
        &self,
        request: Request<RenderImageRequest>,
    ) -> Result<Response<RenderImageResponse>, Status> {
        let req = request.into_inner();
        let dir = self
            .plugin_data
            .resolve_plugin_dir(&req.plugin_id)
            .map_err(|err| match err.kind() {
                std::io::ErrorKind::InvalidInput => Status::invalid_argument(err.to_string()),
                _ => Status::internal(format!("plugin data directory unavailable: {err}")),
            })?
            .join("render");
        let source = req
            .source
            .ok_or_else(|| Status::invalid_argument("either text or svg is required"))?;
        let width = if req.width == 0 {
            DEFAULT_TEXT_WIDTH
        } else {
            req.width
        };

        let response = tokio::task::spawn_blocking(move || -> Result<_, Status> {
            let (key, image) = match &source {
                Source::Text(text) => (
                    format!("text\0{width}\0{text}"),
                    render_text(text, width).map_err(render_status)?,
                ),
                Source::Svg(svg) => (
                    format!("svg\0{svg}"),
                    render_svg(svg).map_err(render_status)?,
                ),
            };
            std::fs::create_dir_all(&dir).map_err(|err| {
                Status::internal(format!("cannot create {}: {err}", dir.display()))
            })?;
            sweep(&dir);
            let digest = Sha256::digest(key.as_bytes());
            let name: String = digest[..16]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let path = dir.join(format!("{name}.png"));
            write_atomically(&path, &image.png).map_err(|err| {
                Status::internal(format!("cannot write {}: {err}", path.display()))
            })?;
            // Absolute, because the path travels to an adapter that does not share our idea of
            // the working directory.
            let path = std::path::absolute(&path).unwrap_or(path);
            Ok(RenderImageResponse {
                file_path: path.to_string_lossy().into_owned(),
                width: image.width,
                height: image.height,
            })
        })
        .await
        .map_err(|err| Status::internal(format!("rendering aborted: {err}")))??;
        Ok(Response::new(response))
    }
}

/// Writes through a temporary file so a concurrent reader never sees half a PNG.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary: PathBuf = path.with_extension(format!("png.{}.tmp", std::process::id()));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path)
}

/// Removes rendered images older than [`RENDER_RETENTION`]; failures only leave files behind.
fn sweep(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let expired = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > RENDER_RETENTION);
        if expired {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
