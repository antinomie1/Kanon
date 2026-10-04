//! Plugin installation request validation and native package-tool dispatch.

use super::*;

/// Request body for `POST /api/v1/plugins/install` as JSON.
///
/// Exactly one source must be given. Unknown fields are refused so a misspelled `replace` cannot
/// silently turn an intended upgrade into a `409`, or a misspelled source into "no source".
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallRequest {
    /// Folder (or `plugin.toml`) on this node.
    #[serde(default)]
    pub path: Option<String>,
    /// `https` URL of a `.kpk` / `.zip` package.
    #[serde(default)]
    pub url: Option<String>,
    /// Git repository URL.
    #[serde(default)]
    pub git: Option<String>,
    /// Branch or tag to clone; only valid together with `git`.
    #[serde(default, rename = "ref")]
    pub git_ref: Option<String>,
    /// Overwrite a plugin already installed under the same id.
    #[serde(default)]
    pub replace: bool,
}

impl InstallRequest {
    /// Turns the request into an install source, enforcing "exactly one source".
    fn into_source(self) -> Result<(InstallSource, bool), ApiError> {
        let non_empty = |value: Option<String>| {
            value
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let (path, url, git, git_ref) = (
            non_empty(self.path),
            non_empty(self.url),
            non_empty(self.git),
            non_empty(self.git_ref),
        );
        if git_ref.is_some() && git.is_none() {
            return Err(ApiError::BadRequest(
                "Field 'ref' is only valid together with 'git'".to_string(),
            ));
        }
        let source = match (path, url, git) {
            (Some(path), None, None) => InstallSource::Path(PathBuf::from(path)),
            (None, Some(url), None) => InstallSource::Url(url),
            (None, None, Some(url)) => InstallSource::Git { url, git_ref },
            (None, None, None) => {
                return Err(ApiError::BadRequest(
                    "Give the plugin to install as one of 'path', 'url' or 'git'".to_string(),
                ));
            }
            _ => {
                return Err(ApiError::BadRequest(
                    "Give exactly one of 'path', 'url' or 'git'".to_string(),
                ));
            }
        };
        Ok((source, self.replace))
    }
}

/// Largest JSON install request; it only carries a path or a URL.
const MAX_INSTALL_JSON_BYTES: usize = 64 * 1024;

/// Body limit of the install route: a full package plus the multipart framing around it.
pub(super) fn install_body_limit() -> usize {
    MAX_PACKAGE_BYTES as usize + 1024 * 1024
}

/// Installs a plugin from a local folder, an uploaded package, a package URL or a Git repository.
///
/// - `application/json`: [`InstallRequest`] (`path`, `url` or `git` + optional `ref`, `replace`).
/// - `multipart/form-data`: a `file` part holding the package (or a `path` part), plus an
///   optional `replace` part (`true` / `false`).
pub(super) async fn install_plugin(
    State(state): State<ApiState>,
    req: axum::extract::Request,
) -> Result<Json<InstallPluginResponse>, ApiError> {
    let content_type = req
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let (source, replace) = if content_type.starts_with("multipart/form-data") {
        read_install_multipart(&state, req).await?
    } else if content_type.is_empty() || content_type.contains("application/json") {
        let bytes = axum::body::to_bytes(req.into_body(), MAX_INSTALL_JSON_BYTES)
            .await
            .map_err(|e| ApiError::BadRequest(format!("Failed to read request body: {e}")))?;
        let payload: InstallRequest = serde_json::from_slice(&bytes)
            .map_err(|e| ApiError::BadRequest(format!("Invalid JSON request body: {e}")))?;
        payload.into_source()?
    } else {
        return Err(ApiError::BadRequest(format!(
            "Unsupported Content-Type: '{content_type}'. Expected application/json or multipart/form-data"
        )));
    };

    Ok(Json(
        plugin_install::install(&state, source, replace).await?,
    ))
}

/// Reads a multipart install request (`file` or `path`, optional `replace`).
pub(super) async fn read_install_multipart(
    state: &ApiState,
    req: axum::extract::Request,
) -> Result<(InstallSource, bool), ApiError> {
    // The route's `DefaultBodyLimit` bounds the whole body; going past it surfaces as a field
    // error with status 413, which is reported as such instead of as a malformed upload.
    let multipart_error = |err: axum::extract::multipart::MultipartError| {
        if err.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::PayloadTooLarge(format!(
                "Plugin packages are limited to {MAX_PACKAGE_BYTES} bytes"
            ))
        } else {
            ApiError::BadRequest(format!("Invalid multipart payload: {}", err.body_text()))
        }
    };

    let mut multipart = axum::extract::Multipart::from_request(req, state)
        .await
        .map_err(|err| ApiError::BadRequest(format!("Invalid multipart payload: {err}")))?;

    let mut archive: Option<Vec<u8>> = None;
    let mut path: Option<String> = None;
    let mut replace = false;
    while let Some(field) = multipart.next_field().await.map_err(multipart_error)? {
        let name = field.name().unwrap_or("").to_string();
        let is_package = field
            .file_name()
            .is_some_and(|file| file.ends_with(".kpk") || file.ends_with(".zip"));
        if name == "file" || is_package {
            archive = Some(field.bytes().await.map_err(multipart_error)?.to_vec());
        } else if name == "path" {
            let text = field.text().await.map_err(multipart_error)?;
            path = Some(text.trim().to_string()).filter(|text| !text.is_empty());
        } else if name == "replace" {
            let text = field.text().await.map_err(multipart_error)?;
            replace = match text.trim() {
                "true" | "1" => true,
                "false" | "0" | "" => false,
                other => {
                    return Err(ApiError::BadRequest(format!(
                        "Field 'replace' must be 'true' or 'false', not '{other}'"
                    )));
                }
            };
        } else {
            return Err(ApiError::BadRequest(format!(
                "Unknown multipart field '{name}'; expected 'file', 'path' or 'replace'"
            )));
        }
    }

    match (archive, path) {
        (Some(bytes), None) => Ok((InstallSource::Archive(bytes), replace)),
        (None, Some(path)) => Ok((InstallSource::Path(PathBuf::from(path)), replace)),
        (Some(_), Some(_)) => Err(ApiError::BadRequest(
            "Send either 'file' or 'path', not both".to_string(),
        )),
        (None, None) => Err(ApiError::BadRequest(
            "Multipart form must contain either 'file' (.kpk/.zip) or 'path'".to_string(),
        )),
    }
}
