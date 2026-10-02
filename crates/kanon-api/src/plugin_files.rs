//! Static files a plugin ships next to its manifest: console pages and translations.
//!
//! # Pages (`pages/`)
//! A plugin may ship a small static web UI under `pages/` (an `index.html` plus its assets). The
//! gateway serves it read-only at `/api/v1/plugins/<id>/pages/`, and the console shows it in a
//! sandboxed frame. [`resolve_page`] turns a request path into a file inside that directory and is
//! the single place that enforces the safety rules:
//!
//! - every path segment is checked *after* percent-decoding, so `..`, `%2e%2e` and `..%2f` are all
//!   the same rejected traversal attempt;
//! - hidden files (any segment starting with `.`) are never served — `.env` or `.git` left in a
//!   plugin folder must not leak through its pages;
//! - the resolved file is canonicalized and must stay below the canonical `pages/` directory, so
//!   a symlink pointing out of the folder is treated as missing;
//! - a directory is only ever answered with its `index.html`, never with a listing.
//!
//! # Translations (`i18n/<locale>.json`)
//! A plugin may translate its display texts. Each file is a flat JSON object of strings keyed by
//! one of the documented key shapes ([`translation_key_is_known`]). Files that cannot be used are
//! reported in [`Translations::errors`] instead of being skipped silently, so a plugin author can
//! see in the console why a language does not show up.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Directory, relative to the plugin root, holding the plugin's static console pages.
pub const PAGES_DIR: &str = "pages";

/// Directory, relative to the plugin root, holding the plugin's translation files.
pub const I18N_DIR: &str = "i18n";

/// Largest translation file the gateway reads; display texts never need more.
pub const MAX_TRANSLATION_BYTES: u64 = 256 * 1024;

/// Most translation files read for one plugin, bounding the work of a catalog read.
pub const MAX_TRANSLATION_FILES: usize = 64;

/// Outcome of resolving a request below a plugin's `pages/` directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageTarget {
    /// A regular file to serve.
    File(PathBuf),
    /// The path names a directory but the request did not end with `/`.
    ///
    /// Relative links in the directory's `index.html` would resolve against the parent, so the
    /// caller redirects to the slash-terminated form instead of serving the file.
    Directory,
}

/// Why a page request cannot be served.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PageError {
    /// The path is malformed or tries to leave the pages directory.
    #[error("Invalid page path: {0}")]
    Invalid(String),
    /// Nothing servable exists at the path (missing, hidden, outside the folder, or a directory
    /// without `index.html`).
    #[error("No such page")]
    NotFound,
}

/// Whether a plugin ships console pages (`pages/index.html` exists).
pub fn has_pages(plugin_dir: &Path) -> bool {
    plugin_dir.join(PAGES_DIR).join("index.html").is_file()
}

/// Resolves a decoded request path below `<plugin_dir>/pages/` to the file to serve.
///
/// `request_path` is the part of the URL after `/pages/` (possibly empty), already
/// percent-decoded. `trailing_slash` says whether the original request ended with `/`, which
/// decides between serving a directory's `index.html` and asking for a redirect.
pub fn resolve_page(
    plugin_dir: &Path,
    request_path: &str,
    trailing_slash: bool,
) -> Result<PageTarget, PageError> {
    let relative = sanitize_relative_path(request_path)?;

    // Canonical forms are compared rather than joined paths: `starts_with` on a non-canonical
    // path would accept `pages/link/../../secret` or a symlink that points elsewhere.
    let root = plugin_dir
        .join(PAGES_DIR)
        .canonicalize()
        .map_err(|_| PageError::NotFound)?;
    let candidate = root
        .join(&relative)
        .canonicalize()
        .map_err(|_| PageError::NotFound)?;
    if !candidate.starts_with(&root) {
        return Err(PageError::NotFound);
    }

    if candidate.is_dir() {
        if !trailing_slash {
            return Ok(PageTarget::Directory);
        }
        let index = candidate
            .join("index.html")
            .canonicalize()
            .map_err(|_| PageError::NotFound)?;
        return if index.starts_with(&root) && index.is_file() {
            Ok(PageTarget::File(index))
        } else {
            // No listing: a directory without an index simply has nothing to show.
            Err(PageError::NotFound)
        };
    }

    if candidate.is_file() {
        Ok(PageTarget::File(candidate))
    } else {
        // Sockets, devices and other special files are never served.
        Err(PageError::NotFound)
    }
}

/// Validates a decoded request path segment by segment and returns it as a relative path.
fn sanitize_relative_path(request_path: &str) -> Result<PathBuf, PageError> {
    let mut relative = PathBuf::new();
    for segment in request_path.split('/') {
        // Empty segments come from a leading, doubled or trailing slash and carry no meaning.
        if segment.is_empty() {
            continue;
        }
        if segment == "." || segment == ".." {
            return Err(PageError::Invalid(
                "relative segments ('.' or '..') are not allowed".to_string(),
            ));
        }
        // A backslash is a separator on Windows and a colon starts a drive or stream name there;
        // neither belongs in a portable page path.
        if segment.contains(['\\', '\0', ':']) {
            return Err(PageError::Invalid(format!(
                "segment '{}' contains a forbidden character",
                segment.escape_debug()
            )));
        }
        if segment.starts_with('.') {
            return Err(PageError::NotFound);
        }
        relative.push(segment);
    }
    Ok(relative)
}

/// `Content-Type` for a served page file, with an explicit charset for text.
///
/// Plugin pages are authored as UTF-8; naming the charset keeps a browser from guessing one.
pub fn page_content_type(path: &Path) -> String {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let essence = mime.essence_str();
    if mime.type_() == mime_guess::mime::TEXT
        || essence == "application/javascript"
        || essence == "application/json"
        || essence == "image/svg+xml"
    {
        format!("{essence}; charset=utf-8")
    } else {
        essence.to_string()
    }
}

/// Translations a plugin ships, keyed by locale tag (`zh-CN`, `en`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Translations {
    /// Usable texts per locale, each a flat map of key → text.
    pub locales: BTreeMap<String, BTreeMap<String, String>>,
    /// Problems found while reading, one readable sentence each (file and reason).
    pub errors: Vec<String>,
}

/// Reads `<plugin_dir>/i18n/*.json`.
///
/// A missing `i18n/` directory is the normal case and yields empty translations. Files that are
/// not `.json` are ignored (a folder may carry a README); hidden files are ignored as well.
/// Everything else that cannot be used is reported in [`Translations::errors`]; within a usable
/// file, unknown keys and non-string values are reported while the valid keys are kept.
pub fn load_translations(plugin_dir: &Path) -> Translations {
    let mut translations = Translations::default();
    let dir = plugin_dir.join(I18N_DIR);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return translations,
        Err(err) => {
            translations
                .errors
                .push(format!("{I18N_DIR}/ cannot be read: {err}"));
            return translations;
        }
    };

    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "json")
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        })
        .collect();
    // Sorted so the reported errors and the truncation below are deterministic.
    files.sort();

    if files.len() > MAX_TRANSLATION_FILES {
        translations.errors.push(format!(
            "{I18N_DIR}/ holds {} translation files; only the first {MAX_TRANSLATION_FILES} are read",
            files.len()
        ));
        files.truncate(MAX_TRANSLATION_FILES);
    }

    for path in files {
        let file_label = format!(
            "{I18N_DIR}/{}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        let locale = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_default();
        if !is_locale_tag(&locale) {
            translations.errors.push(format!(
                "{file_label}: '{locale}' is not a locale tag such as 'en' or 'zh-CN'"
            ));
            continue;
        }
        match read_translation_file(&path) {
            Ok((texts, problems)) => {
                translations.errors.extend(
                    problems
                        .into_iter()
                        .map(|problem| format!("{file_label}: {problem}")),
                );
                translations.locales.insert(locale, texts);
            }
            Err(reason) => translations.errors.push(format!("{file_label}: {reason}")),
        }
    }

    translations
}

/// Parses one translation file into its usable texts plus per-key problems.
fn read_translation_file(path: &Path) -> Result<(BTreeMap<String, String>, Vec<String>), String> {
    let metadata = std::fs::metadata(path).map_err(|err| format!("cannot be read: {err}"))?;
    if metadata.len() > MAX_TRANSLATION_BYTES {
        return Err(format!(
            "is {} bytes; translation files are limited to {MAX_TRANSLATION_BYTES} bytes",
            metadata.len()
        ));
    }
    let raw = std::fs::read_to_string(path).map_err(|err| format!("cannot be read: {err}"))?;
    let document: serde_json::Value =
        serde_json::from_str(&raw).map_err(|err| format!("is not valid JSON: {err}"))?;
    let serde_json::Value::Object(map) = document else {
        return Err("must be a JSON object of strings".to_string());
    };

    let mut texts = BTreeMap::new();
    let mut problems = Vec::new();
    for (key, value) in map {
        if !translation_key_is_known(&key) {
            problems.push(format!("unknown key '{key}'"));
            continue;
        }
        match value {
            serde_json::Value::String(text) => {
                texts.insert(key, text);
            }
            _ => problems.push(format!("value of '{key}' must be a string")),
        }
    }
    Ok((texts, problems))
}

/// Whether `key` is one of the documented translation key shapes.
///
/// - `name`, `description` — the plugin's display name and summary;
/// - `config.<field path>.title`, `config.<field path>.description` — a configuration field's
///   label and hint, the path being the property names from the schema root joined with `.`;
/// - `commands.<name>.description` — a command's help text.
pub fn translation_key_is_known(key: &str) -> bool {
    if key == "name" || key == "description" {
        return true;
    }
    if let Some(rest) = key.strip_prefix("config.") {
        return ["title", "description"].iter().any(|suffix| {
            rest.strip_suffix(suffix)
                .and_then(|path| path.strip_suffix('.'))
                .is_some_and(|path| !path.is_empty() && path.split('.').all(|p| !p.is_empty()))
        });
    }
    if let Some(rest) = key.strip_prefix("commands.") {
        return rest
            .strip_suffix(".description")
            .is_some_and(|name| !name.is_empty() && !name.contains('.'));
    }
    false
}

/// Whether `tag` looks like a BCP 47 locale tag (`en`, `zh-CN`, `pt_BR` is tolerated).
fn is_locale_tag(tag: &str) -> bool {
    (2..=35).contains(&tag.len())
        && tag.starts_with(|c: char| c.is_ascii_alphabetic())
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}
