//! Bounded MCP attachment storage, naming and retention.

use super::*;

impl McpServer {
    /// Stores one base64 attachment unless the per-call limit is reached; every skip becomes a
    /// note in the tool result.
    pub(super) fn collect_attachment(
        &self,
        attachments: &mut Vec<ToolAttachment>,
        notes: &mut Vec<String>,
        mime: &str,
        data: &str,
        name: Option<&str>,
    ) {
        if attachments.len() >= MCP_MAX_ATTACHMENTS {
            notes.push(format!(
                "[attachment skipped: at most {MCP_MAX_ATTACHMENTS} attachments are forwarded per call]"
            ));
            return;
        }
        match self.store_attachment(mime, data, name) {
            Ok(attachment) => attachments.push(attachment),
            Err(reason) => notes.push(format!("[attachment skipped: {reason}]")),
        }
    }

    /// Decodes one base64 attachment and writes it beside the node's data.
    ///
    /// A named attachment gets a directory of its own so the file keeps exactly that name: the
    /// file name is what a recipient sees. Returns a human-readable reason on failure; the caller
    /// turns it into a note in the tool result so a dropped file is never mistaken for a
    /// successful one.
    pub(super) fn store_attachment(
        &self,
        mime: &str,
        data: &str,
        name: Option<&str>,
    ) -> Result<ToolAttachment, String> {
        use base64::Engine;

        // Some servers inline a full data URL instead of raw base64.
        let payload = data
            .split_once(";base64,")
            .map(|(_, encoded)| encoded)
            .unwrap_or(data)
            .trim();
        // Reject impossible sizes before the decoder reserves an output buffer. The decoded
        // check remains necessary because the final base64 quartet may contain padding.
        if payload.len() > MCP_MAX_ATTACHMENT_BYTES.div_ceil(3) * 4 {
            return Err(format!(
                "{mime} encoded data exceeds the {} byte attachment limit",
                MCP_MAX_ATTACHMENT_BYTES
            ));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .map_err(|err| format!("{mime} is not valid base64: {err}"))?;

        if bytes.len() > MCP_MAX_ATTACHMENT_BYTES {
            return Err(format!(
                "{mime} is {} bytes, above the {} byte limit",
                bytes.len(),
                MCP_MAX_ATTACHMENT_BYTES
            ));
        }

        let path = new_attachment_path(&self.attachment_dir, mime, name)?;
        std::fs::write(&path, &bytes)
            .map_err(|err| format!("failed to write {}: {err}", path.display()))?;

        // The path crosses a process boundary (the adapter plugin opens it), so it is handed over
        // absolute: a relative path would resolve against whatever directory that host runs in.
        let absolute = std::fs::canonicalize(&path).unwrap_or(path);
        Ok(ToolAttachment {
            mime_type: mime.to_string(),
            file_path: Some(absolute.to_string_lossy().to_string()),
            url: None,
        })
    }
}

/// The file name an embedded resource is delivered under, taken from its URI.
///
/// Only the last path component is used, and anything that could leave the attachment directory
/// or is not a portable file name is rejected (the attachment then gets a generated name). A name
/// without an extension gets the MIME type's, so the recipient can open the file.
pub(super) fn resource_file_name(uri: &str, mime: &str) -> Option<String> {
    let rest = uri.split_once("://").map_or(uri, |(_, rest)| rest);
    let path = rest.split(['?', '#']).next().unwrap_or_default();
    let name = path.rsplit('/').next().unwrap_or_default().trim();
    let portable = !name.is_empty()
        && name != "."
        && name != ".."
        && name.len() <= 200
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'));
    if !portable {
        return None;
    }
    let extension = extension_for_mime(mime);
    if name.contains('.') || extension == "bin" {
        Some(name.to_string())
    } else {
        Some(format!("{name}.{extension}"))
    }
}

/// File extension used for one MIME type.
pub(crate) fn extension_for_mime(mime: &str) -> &'static str {
    let mime = mime.trim().to_ascii_lowercase();
    MEDIA_TYPES
        .iter()
        .find(|(known, _)| *known == mime)
        .map_or("bin", |(_, extension)| extension)
}

/// MIME type for a file extension; `application/octet-stream` when it is not a known one.
///
/// An unknown type is still sent, as a named file: the extension it keeps tells the recipient's
/// client what opens it.
pub(crate) fn mime_for_extension(extension: &str) -> &'static str {
    let extension = extension.trim().to_ascii_lowercase();
    MEDIA_TYPES
        .iter()
        .find(|(_, known)| *known == extension)
        .map_or("application/octet-stream", |(mime, _)| mime)
}

/// MIME types and the file extensions they are stored under, for lookups in both directions.
///
/// The first row naming a MIME type gives its extension and the first row naming an extension
/// gives its MIME type, so canonical rows come before their aliases.
const MEDIA_TYPES: &[(&str, &str)] = &[
    ("image/png", "png"),
    ("image/jpeg", "jpg"),
    ("image/jpg", "jpg"),
    ("image/jpeg", "jpeg"),
    ("image/gif", "gif"),
    ("image/webp", "webp"),
    ("image/bmp", "bmp"),
    ("audio/mpeg", "mp3"),
    ("audio/mp3", "mp3"),
    ("audio/wav", "wav"),
    ("audio/wave", "wav"),
    ("audio/x-wav", "wav"),
    ("audio/ogg", "ogg"),
    ("audio/ogg", "oga"),
    ("audio/opus", "opus"),
    ("audio/aac", "aac"),
    ("audio/mp4", "m4a"),
    ("audio/m4a", "m4a"),
    ("audio/x-m4a", "m4a"),
    ("audio/flac", "flac"),
    ("audio/aiff", "aiff"),
    ("audio/amr", "amr"),
    ("audio/silk", "silk"),
    ("video/mp4", "mp4"),
    ("video/webm", "webm"),
    ("video/quicktime", "mov"),
    ("video/x-matroska", "mkv"),
    ("application/pdf", "pdf"),
    ("application/zip", "zip"),
    ("application/json", "json"),
    ("text/plain", "txt"),
    ("text/csv", "csv"),
    ("text/markdown", "md"),
];

/// Process-wide sequence that keeps attachment names unique when several tools store attachments
/// within the same millisecond.
static ATTACHMENT_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Reserves a fresh path in `dir` for one attachment; the caller writes the file.
///
/// A named attachment gets a directory of its own so the file keeps exactly that name: the file
/// name is what a recipient sees. An unnamed one is stored flat under its MIME type's extension.
pub(crate) fn new_attachment_path(
    dir: &Path,
    mime: &str,
    name: Option<&str>,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir)
        .map_err(|err| format!("failed to create {}: {err}", dir.display()))?;
    let seq = ATTACHMENT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    match name {
        Some(name) => {
            let slot = dir.join(format!("{stamp}-{seq}"));
            std::fs::create_dir(&slot)
                .map_err(|err| format!("failed to create {}: {err}", slot.display()))?;
            Ok(slot.join(name))
        }
        None => Ok(dir.join(format!("{stamp}-{seq}.{}", extension_for_mime(mime)))),
    }
}

/// Deletes attachment files older than `max_age` and reports how many were swept.
///
/// Called at node start: a file that has survived its retention window was either never delivered
/// (and is already recorded in the dead-letter log) or has long since been sent.
pub fn prune_attachments(dir: &Path, max_age: Duration) -> std::io::Result<usize> {
    if !dir.is_dir() {
        return Ok(0);
    }

    let mut removed = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        // Named attachments live in a directory of their own; either entry is one attachment.
        let is_dir = path.is_dir();
        if !is_dir && !path.is_file() {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        let Ok(age) = std::time::SystemTime::now().duration_since(modified) else {
            continue;
        };
        if age < max_age {
            continue;
        }
        let swept = if is_dir {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if swept.is_ok() {
            removed += 1;
        }
    }

    Ok(removed)
}
