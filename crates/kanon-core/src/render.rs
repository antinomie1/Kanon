//! Rendering text and SVG into PNG images that plugins send as pictures.
//!
//! Text is laid out as a simple card: wrapped to the requested width, a blank line separates
//! paragraphs and a line starting with `# ` is a heading. The layout is turned into SVG and
//! rendered by resvg with the system's fonts; characters the first font lacks (CJK, emoji) are
//! taken from whichever installed font has them. An SVG document is rendered as it is.
//!
//! Wrapping uses a conservative per-character width estimate rather than real glyph metrics, and
//! the card is then widened to the real extent of the rendered text, so an estimate that was
//! too small never clips a line.
//!
//! Rendering is CPU work: callers on the async runtime run it on the blocking pool.

use std::sync::{Arc, OnceLock};

use resvg::{tiny_skia, usvg};

/// Default card width for text, in pixels.
pub const DEFAULT_TEXT_WIDTH: u32 = 720;
/// Accepted card widths for text.
pub const TEXT_WIDTH_RANGE: std::ops::RangeInclusive<u32> = 200..=2000;
/// Longest text accepted, in characters.
pub const MAX_TEXT_CHARS: usize = 20_000;
/// Largest image produced, in pixels (64 MiB of RGBA), so one request cannot exhaust memory.
pub const MAX_PIXELS: u64 = 16 * 1024 * 1024;

const PADDING: f32 = 32.0;
const BODY_SIZE: f32 = 22.0;
const HEADING_SIZE: f32 = 30.0;
const LINE_HEIGHT: f32 = 1.6;
const BACKGROUND: tiny_skia::Color = tiny_skia::Color::WHITE;
const TEXT_COLOR: &str = "#1f2328";
const FONT_FAMILY: &str = "sans-serif";
/// Installed families tried, in order, for `sans-serif`.
const SANS_SERIF_PREFERENCE: &[&str] = &[
    "Noto Sans CJK SC",
    "Source Han Sans SC",
    "PingFang SC",
    "Microsoft YaHei",
    "WenQuanYi Micro Hei",
    "Noto Sans",
    "DejaVu Sans",
    "Liberation Sans",
    "Arial",
    "Helvetica",
];

/// Why an image could not be rendered.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// The request is malformed or too large.
    #[error("{0}")]
    Invalid(String),
    /// The SVG could not be parsed.
    #[error("invalid SVG: {0}")]
    Svg(String),
    /// The node has no fonts installed, so text would come out blank.
    #[error("no fonts are installed on this node; install a font package to render text")]
    NoFonts,
    /// Encoding the PNG failed.
    #[error("PNG encoding failed: {0}")]
    Encode(String),
}

/// A rendered PNG.
#[derive(Debug, Clone)]
pub struct RenderedImage {
    /// PNG bytes.
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Renders text as a card `width` pixels wide (see the module documentation).
pub fn render_text(text: &str, width: u32) -> Result<RenderedImage, RenderError> {
    if !TEXT_WIDTH_RANGE.contains(&width) {
        return Err(RenderError::Invalid(format!(
            "width must be within {}..={}, got {width}",
            TEXT_WIDTH_RANGE.start(),
            TEXT_WIDTH_RANGE.end()
        )));
    }
    if text.trim().is_empty() {
        return Err(RenderError::Invalid("text must not be empty".to_string()));
    }
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(RenderError::Invalid(format!(
            "text must be at most {MAX_TEXT_CHARS} characters"
        )));
    }
    let fonts = system_fonts();
    if fonts.is_empty() {
        return Err(RenderError::NoFonts);
    }

    let lines = layout(text, width as f32 - 2.0 * PADDING);
    let height = lines.last().map_or(PADDING, |line| line.bottom) + PADDING;
    let mut svg_width = width as f32;
    let mut tree = parse(&card_svg(&lines, svg_width, height), fonts.clone())?;
    // The estimate is conservative, but a font with unusually wide glyphs could still overrun:
    // widen the card to what was actually drawn rather than clip it.
    let drawn = tree.root().abs_bounding_box().right() + PADDING;
    if drawn > svg_width {
        svg_width = drawn.ceil();
        tree = parse(&card_svg(&lines, svg_width, height), fonts)?;
    }
    rasterize(&tree, Some(BACKGROUND))
}

/// Renders an SVG document at its own size; what it leaves unpainted stays transparent.
pub fn render_svg(svg: &str) -> Result<RenderedImage, RenderError> {
    if svg.trim().is_empty() {
        return Err(RenderError::Invalid("svg must not be empty".to_string()));
    }
    let tree = parse(svg, system_fonts())?;
    rasterize(&tree, None)
}

/// One laid-out line of the card.
struct Line {
    text: String,
    size: f32,
    bold: bool,
    /// Baseline position.
    baseline: f32,
    /// Bottom edge of the line box.
    bottom: f32,
}

/// Wraps text into lines no wider (by estimate) than `available` pixels.
fn layout(text: &str, available: f32) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut top = PADDING;
    let mut previous_blank = false;
    for raw in text.trim_matches('\n').lines() {
        let raw = raw.trim_end();
        if raw.trim().is_empty() {
            // A paragraph break is half a line; runs of blank lines collapse into one.
            if !previous_blank {
                top += BODY_SIZE * LINE_HEIGHT / 2.0;
            }
            previous_blank = true;
            continue;
        }
        previous_blank = false;
        let (content, size, bold) = match raw.strip_prefix("# ") {
            Some(heading) => (heading.trim(), HEADING_SIZE, true),
            None => (raw, BODY_SIZE, false),
        };
        for wrapped in wrap(content, available, size) {
            let line_height = size * LINE_HEIGHT;
            // Centre the glyphs in the line box: the baseline sits about 0.8 em below the top of
            // a font's em box, plus half the leading.
            let baseline = top + (line_height - size) / 2.0 + size * 0.8;
            top += line_height;
            lines.push(Line {
                text: wrapped,
                size,
                bold,
                baseline,
                bottom: top,
            });
        }
    }
    lines
}

/// Greedy wrapping: Latin words stay whole when they fit, CJK breaks between any characters.
fn wrap(text: &str, available: f32, size: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0.0;
    for token in tokens(text) {
        let token_width: f32 = token.chars().map(|c| char_width(c, size)).sum();
        // A word longer than a whole line is broken between characters, starting right where
        // the line is, instead of first leaving a short line behind.
        let breaks = token_width > available;
        if !breaks && current_width + token_width > available && !current.is_empty() {
            lines.push(std::mem::take(&mut current).trim_end().to_string());
            current_width = 0.0;
            if token.trim().is_empty() {
                continue; // no leading space on a wrapped line
            }
        }
        if breaks {
            for c in token.chars() {
                let width = char_width(c, size);
                if current_width + width > available && !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                    current_width = 0.0;
                }
                current.push(c);
                current_width += width;
            }
        } else {
            current.push_str(token);
            current_width += token_width;
        }
    }
    if !current.trim().is_empty() {
        lines.push(current.trim_end().to_string());
    }
    lines
}

/// Splits text into wrap units: runs of narrow non-space characters, single wide characters,
/// and single spaces.
fn tokens(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start = None;
    for (index, c) in text.char_indices() {
        let narrow_word = !c.is_whitespace() && !is_wide(c);
        match (start, narrow_word) {
            (None, true) => start = Some(index),
            (Some(_), true) => {}
            (Some(begin), false) => {
                tokens.push(&text[begin..index]);
                start = None;
                tokens.push(&text[index..index + c.len_utf8()]);
            }
            (None, false) => tokens.push(&text[index..index + c.len_utf8()]),
        }
    }
    if let Some(begin) = start {
        tokens.push(&text[begin..]);
    }
    tokens
}

/// Estimated advance of `c`: a full em for wide characters, 0.55 em otherwise (a little wider
/// than the average of proportional Latin fonts, so lines rarely overrun).
fn char_width(c: char, size: f32) -> f32 {
    if is_wide(c) { size } else { size * 0.55 }
}

/// CJK, full-width forms and emoji take a full em.
fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1FAFF | 0x20000..=0x3FFFD)
}

/// The card as an SVG document.
fn card_svg(lines: &[Line], width: f32, height: f32) -> String {
    let mut svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">"#
    );
    for line in lines {
        svg.push_str(&format!(
            r#"<text x="{PADDING}" y="{}" font-family="{FONT_FAMILY}" font-size="{}" font-weight="{}" fill="{TEXT_COLOR}" xml:space="preserve">{}</text>"#,
            line.baseline,
            line.size,
            if line.bold { "bold" } else { "normal" },
            escape(&line.text)
        ));
    }
    svg.push_str("</svg>");
    svg
}

fn escape(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).fold(
        String::with_capacity(text.len()),
        |mut out, c| {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                _ => out.push(c),
            }
            out
        },
    )
}

fn parse(svg: &str, fonts: Arc<usvg::fontdb::Database>) -> Result<usvg::Tree, RenderError> {
    let options = usvg::Options {
        fontdb: fonts,
        font_family: FONT_FAMILY.to_string(),
        // Embedded `data:` images are fine; anything else in `href` would be read from the
        // node's disk, so it is ignored.
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_string: Box::new(|_, _| None),
            ..usvg::ImageHrefResolver::default()
        },
        ..usvg::Options::default()
    };
    usvg::Tree::from_str(svg, &options).map_err(|err| RenderError::Svg(err.to_string()))
}

fn rasterize(
    tree: &usvg::Tree,
    background: Option<tiny_skia::Color>,
) -> Result<RenderedImage, RenderError> {
    let size = tree.size().to_int_size();
    let (width, height) = (size.width(), size.height());
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(RenderError::Invalid(format!(
            "a {width}x{height} image exceeds the {MAX_PIXELS}-pixel limit"
        )));
    }
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| RenderError::Invalid(format!("cannot render a {width}x{height} image")))?;
    if let Some(background) = background {
        pixmap.fill(background);
    }
    resvg::render(tree, tiny_skia::Transform::identity(), &mut pixmap.as_mut());
    let png = pixmap
        .encode_png()
        .map_err(|err| RenderError::Encode(err.to_string()))?;
    Ok(RenderedImage { png, width, height })
}

/// The system's fonts, loaded once on first use (scanning font directories takes a while, and a
/// node that never renders should not pay for it).
fn system_fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut fonts = usvg::fontdb::Database::new();
            fonts.load_system_fonts();
            // fontdb maps the generic `sans-serif` to Arial, which most Linux hosts lack, and
            // text whose primary font is missing is dropped rather than falling back. Point it
            // at a sans-serif face that is installed (CJK-capable first, so Chinese text does
            // not mix fonts mid-line); with none of these, any installed face.
            let installed = |family: &str| {
                fonts
                    .faces()
                    .any(|face| face.families.iter().any(|(name, _)| name == family))
            };
            let family = SANS_SERIF_PREFERENCE
                .iter()
                .find(|family| installed(family))
                .map(|family| family.to_string())
                .or_else(|| {
                    fonts
                        .faces()
                        .next()
                        .and_then(|face| face.families.first())
                        .map(|(name, _)| name.clone())
                });
            if let Some(family) = family {
                fonts.set_sans_serif_family(family);
            }
            tracing::debug!(faces = fonts.len(), "System fonts loaded for rendering");
            Arc::new(fonts)
        })
        .clone()
}
