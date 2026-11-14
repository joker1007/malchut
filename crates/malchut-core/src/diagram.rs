//! Mermaid 図を SVG にして、Cairo で描ける形で保持する。

use std::fmt::Write as _;

use merman::svg::SvgPipeline;
use merman::{OperationControl, RenderOutput, RenderRequest, Renderer, SvgRequest};

use crate::theme::{Color, Style};

/// 読み込み済みの SVG。
pub struct SvgImage {
    handle: rsvg::SvgHandle,
    pub width: f64,
    pub height: f64,
}

impl std::fmt::Debug for SvgImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SvgImage({}x{})", self.width, self.height)
    }
}

impl SvgImage {
    pub fn from_svg(svg: &str) -> Result<SvgImage, String> {
        let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from(svg.as_bytes()));
        let handle = rsvg::Loader::new()
            .read_stream(&stream, None::<&gio::File>, None::<&gio::Cancellable>)
            .map_err(|err| err.to_string())?;
        Self::from_handle(handle)
    }

    pub fn from_file(path: &std::path::Path) -> Result<SvgImage, String> {
        let handle = rsvg::Loader::new()
            .read_path(path)
            .map_err(|err| err.to_string())?;
        Self::from_handle(handle)
    }

    fn from_handle(handle: rsvg::SvgHandle) -> Result<SvgImage, String> {
        let renderer = rsvg::CairoRenderer::new(&handle);
        let (width, height) = match renderer.intrinsic_size_in_pixels() {
            Some(size) => size,
            None => {
                let view_box = renderer
                    .intrinsic_dimensions()
                    .vbox
                    .ok_or("SVG の大きさが分かりません")?;
                (view_box.width(), view_box.height())
            }
        };
        if width <= 0.0 || height <= 0.0 {
            return Err("SVG の大きさが 0 です".to_owned());
        }
        Ok(SvgImage {
            handle,
            width,
            height,
        })
    }

    /// 指定の矩形いっぱいに描く。
    pub fn paint(&self, cr: &cairo::Context, x: f64, y: f64, width: f64, height: f64) {
        let viewport = cairo::Rectangle::new(x, y, width, height);
        // 描画に失敗した図は空白のままにする。
        let _ = rsvg::CairoRenderer::new(&self.handle).render_document(cr, &viewport);
    }
}

/// Mermaid のソースを、スライドの配色に合わせた SVG にする。
pub fn render_mermaid(source: &str, style: &Style) -> Result<SvgImage, String> {
    // ソース自身が init ディレクティブを持つ場合は、そちらを優先する。
    let source = if source.trim_start().starts_with("%%{") {
        source.to_owned()
    } else {
        format!("{}\n{source}", init_directive(style))
    };
    let request = SvgRequest {
        // librsvg は foreignObject を描けないので、SVG のテキストで出力させる。
        pipeline: Some(SvgPipeline::resvg_safe()),
        ..Default::default()
    };
    let output = Renderer::new()
        .render(RenderRequest::svg(&source, OperationControl::new(), request))
        .map_err(|err| err.to_string())?;
    let RenderOutput::Svg(Some(svg)) = output else {
        return Err("Mermaid の図として解釈できません".to_owned());
    };
    SvgImage::from_svg(svg.svg())
}

/// テーマの配色とフォントを Mermaid のテーマ変数として渡す。
fn init_directive(style: &Style) -> String {
    let palette = &style.palette;
    let dark = luminance(palette.background) < 0.5;
    let variables = [
        ("background", palette.background.to_hex()),
        ("primaryColor", palette.code_background.to_hex()),
        ("primaryTextColor", palette.foreground.to_hex()),
        ("primaryBorderColor", palette.muted.to_hex()),
        ("secondaryColor", palette.background.to_hex()),
        ("tertiaryColor", palette.background.to_hex()),
        ("lineColor", palette.foreground.to_hex()),
        ("textColor", palette.foreground.to_hex()),
        ("noteBkgColor", palette.line_highlight.to_hex()),
        ("noteTextColor", palette.foreground.to_hex()),
        ("fontFamily", style.fonts.body.replace(['"', '\\'], "")),
    ];
    let mut json = format!("%%{{init: {{\"theme\": \"base\", \"themeVariables\": {{\"darkMode\": {dark}");
    for (key, value) in variables {
        let _ = write!(json, ", \"{key}\": \"{value}\"");
    }
    json.push_str("}}}%%");
    json
}

fn luminance(color: Color) -> f64 {
    0.2126 * color.r + 0.7152 * color.g + 0.0722 * color.b
}
