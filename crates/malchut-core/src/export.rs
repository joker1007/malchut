//! PDF と PNG への書き出し。

use std::path::Path;

use crate::layout::{DisplayList, Layouter};
use crate::model::{Deck, LinkTarget};
use crate::paint::{paint, render_image};
use crate::theme::Theme;

/// PDF のページは論理座標の半分の大きさ (16:9 なら 960×540pt) にする。
const PDF_SCALE: f64 = 0.5;

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("描画に失敗しました: {0}")]
    Cairo(#[from] cairo::Error),
    #[error("書き出しに失敗しました: {0}")]
    Io(#[from] cairo::IoError),
    #[error("ファイルを作成できません: {0}")]
    File(#[from] std::io::Error),
}

/// 全スライドをレイアウトする。画像の相対パスは `base_dir` から解決する。
pub fn layout_deck(deck: &Deck, theme: &Theme, base_dir: &Path) -> Vec<DisplayList> {
    let layouter = Layouter::new();
    deck.slides
        .iter()
        .enumerate()
        .map(|(index, slide)| {
            let style = theme.style(&slide.props.class);
            layouter.layout(slide, &style, deck.meta.size, index + 1, base_dir)
        })
        .collect()
}

/// 1 スライド 1 ページの PDF を書き出す。
pub fn export_pdf(
    deck: &Deck,
    theme: &Theme,
    base_dir: &Path,
    path: &Path,
) -> Result<(), ExportError> {
    let (width, height) = deck.meta.size.logical();
    let surface = cairo::PdfSurface::new(width * PDF_SCALE, height * PDF_SCALE, path)?;
    let cr = cairo::Context::new(&surface)?;
    cr.scale(PDF_SCALE, PDF_SCALE);
    for list in layout_deck(deck, theme, base_dir) {
        paint(&cr, &list)?;
        // リンクの範囲を PDF の注釈として埋め込む。
        for link in &list.links {
            let target = match deck.link_target(&link.url) {
                Some(LinkTarget::Slide(index)) => format!("page={}", index + 1),
                Some(LinkTarget::External(url)) => format!("uri='{}'", escape_attribute(url)),
                None => continue,
            };
            // rect には描画時の拡大率が適用されないので、ここでページの座標に直す。
            let attributes = format!(
                "rect=[{} {} {} {}] {target}",
                link.x * PDF_SCALE,
                link.y * PDF_SCALE,
                link.width * PDF_SCALE,
                link.height * PDF_SCALE
            );
            cr.tag_begin(cairo::CAIRO_TAG_LINK, &attributes);
            cr.tag_end(cairo::CAIRO_TAG_LINK);
        }
        cr.show_page()?;
    }
    drop(cr);
    surface.finish();
    surface.status()?;
    Ok(())
}

/// Cairo のタグ属性の文字列値として使えるよう、引用符とバックスラッシュをエスケープする。
fn escape_attribute(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

/// スライドごとに `<dir>/slide-NNN.png` を書き出す。
pub fn export_png(
    deck: &Deck,
    theme: &Theme,
    base_dir: &Path,
    dir: &Path,
    scale: f64,
) -> Result<(), ExportError> {
    std::fs::create_dir_all(dir)?;
    for (index, list) in layout_deck(deck, theme, base_dir).iter().enumerate() {
        let surface = render_image(list, scale)?;
        let mut file = std::fs::File::create(dir.join(format!("slide-{:03}.png", index + 1)))?;
        surface.write_to_png(&mut file)?;
    }
    Ok(())
}
