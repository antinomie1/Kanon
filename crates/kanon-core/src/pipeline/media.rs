//! Inbound images fetched by the node before the model sees them.
//!
//! Platform image URLs are signed and short-lived (QQ's `rkey`), and a quoted picture from an
//! older message often arrives already expired. Handed to the provider as a URL, a picture it
//! cannot download makes it reject the whole request, so the user gets no answer at all, not even
//! to the words beside the picture. The node downloads each image itself and sends the bytes
//! inline instead. A picture that cannot be downloaded is left out, and the message text says so,
//! so the model answers what it can and knows what it could not see.

use std::sync::LazyLock;
use std::time::Duration;

use base64::Engine;
use futures_util::future::join_all;
use kanon_llm::{ChatMessage, ContentPart};

/// Largest image downloaded for the model.
///
/// Well above what a chat platform delivers as a picture, and inside what providers accept for
/// one inline image.
pub const MAX_INBOUND_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// How long one image may take to download, body included.
///
/// The model call waits for every download, so a stalled CDN must not hold the turn for long.
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Shared so downloads reuse connections to the platform's CDN across turns.
static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(reqwest::Client::new);

/// Replaces every remote image of `message` with its downloaded bytes.
///
/// Images are downloaded concurrently. One that fails is dropped from the parts, logged, and
/// named in the message text with the reason. A message with no text (an image-only endpoint,
/// see [`super::build_user_message`]) gets no note, because such an endpoint rejects text; there
/// the failure is only logged. Local files and inline data are left as they are: the provider
/// layer already sends those as bytes.
pub async fn inline_images(message: &mut ChatMessage) {
    let Some(parts) = message.parts.take() else {
        return;
    };
    let mut kept = Vec::with_capacity(parts.len());
    let mut failures = Vec::new();
    for result in join_all(parts.into_iter().map(inline_part)).await {
        match result {
            Ok(part) => kept.push(part),
            Err(reason) => failures.push(reason),
        }
    }

    if let Some(content) = message.content.as_mut().filter(|text| !text.is_empty()) {
        for reason in &failures {
            content.push_str(&format!("\n[有一张图片无法下载，你看不到它：{reason}]"));
        }
    }
    message.parts = (!kept.is_empty()).then_some(kept);
}

/// Downloads one part's image when it is a remote URL; returns every other part unchanged.
async fn inline_part(part: ContentPart) -> Result<ContentPart, String> {
    let ContentPart::Image {
        url: Some(url),
        mime_type,
        ..
    } = &part
    else {
        return Ok(part);
    };
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Ok(part);
    }

    let (bytes, mime) = fetch_image(url, mime_type.as_deref()).await.map_err(|reason| {
        // The URL itself is left out: its query string is the platform's signature.
        let host = reqwest::Url::parse(url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
            .unwrap_or_default();
        tracing::warn!(host = %host, reason = %reason, "Inbound image could not be downloaded; the model gets the text only");
        reason
    })?;
    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(ContentPart::image_url(
        format!("data:{mime};base64,{data}"),
        Some(mime),
    ))
}

/// Downloads an image and returns its bytes with their MIME type.
///
/// The type comes from the bytes when they are a common image format, because CDNs label
/// pictures loosely, then from an `image/*` response header, then from the one the platform
/// declared. Anything else is refused: an error page would reach the model as a broken image.
async fn fetch_image(url: &str, declared: Option<&str>) -> Result<(Vec<u8>, String), String> {
    // The reasons below reach the model and the log; `without_url` keeps the signed URL out.
    let mut response = CLIENT
        .get(url)
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .map_err(|err| describe(err.without_url()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("the link answered HTTP {status}"));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_INBOUND_IMAGE_BYTES as u64)
    {
        return Err(too_large());
    }
    let header = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_string()
        });

    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| describe(err.without_url()))?
    {
        if bytes.len() + chunk.len() > MAX_INBOUND_IMAGE_BYTES {
            return Err(too_large());
        }
        bytes.extend_from_slice(&chunk);
    }

    let is_image = |mime: &&str| mime.starts_with("image/");
    let mime = sniff(&bytes)
        .map(str::to_string)
        .or_else(|| header.as_deref().filter(is_image).map(str::to_string))
        .or_else(|| declared.filter(is_image).map(str::to_string))
        .ok_or_else(|| {
            format!(
                "the link returned {} instead of an image",
                header.as_deref().unwrap_or("data of no declared type")
            )
        })?;
    Ok((bytes, mime))
}

/// Recognizes the image formats every vision provider accepts by their leading bytes.
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => Some("image/webp"),
        _ => None,
    }
}

fn too_large() -> String {
    format!(
        "the image is larger than the {} MiB Kanon downloads",
        MAX_INBOUND_IMAGE_BYTES >> 20
    )
}

/// A transport failure in a few words.
fn describe(err: reqwest::Error) -> String {
    if err.is_timeout() {
        format!(
            "the download took longer than {} s",
            FETCH_TIMEOUT.as_secs()
        )
    } else {
        format!("the download failed: {err}")
    }
}
