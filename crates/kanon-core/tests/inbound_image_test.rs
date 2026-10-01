//! Inbound images reach the model as bytes the node downloaded, and a picture that cannot be
//! downloaded costs the model that picture, never the whole turn.

use axum::Router;
use axum::http::{StatusCode, header};
use axum::routing::get;
use kanon_core::pipeline::{MAX_INBOUND_IMAGE_BYTES, inline_images};
use kanon_llm::{ChatMessage, ContentPart};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n picture";

/// Serves a picture, an expired link, an error page, an oversized picture and a JPEG labelled as
/// arbitrary bytes, the way CDNs sometimes label pictures.
async fn spawn_cdn() -> String {
    let app = Router::new()
        .route(
            "/pic",
            get(|| async { ([(header::CONTENT_TYPE, "image/png")], PNG) }),
        )
        .route("/expired", get(|| async { StatusCode::BAD_REQUEST }))
        .route(
            "/page",
            get(|| async { ([(header::CONTENT_TYPE, "text/html")], "<html>login</html>") }),
        )
        .route(
            "/huge",
            get(|| async { vec![0xFFu8; MAX_INBOUND_IMAGE_BYTES + 1] }),
        )
        .route(
            "/unlabelled",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/octet-stream")],
                    b"\xFF\xD8\xFF\xE0 jpeg".to_vec(),
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn image_urls(message: &ChatMessage) -> Vec<String> {
    message
        .parts
        .iter()
        .flatten()
        .filter_map(|part| match part {
            ContentPart::Image { url, .. } => url.clone(),
            ContentPart::Text { .. } => None,
        })
        .collect()
}

#[tokio::test]
async fn downloaded_pictures_are_sent_inline_and_the_rest_are_named_in_the_text() {
    let cdn = spawn_cdn().await;
    let mut message = ChatMessage::user_multimodal(
        "[图片] [图片] [图片] [图片] [图片] what are these",
        ["pic", "expired", "page", "huge", "unlabelled"]
            .iter()
            .map(|path| ContentPart::image_url(format!("{cdn}/{path}?rkey=signed"), None))
            .collect(),
    );

    inline_images(&mut message).await;

    let png = format!(
        "data:image/png;base64,{}",
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, PNG)
    );
    let urls = image_urls(&message);
    assert_eq!(urls.len(), 2, "{urls:?}");
    assert_eq!(urls[0], png);
    assert!(
        urls[1].starts_with("data:image/jpeg;base64,"),
        "{}",
        urls[1]
    );

    let text = message.content.as_deref().unwrap();
    assert!(text.starts_with("[图片] [图片] [图片] [图片] [图片] what are these"));
    for reason in ["HTTP 400", "text/html", "larger than the 10 MiB"] {
        assert!(text.contains(reason), "{reason}: {text}");
    }
    // The signed link stays out of what the model reads.
    assert!(!text.contains("rkey"), "{text}");
}

#[tokio::test]
async fn a_turn_whose_pictures_all_fail_keeps_its_words_and_loses_its_parts() {
    let cdn = spawn_cdn().await;
    let local = ContentPart::image_file("/tmp/not-fetched.png", None);
    let mut message = ChatMessage::user_multimodal(
        "[图片] hello",
        vec![ContentPart::image_url(format!("{cdn}/expired"), None)],
    );
    inline_images(&mut message).await;
    assert!(!message.has_parts());
    assert!(message.content.as_deref().unwrap().contains("HTTP 400"));

    // Local files are already sent as bytes by the provider layer; nothing is downloaded.
    let mut message = ChatMessage::user_multimodal("[图片]", vec![local.clone()]);
    inline_images(&mut message).await;
    assert_eq!(message.parts, Some(vec![local]));
    assert_eq!(message.content.as_deref(), Some("[图片]"));
}
