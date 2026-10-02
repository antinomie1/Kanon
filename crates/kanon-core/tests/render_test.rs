//! Rendering text cards and SVG into PNGs for plugins.

use kanon_core::ipc::CoreApiService;
use kanon_core::render::{RenderError, render_svg, render_text};
use kanon_proto::v1::bot_api_service_server::BotApiService;
use kanon_proto::v1::{RenderImageRequest, render_image_request::Source};
use resvg::tiny_skia::Pixmap;
use tonic::{Code, Request};

fn pixel(png: &[u8], x: u32, y: u32) -> (u8, u8, u8, u8) {
    let pixmap = Pixmap::decode_png(png).expect("valid png");
    let color = pixmap.pixel(x, y).expect("inside");
    (color.red(), color.green(), color.blue(), color.alpha())
}

#[test]
fn svg_is_rendered_at_its_own_size_and_never_reads_the_nodes_files() {
    let red = render_svg(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect width="100" height="50" fill="#ff0000"/></svg>"##,
    )
    .expect("render");
    assert_eq!((red.width, red.height), (100, 50));
    assert_eq!(pixel(&red.png, 10, 10), (255, 0, 0, 255));

    // An `<image>` pointing at a file on the node is ignored, not read into the picture.
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("secret.png");
    std::fs::write(&secret, &red.png).unwrap();
    let probe = render_svg(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="20" height="20"><image xlink:href="{}" width="20" height="20"/></svg>"#,
        secret.display()
    ))
    .expect("render");
    assert_eq!(pixel(&probe.png, 10, 10).3, 0, "nothing was drawn");

    assert!(matches!(render_svg("<svg"), Err(RenderError::Svg(_))));
}

#[test]
fn text_cards_wrap_to_the_width_and_grow_with_the_text() {
    let short = match render_text("# 天气\n晴，25°C", 720) {
        Err(RenderError::NoFonts) => return, // a node without fonts says so instead
        other => other.expect("render"),
    };
    assert_eq!(short.width, 720);

    let long_line =
        "The quick brown fox jumps over the lazy dog. 敏捷的棕色狐狸跳过了懒狗。".repeat(8);
    let long = render_text(&long_line, 720).expect("render");
    assert_eq!(
        long.width, 720,
        "long lines wrap instead of widening the card"
    );
    assert!(
        long.height > short.height * 2,
        "{} vs {}",
        long.height,
        short.height
    );

    // Something was actually drawn on the white card.
    let pixmap = Pixmap::decode_png(&long.png).unwrap();
    assert!(
        pixmap.pixels().iter().any(|p| p.red() < 128),
        "text is visible"
    );

    assert!(matches!(
        render_text("hi", 100),
        Err(RenderError::Invalid(_))
    ));
    assert!(matches!(
        render_text("  \n", 720),
        Err(RenderError::Invalid(_))
    ));
}

#[tokio::test]
async fn plugins_get_a_png_in_their_own_directory() {
    let dir = tempfile::tempdir().unwrap();
    let service = CoreApiService::new(tokio::sync::mpsc::channel(1).0)
        .with_plugin_data_dir(kanon_storage::PluginDataDir::new(dir.path()));
    let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><rect width="8" height="8"/></svg>"#;
    let request = || RenderImageRequest {
        plugin_id: "weather".to_string(),
        source: Some(Source::Svg(svg.to_string())),
        width: 0,
    };

    let first = service
        .render_image(Request::new(request()))
        .await
        .expect("render")
        .into_inner();
    let path = std::path::Path::new(&first.file_path);
    assert!(path.is_absolute());
    assert!(path.starts_with(dir.path().join("weather").join("render")));
    assert!(path.is_file());
    assert_eq!((first.width, first.height), (8, 8));

    let again = service
        .render_image(Request::new(request()))
        .await
        .expect("render")
        .into_inner();
    assert_eq!(
        again.file_path, first.file_path,
        "the same image is one file"
    );

    let err = service
        .render_image(Request::new(RenderImageRequest {
            plugin_id: "../escape".to_string(),
            ..request()
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    let err = service
        .render_image(Request::new(RenderImageRequest {
            source: Some(Source::Text("hi".to_string())),
            width: 50_000,
            ..request()
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
}
