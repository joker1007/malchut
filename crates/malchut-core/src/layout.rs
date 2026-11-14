//! [`Slide`] を論理座標上の描画命令列 [`DisplayList`] に変換する。

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

use pango::prelude::*;

use crate::diagram::{self, SvgImage};
use crate::highlight;
use crate::model::*;
use crate::picture::Picture;
use crate::theme::{Color, Style, VAlign};

const UNITS: f64 = pango::SCALE as f64;

/// 画像のキャッシュがこの件数を超えたら捨てる。
const PICTURE_CACHE_LIMIT: usize = 256;

/// 本文の文字サイズに対する、キャプションの文字サイズの比。
const CAPTION_SCALE: f64 = 0.6;

/// 読み込めなかった画像の代わりに確保する大きさ。
const MISSING_IMAGE_SIZE: (f64, f64) = (640.0, 360.0);

/// Mermaid が図を組むときの既定の文字サイズ (px)。
const DIAGRAM_BASE_FONT_SIZE: f64 = 16.0;

#[derive(Debug)]
pub struct DisplayList {
    pub width: f64,
    pub height: f64,
    pub items: Vec<Item>,
    /// 内容が枠に収まっていない。
    pub overflow: bool,
    /// このスライドが参照している画像ファイル。
    pub assets: Vec<PathBuf>,
    /// クリックできるリンクの範囲。
    pub links: Vec<LinkArea>,
}

/// リンクの文字が占める矩形 (論理座標) とリンク先。折り返したリンクは行ごとに 1 つ。
#[derive(Debug, Clone, PartialEq)]
pub struct LinkArea {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub url: String,
}

impl DisplayList {
    /// 指定の位置 (論理座標) にあるリンクのリンク先。
    pub fn link_at(&self, x: f64, y: f64) -> Option<&str> {
        self.links
            .iter()
            .find(|link| {
                (link.x..=link.x + link.width).contains(&x)
                    && (link.y..=link.y + link.height).contains(&y)
            })
            .map(|link| link.url.as_str())
    }
}

/// マークアップを取り除いたテキスト上での、リンクのバイト範囲。
struct LinkSpan {
    start: usize,
    end: usize,
    url: String,
}

#[derive(Debug)]
pub enum Item {
    Rect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        radius: f64,
        color: Color,
    },
    Text {
        x: f64,
        y: f64,
        layout: pango::Layout,
        /// マークアップで色指定されていない部分の色。
        color: Color,
    },
    Svg {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        image: Rc<SvgImage>,
    },
    Image {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        picture: Rc<Picture>,
        /// 元画像のうち描く範囲 (x, y, 幅, 高さ)。
        source: (f64, f64, f64, f64),
        radius: f64,
        shadow: bool,
        opacity: f64,
    },
}

impl Item {
    fn shift_y(&mut self, dy: f64) {
        match self {
            Item::Rect { y, .. }
            | Item::Text { y, .. }
            | Item::Svg { y, .. }
            | Item::Image { y, .. } => *y += dy,
        }
    }
}

/// 図の描画結果。失敗した場合はエラーメッセージ。
type DiagramResult = Result<Rc<SvgImage>, String>;

type PictureResult = Result<Rc<Picture>, String>;
type PictureCache = HashMap<(PathBuf, Option<SystemTime>), PictureResult>;

/// 図のキャッシュがこの件数を超えたら捨てる (編集中の図の古い版がたまるため)。
const DIAGRAM_CACHE_LIMIT: usize = 64;

pub struct Layouter {
    context: pango::Context,
    /// (ソース, 配色) のハッシュをキーにした図のキャッシュ。
    diagrams: RefCell<HashMap<u64, DiagramResult>>,
    /// (パス, 更新時刻) をキーにした画像のキャッシュ。
    pictures: RefCell<PictureCache>,
}

impl Default for Layouter {
    fn default() -> Self {
        Self::new()
    }
}

impl Layouter {
    pub fn new() -> Self {
        let context = pangocairo::FontMap::default().create_context();
        // ヒンティングを切り、拡大率が違っても折り返し位置が変わらないようにする。
        if let Ok(mut options) = cairo::FontOptions::new() {
            options.set_hint_metrics(cairo::HintMetrics::Off);
            options.set_hint_style(cairo::HintStyle::None);
            pangocairo::functions::context_set_font_options(&context, Some(&options));
        }
        Layouter {
            context,
            diagrams: RefCell::new(HashMap::new()),
            pictures: RefCell::new(HashMap::new()),
        }
    }

    /// `page_number` は 1 始まりのスライド番号。画像の相対パスは `base_dir` から解決する。
    pub fn layout(
        &self,
        slide: &Slide,
        style: &Style,
        size: SlideSize,
        page_number: usize,
        base_dir: &Path,
    ) -> DisplayList {
        let (width, height) = size.logical();
        let available = height - 2.0 * style.slide.padding_y;
        let mut flow = Flow {
            context: &self.context,
            diagrams: &self.diagrams,
            pictures: &self.pictures,
            base_dir,
            assets: Vec::new(),
            links: Vec::new(),
            available,
            style,
            items: Vec::new(),
            color: style.palette.foreground,
            align: style.slide.align,
            overflow: false,
        };

        // 背景画像が片側だけを使う場合、本文は残りの側に置く。
        let split = slide.backgrounds.iter().find_map(|image| match image.placement {
            Placement::Background { split, .. } => split,
            _ => None,
        });
        let (background, mut region_x, mut region_width) = match split {
            Some((side, percent)) => {
                let used = width * percent.0 / 100.0;
                match side {
                    Side::Left => ((0.0, used), used, width - used),
                    Side::Right => ((width - used, used), 0.0, width - used),
                }
            }
            None => ((0.0, width), 0.0, width),
        };
        flow.rect(0.0, 0.0, width, height, 0.0, style.palette.background);
        flow.backgrounds(&slide.backgrounds, background.0, background.1, height);
        let mut items = std::mem::take(&mut flow.items);

        region_x += style.slide.padding_x;
        region_width -= 2.0 * style.slide.padding_x;
        // フッタなどは、片側の画像に関係なく本文領域いっぱいに置く。
        let (margin_x, margin_width) = (region_x, region_width);
        if let Some(image) = &slide.side {
            let side_width = image
                .width
                .unwrap_or(Length::Percent(Num(40.0)))
                .resolve(region_width)
                .clamp(0.0, region_width);
            let gap = style.text.size;
            let side_x = match image.placement {
                Placement::Side(Side::Left) => {
                    region_x += side_width + gap;
                    region_x - side_width - gap
                }
                _ => region_x + region_width - side_width,
            };
            region_width -= side_width + gap;
            flow.contained(image, side_x, style.slide.padding_y, side_width, available);
            items.append(&mut flow.items);
        }

        let content_height = flow.blocks(
            &slide.blocks,
            region_x,
            region_width,
            0.0,
            style.text.block_spacing,
        );
        let slack = (available - content_height).max(0.0);
        let offset = style.slide.padding_y
            + match style.slide.valign {
                VAlign::Top => 0.0,
                VAlign::Center => slack / 2.0,
                VAlign::Bottom => slack,
            };
        for item in &mut flow.items {
            item.shift_y(offset);
        }
        for link in &mut flow.links {
            link.y += offset;
        }
        let overflow = flow.overflow || content_height > available + 0.5;
        items.append(&mut flow.items);

        for image in &slide.floats {
            flow.float(image, width, height);
        }

        // ヘッダ、フッタ、ページ番号。
        flow.color = style.palette.muted;
        let line_height = style.footer.size * 1.2;
        let footer_y = height - style.footer.margin - line_height;
        let mut margin_text = |text: &str, y: f64, align: Align| {
            let layout = flow.text_layout(
                &escape(text),
                &TextOptions {
                    font: &style.fonts.body,
                    size: style.footer.size,
                    line_height: 1.2,
                    width: Some(margin_width),
                    align,
                },
            );
            flow.put_text(layout, margin_x, y);
        };
        if let Some(header) = &slide.props.header {
            margin_text(header, style.footer.margin, Align::Left);
        }
        if let Some(footer) = &slide.props.footer {
            margin_text(footer, footer_y, Align::Left);
        }
        if slide.props.paginate {
            margin_text(&page_number.to_string(), footer_y, Align::Right);
        }
        items.append(&mut flow.items);

        DisplayList {
            width,
            height,
            items,
            overflow,
            assets: flow.assets,
            links: flow.links,
        }
    }
}

struct TextOptions<'s> {
    font: &'s str,
    size: f64,
    /// フォントサイズに対する行送りの倍率。
    line_height: f64,
    /// 折り返し幅。`None` なら折り返さない。
    width: Option<f64>,
    align: Align,
}

struct Flow<'a> {
    context: &'a pango::Context,
    diagrams: &'a RefCell<HashMap<u64, DiagramResult>>,
    pictures: &'a RefCell<PictureCache>,
    base_dir: &'a Path,
    assets: Vec<PathBuf>,
    links: Vec<LinkArea>,
    /// 本文領域の高さ。
    available: f64,
    style: &'a Style,
    items: Vec<Item>,
    color: Color,
    align: Align,
    overflow: bool,
}

fn text_layout(context: &pango::Context, markup: &str, options: &TextOptions) -> pango::Layout {
    let layout = pango::Layout::new(context);
    let mut font = pango::FontDescription::new();
    font.set_family(options.font);
    font.set_absolute_size(options.size * UNITS);
    layout.set_font_description(Some(&font));
    layout.set_markup(markup);
    let attrs = layout.attributes().unwrap_or_default();
    attrs.insert(pango::AttrInt::new_line_height_absolute(
        (options.size * options.line_height * UNITS) as i32,
    ));
    layout.set_attributes(Some(&attrs));
    if let Some(width) = options.width {
        set_width(&layout, width, options.align);
    }
    layout
}

impl<'a> Flow<'a> {
    fn text_layout(&self, markup: &str, options: &TextOptions) -> pango::Layout {
        text_layout(self.context, markup, options)
    }

    fn body_layout(&self, markup: &str, width: Option<f64>, align: Align) -> pango::Layout {
        self.text_layout(
            markup,
            &TextOptions {
                font: &self.style.fonts.body,
                size: self.style.text.size,
                line_height: self.style.text.line_height,
                width,
                align,
            },
        )
    }

    fn put_text(&mut self, layout: pango::Layout, x: f64, y: f64) {
        self.items.push(Item::Text {
            x,
            y,
            layout,
            color: self.color,
        });
    }

    fn rect(&mut self, x: f64, y: f64, width: f64, height: f64, radius: f64, color: Color) {
        self.items.push(Item::Rect {
            x,
            y,
            width,
            height,
            radius,
            color,
        });
    }

    fn markup(&self, inlines: &[Inline]) -> (String, Vec<LinkSpan>) {
        let mut builder = MarkupBuilder {
            out: String::new(),
            style: self.style,
            last: None,
            pending_break: false,
            plain_len: 0,
            links: Vec::new(),
        };
        builder.inlines(inlines);
        (builder.out, builder.links)
    }

    /// 組み上がったテキストの中のリンクについて、クリックできる範囲を記録する。
    /// 折り返し幅と揃えを決めたあと、テキストを置く位置を渡して呼ぶ。
    fn add_links(&mut self, layout: &pango::Layout, x: f64, y: f64, spans: &[LinkSpan]) {
        if spans.is_empty() {
            return;
        }
        let mut lines = layout.iter();
        loop {
            if let Some(line) = lines.line_readonly() {
                let (_, extents) = lines.line_extents();
                let line_start = line.start_index() as usize;
                let line_end = line_start + line.length() as usize;
                for span in spans {
                    let (start, end) = (span.start.max(line_start), span.end.min(line_end));
                    if start >= end {
                        continue;
                    }
                    // x_ranges はレイアウト左端からの (開始, 終了) の組を並べて返す。
                    for range in line.x_ranges(start as i32, end as i32).as_chunks::<2>().0 {
                        self.links.push(LinkArea {
                            x: x + f64::from(range[0]) / UNITS,
                            y: y + f64::from(extents.y()) / UNITS,
                            width: f64::from(range[1] - range[0]) / UNITS,
                            height: f64::from(extents.height()) / UNITS,
                            url: span.url.clone(),
                        });
                    }
                }
            }
            if !lines.next_line() {
                break;
            }
        }
    }

    /// ブロック列を縦に並べ、最後のブロックの下端の y を返す。
    fn blocks(&mut self, blocks: &[Block], x: f64, width: f64, top: f64, spacing: f64) -> f64 {
        let mut y = top;
        for (index, block) in blocks.iter().enumerate() {
            if index > 0 {
                y += match blocks[index - 1] {
                    Block::Heading { .. } => self.style.heading.spacing,
                    _ => spacing,
                };
            }
            y += match block {
                Block::Heading { level, inlines } => self.heading(*level, inlines, x, width, y),
                Block::Paragraph(inlines) => {
                    let (markup, links) = self.markup(inlines);
                    let layout = self.body_layout(&markup, Some(width), self.align);
                    let height = size_of(&layout).1;
                    self.add_links(&layout, x, y, &links);
                    self.put_text(layout, x, y);
                    height
                }
                Block::List(list) => self.list(list, x, width, y),
                Block::Code(code) => self.code(code, x, width, y),
                Block::Diagram { kind, source } => self.diagram(*kind, source, x, width, y),
                Block::Quote(children) => self.quote(children, x, width, y),
                Block::Table(table) => self.table(table, x, width, y),
                Block::Images(images) => self.images(images, x, width, y),
            };
        }
        y
    }

    fn heading(&mut self, level: u8, inlines: &[Inline], x: f64, width: f64, y: f64) -> f64 {
        let style = self.style;
        let heading = &style.heading;
        let size = heading.sizes[usize::from(level.clamp(1, 6)) - 1];
        let (inner, links) = self.markup(inlines);
        let markup = format!("<span font_weight=\"{}\">{inner}</span>", heading.weight);
        let layout = self.text_layout(
            &markup,
            &TextOptions {
                font: &style.fonts.heading,
                size,
                line_height: 1.25,
                width: Some(width),
                align: self.align,
            },
        );
        let height = size_of(&layout).1;
        let saved = self.color;
        self.color = heading.color.unwrap_or(saved);
        self.add_links(&layout, x, y, &links);
        self.put_text(layout, x, y);
        self.color = saved;
        height
    }

    fn list(&mut self, list: &List, x: f64, width: f64, top: f64) -> f64 {
        let markers: Vec<pango::Layout> = list
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let marker = match (item.checked, list.start) {
                    // 異体字セレクタで、絵文字ではなく文字としての字形を選ぶ。
                    (Some(true), _) => "☑\u{FE0E}".to_owned(),
                    (Some(false), _) => "☐\u{FE0E}".to_owned(),
                    (None, Some(start)) => format!("{}.", start + index),
                    (None, None) => "•".to_owned(),
                };
                self.body_layout(&escape(&marker), None, Align::Left)
            })
            .collect();
        let gap = self.style.text.size * 0.4;
        let indent = markers
            .iter()
            .map(|m| size_of(m).0 + gap)
            .fold(self.style.list.indent, f64::max);

        // 項目の本文は、スライドが中央揃えでも左揃えにする。
        let saved_align = std::mem::replace(&mut self.align, Align::Left);
        let spacing = self.style.list.item_spacing;
        let mut y = top;
        for (index, (item, marker)) in list.items.iter().zip(markers).enumerate() {
            if index > 0 {
                y += spacing;
            }
            let marker_width = size_of(&marker).0;
            let marker_height = size_of(&marker).1;
            // 番号は右揃え、記号は字下げ幅の中ほどに置く。
            let marker_x = if list.start.is_some() {
                x + indent - gap - marker_width
            } else {
                x + (indent - gap - marker_width).max(0.0) / 2.0
            };
            self.put_text(marker, marker_x, y);
            let bottom = self.blocks(&item.blocks, x + indent, width - indent, y, spacing);
            y = bottom.max(y + marker_height);
        }
        self.align = saved_align;
        y - top
    }

    fn diagram(&mut self, kind: DiagramKind, source: &str, x: f64, width: f64, top: f64) -> f64 {
        let style = self.style;
        let mut hasher = DefaultHasher::new();
        (kind, source, format!("{:?}{:?}", style.palette, style.fonts)).hash(&mut hasher);
        let key = hasher.finish();
        let cached = self.diagrams.borrow().get(&key).cloned();
        let result = cached.unwrap_or_else(|| {
            let result = match kind {
                DiagramKind::Mermaid => diagram::render_mermaid(source, style).map(Rc::new),
            };
            let mut cache = self.diagrams.borrow_mut();
            if cache.len() >= DIAGRAM_CACHE_LIMIT {
                cache.clear();
            }
            cache.insert(key, result.clone());
            result
        });
        match result {
            Ok(image) => {
                // 図の文字が本文より少し小さくなる倍率を基準に、残りの領域に収まるよう縮める。
                let natural = style.text.size / DIAGRAM_BASE_FONT_SIZE * 0.7;
                let remaining = (self.available - top).max(self.available * 0.2);
                let scale = natural
                    .min(width / image.width)
                    .min(remaining / image.height);
                let (image_width, image_height) = (image.width * scale, image.height * scale);
                self.items.push(Item::Svg {
                    x: x + (width - image_width) / 2.0,
                    y: top,
                    width: image_width,
                    height: image_height,
                    image,
                });
                image_height
            }
            Err(message) => {
                let code = CodeBlock {
                    code: format!("図を描画できません: {message}\n\n{source}"),
                    ..CodeBlock::default()
                };
                self.code(&code, x, width, top)
            }
        }
    }

    /// 画像を読み込む。同じファイルは、更新されない限り読み直さない。
    fn load(&mut self, image: &Image) -> PictureResult {
        if image.src.contains("://") {
            return Err(format!("URL の画像は未対応です: {}", image.src));
        }
        let path = self.base_dir.join(&image.src);
        self.assets.push(path.clone());
        let modified = std::fs::metadata(&path).and_then(|meta| meta.modified()).ok();
        let key = (path, modified);
        if let Some(cached) = self.pictures.borrow().get(&key) {
            return cached.clone();
        }
        let result = Picture::load(&key.0)
            .map(Rc::new)
            .map_err(|err| format!("{}: {err}", image.src));
        let mut cache = self.pictures.borrow_mut();
        if cache.len() >= PICTURE_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(key, result.clone());
        result
    }

    /// 画像を指定の矩形に置く。読み込めなかった場合は、同じ場所にメッセージを出す。
    fn place(&mut self, image: &Image, loaded: &PictureResult, rect: (f64, f64, f64, f64)) {
        let (x, y, width, height) = rect;
        match loaded {
            Ok(picture) => self.items.push(Item::Image {
                x,
                y,
                width,
                height,
                picture: picture.clone(),
                source: source_rect(image, picture),
                radius: image.round.0,
                shadow: image.shadow,
                opacity: image.opacity.0,
            }),
            Err(message) => {
                self.rect(x, y, width, height, 0.0, self.style.palette.code_background);
                let size = self.style.text.size * 0.5;
                let layout = self.text_layout(
                    &escape(&format!("画像を表示できません: {message}")),
                    &TextOptions {
                        font: &self.style.fonts.body,
                        size,
                        line_height: 1.3,
                        width: Some((width - 2.0 * size).max(size)),
                        align: Align::Left,
                    },
                );
                let saved = std::mem::replace(&mut self.color, self.style.palette.muted);
                self.put_text(layout, x + size, y + size);
                self.color = saved;
            }
        }
    }

    /// キャプションを画像の下に置き、使った高さを返す。
    fn caption(&mut self, image: &Image, x: f64, y: f64, width: f64) -> f64 {
        let Some(caption) = &image.caption else {
            return 0.0;
        };
        let size = self.style.text.size * CAPTION_SCALE;
        let layout = self.text_layout(
            &escape(caption),
            &TextOptions {
                font: &self.style.fonts.body,
                size,
                line_height: 1.3,
                width: Some(width),
                align: Align::Center,
            },
        );
        let height = size_of(&layout).1;
        // 影があるときは、影に重ならないよう間隔を広げる。
        let gap = if image.shadow { size * 1.2 } else { size * 0.3 };
        let saved = std::mem::replace(&mut self.color, self.style.palette.muted);
        self.put_text(layout, x, y + gap);
        self.color = saved;
        height + gap
    }

    /// キャプションに必要な高さの見積もり (1 行分)。
    fn caption_height(&self, image: &Image) -> f64 {
        match image.caption {
            Some(_) if image.shadow => self.style.text.size * CAPTION_SCALE * 2.5,
            Some(_) => self.style.text.size * CAPTION_SCALE * 1.6,
            None => 0.0,
        }
    }

    /// 背景画像を、指定の横範囲に等分して並べる。
    fn backgrounds(&mut self, images: &[Image], x: f64, width: f64, height: f64) {
        let column = width / images.len().max(1) as f64;
        for (index, image) in images.iter().enumerate() {
            let loaded = self.load(image);
            let column_x = x + column * index as f64;
            let contain = matches!(
                image.placement,
                Placement::Background { fit: Fit::Contain, .. }
            );
            match &loaded {
                Ok(picture) if !contain => {
                    // 領域と同じ縦横比になるよう、元画像の中央を切り出して全面に敷く。
                    let (sx, sy, sw, sh) = source_rect(image, picture);
                    let scale = (column / sw).max(height / sh);
                    let (crop_width, crop_height) = (column / scale, height / scale);
                    self.items.push(Item::Image {
                        x: column_x,
                        y: 0.0,
                        width: column,
                        height,
                        picture: picture.clone(),
                        source: (
                            sx + (sw - crop_width) / 2.0,
                            sy + (sh - crop_height) / 2.0,
                            crop_width,
                            crop_height,
                        ),
                        radius: 0.0,
                        shadow: false,
                        opacity: image.opacity.0,
                    });
                }
                Ok(picture) => {
                    let (_, _, sw, sh) = source_rect(image, picture);
                    let scale = (column / sw).min(height / sh);
                    let (w, h) = (sw * scale, sh * scale);
                    let rect = (column_x + (column - w) / 2.0, (height - h) / 2.0, w, h);
                    self.place(image, &loaded, rect);
                }
                Err(_) => self.place(image, &loaded, (column_x, 0.0, column, height)),
            }
        }
    }

    /// 画像を、矩形の中に収まる最大の大きさで中央に置く (キャプション付き)。
    fn contained(&mut self, image: &Image, x: f64, y: f64, width: f64, height: f64) {
        let loaded = self.load(image);
        let room = height - self.caption_height(image);
        let (w, h) = match &loaded {
            Ok(picture) => {
                let (_, _, sw, sh) = source_rect(image, picture);
                let scale = (width / sw).min(room / sh);
                (sw * scale, sh * scale)
            }
            Err(_) => (width, width * 9.0 / 16.0),
        };
        let image_x = x + (width - w) / 2.0;
        let image_y = y + (room - h) / 2.0;
        self.place(image, &loaded, (image_x, image_y, w, h));
        self.caption(image, image_x, image_y + h, w);
    }

    /// 絶対配置の画像を置く。位置と大きさの割合はスライド全体が基準。
    fn float(&mut self, image: &Image, slide_width: f64, slide_height: f64) {
        let Placement::At { x, y, anchor } = image.placement else {
            return;
        };
        let loaded = self.load(image);
        let (w, h) = image_size(image, &loaded, slide_width, slide_height);
        let (fraction_x, fraction_y) = anchor.fraction();
        let image_x = x.resolve(slide_width) - w * fraction_x;
        let image_y = y.resolve(slide_height) - h * fraction_y;
        self.place(image, &loaded, (image_x, image_y, w, h));
        self.caption(image, image_x, image_y + h, w);
    }

    /// 画像だけの段落。複数あれば横に並べ、幅と残りの高さに収まるよう全体を縮める。
    fn images(&mut self, images: &[Image], x: f64, width: f64, top: f64) -> f64 {
        let gap = self.style.text.size * 0.5;
        let loaded: Vec<PictureResult> = images.iter().map(|image| self.load(image)).collect();
        let sizes: Vec<(f64, f64)> = images
            .iter()
            .zip(&loaded)
            .map(|(image, loaded)| image_size(image, loaded, width, self.available))
            .collect();
        let caption_room = images
            .iter()
            .map(|image| self.caption_height(image))
            .fold(0.0, f64::max);
        let gaps = gap * (images.len() as f64 - 1.0);
        let total_width: f64 = sizes.iter().map(|size| size.0).sum();
        let tallest = sizes.iter().map(|size| size.1).fold(0.0, f64::max);
        let remaining = (self.available - top).max(self.available * 0.2) - caption_room;
        let scale = ((width - gaps) / total_width)
            .min(remaining / tallest)
            .min(1.0);

        let row_width = total_width * scale + gaps;
        let row_height = tallest * scale;
        let align = images.iter().find_map(|image| image.align).unwrap_or(Align::Center);
        let mut image_x = match align {
            Align::Left => x,
            Align::Center => x + (width - row_width) / 2.0,
            Align::Right => x + width - row_width,
        };
        let mut caption_height = 0.0_f64;
        for ((image, loaded), (w, h)) in images.iter().zip(&loaded).zip(sizes) {
            let (w, h) = (w * scale, h * scale);
            self.place(image, loaded, (image_x, top + (row_height - h) / 2.0, w, h));
            caption_height = caption_height.max(self.caption(image, image_x, top + row_height, w));
            image_x += w + gap;
        }
        row_height + caption_height
    }

    fn quote(&mut self, children: &[Block], x: f64, width: f64, top: f64) -> f64 {
        let all = self.style;
        let quote = &all.quote;
        let saved = std::mem::replace(&mut self.color, all.palette.muted);
        let bottom = self.blocks(
            children,
            x + quote.indent,
            width - quote.indent,
            top,
            all.text.block_spacing,
        );
        self.color = saved;
        self.rect(
            x,
            top,
            quote.bar_width,
            bottom - top,
            quote.bar_width / 2.0,
            all.palette.border,
        );
        bottom - top
    }

    fn code(&mut self, code: &CodeBlock, x: f64, width: f64, top: f64) -> f64 {
        let all = self.style;
        let context = self.context;
        let style = &all.code;
        let lines = highlight::highlight(&code.code, code.lang.as_deref(), &style.theme);
        let mut markup = String::new();
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                markup.push('\n');
            }
            for span in line {
                markup.push_str(&format!("<span foreground=\"{}\"", span.color.to_hex()));
                if span.bold {
                    markup.push_str(" font_weight=\"bold\"");
                }
                if span.italic {
                    markup.push_str(" font_style=\"italic\"");
                }
                markup.push('>');
                escape_into(&span.text, &mut markup);
                markup.push_str("</span>");
            }
        }
        let numbers: String = (1..=lines.len())
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        let mono = |text: &str, size: f64| {
            text_layout(
                context,
                text,
                &TextOptions {
                    font: &all.fonts.mono,
                    size,
                    line_height: style.line_height,
                    width: None,
                    align: Align::Left,
                },
            )
        };
        // 幅に収まらなければ、下限まで文字を縮める。行は折り返さない。
        let measure = |size: f64| {
            let gutter = if code.numbered {
                size_of(&mono(&numbers, size)).0 + size
            } else {
                0.0
            };
            (size_of(&mono(&markup, size)).0, gutter)
        };
        let inner = width - 2.0 * style.padding;
        let mut size = style.size;
        let (text_width, gutter) = measure(size);
        if text_width + gutter > inner {
            size = (size * inner / (text_width + gutter)).max(style.min_size);
        }
        let (text_width, gutter) = measure(size);
        if text_width + gutter > inner + 0.5 {
            self.overflow = true;
        }

        let layout = mono(&markup, size);
        let pitch = size * style.line_height;
        let height = size_of(&layout).1 + 2.0 * style.padding;
        self.rect(
            x,
            top,
            width,
            height,
            style.radius,
            all.palette.code_background,
        );
        for &line in &code.highlight_lines {
            if (1..=lines.len()).contains(&line) {
                self.rect(
                    x,
                    top + style.padding + (line - 1) as f64 * pitch,
                    width,
                    pitch,
                    0.0,
                    all.palette.line_highlight,
                );
            }
        }
        let text_x = x + style.padding;
        if code.numbered {
            let number_layout = mono(&numbers, size);
            set_width(&number_layout, gutter - size, Align::Right);
            let saved = std::mem::replace(&mut self.color, all.palette.muted);
            self.put_text(number_layout, text_x, top + style.padding);
            self.color = saved;
        }
        self.put_text(layout, text_x + gutter, top + style.padding);
        height
    }

    fn table(&mut self, table: &Table, x: f64, width: f64, top: f64) -> f64 {
        let all = self.style;
        let style = &all.table;
        let columns = table
            .rows
            .iter()
            .map(Vec::len)
            .fold(table.header.len(), usize::max);
        if columns == 0 {
            return 0.0;
        }
        let header = (!table.header.is_empty()).then_some(&table.header);
        let rows: Vec<(bool, &Vec<Vec<Inline>>)> = header
            .into_iter()
            .map(|row| (true, row))
            .chain(table.rows.iter().map(|row| (false, row)))
            .collect();

        // まず折り返しなしで組み、列ごとの自然な幅を求める。
        let mut cell_links = Vec::new();
        let cells: Vec<Vec<pango::Layout>> = rows
            .iter()
            .map(|(is_header, row)| {
                row.iter()
                    .map(|cell| {
                        let (mut markup, links) = self.markup(cell);
                        cell_links.push(links);
                        if *is_header {
                            markup = format!("<b>{markup}</b>");
                        }
                        self.body_layout(&markup, None, Align::Left)
                    })
                    .collect()
            })
            .collect();
        let mut column_widths = vec![0.0_f64; columns];
        for row in &cells {
            for (column, cell) in row.iter().enumerate() {
                column_widths[column] =
                    column_widths[column].max(size_of(cell).0 + 2.0 * style.padding_x);
            }
        }
        let natural: f64 = column_widths.iter().sum();
        if natural > width {
            for column_width in &mut column_widths {
                *column_width *= width / natural;
            }
        }
        let table_width = natural.min(width);
        let table_x = match self.align {
            Align::Left => x,
            Align::Center => x + (width - table_width) / 2.0,
            Align::Right => x + width - table_width,
        };

        let mut cell_links = cell_links.into_iter();
        let mut y = top;
        for ((is_header, _), row) in rows.iter().zip(cells) {
            for (column, cell) in row.iter().enumerate() {
                let align = table.alignments.get(column).copied().unwrap_or_default();
                set_width(cell, column_widths[column] - 2.0 * style.padding_x, align);
            }
            let row_height = row.iter().map(|cell| size_of(cell).1).fold(0.0, f64::max)
                + 2.0 * style.padding_y;
            let mut cell_x = table_x;
            for (column, cell) in row.into_iter().enumerate() {
                let (text_x, text_y) = (cell_x + style.padding_x, y + style.padding_y);
                self.add_links(&cell, text_x, text_y, &cell_links.next().unwrap_or_default());
                self.put_text(cell, text_x, text_y);
                cell_x += column_widths[column];
            }
            y += row_height;
            // 見出し行の下は太く、それ以外は細い罫線を引く。
            let thickness = if *is_header {
                style.border_width
            } else {
                style.border_width / 2.0
            };
            let color = if *is_header {
                all.palette.muted
            } else {
                all.palette.border
            };
            self.rect(table_x, y, table_width, thickness, 0.0, color);
            y += thickness;
        }
        y - top
    }
}

/// 元画像のうち描く範囲 (`crop` の指定、なければ全体)。
fn source_rect(image: &Image, picture: &Picture) -> (f64, f64, f64, f64) {
    let (width, height) = picture.size();
    let Some([x, y, w, h]) = image.crop else {
        return (0.0, 0.0, width, height);
    };
    let x = x.resolve(width).clamp(0.0, width - 1.0);
    let y = y.resolve(height).clamp(0.0, height - 1.0);
    (
        x,
        y,
        w.resolve(width).clamp(1.0, width - x),
        h.resolve(height).clamp(1.0, height - y),
    )
}

/// `w:` / `h:` の指定から表示サイズを決める。片方だけなら縦横比を保ち、なければ本来の大きさ。
fn image_size(image: &Image, loaded: &PictureResult, ref_width: f64, ref_height: f64) -> (f64, f64) {
    let (natural_width, natural_height) = match loaded {
        Ok(picture) => {
            let (_, _, w, h) = source_rect(image, picture);
            (w, h)
        }
        Err(_) => MISSING_IMAGE_SIZE,
    };
    let width = image.width.map(|w| w.resolve(ref_width));
    let height = image.height.map(|h| h.resolve(ref_height));
    match (width, height) {
        (Some(width), Some(height)) => (width, height),
        (Some(width), None) => (width, width * natural_height / natural_width),
        (None, Some(height)) => (height * natural_width / natural_height, height),
        (None, None) => (natural_width, natural_height),
    }
}

fn set_width(layout: &pango::Layout, width: f64, align: Align) {
    layout.set_width((width * UNITS) as i32);
    layout.set_wrap(pango::WrapMode::WordChar);
    layout.set_alignment(match align {
        Align::Left => pango::Alignment::Left,
        Align::Center => pango::Alignment::Center,
        Align::Right => pango::Alignment::Right,
    });
}

/// レイアウトの論理サイズ (幅, 高さ)。
fn size_of(layout: &pango::Layout) -> (f64, f64) {
    let (_, logical) = layout.extents();
    (
        f64::from(logical.width()) / UNITS,
        f64::from(logical.height()) / UNITS,
    )
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_into(text, &mut out);
    out
}

fn escape_into(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
}

/// インライン要素を Pango マークアップに変換する。
struct MarkupBuilder<'a> {
    out: String,
    style: &'a Style,
    last: Option<char>,
    /// 直前がソース上の改行で、空白にするかどうかを次の文字を見て決める。
    pending_break: bool,
    /// これまでに出力した、マークアップを除いたテキストのバイト数。
    plain_len: usize,
    links: Vec<LinkSpan>,
}

impl MarkupBuilder<'_> {
    fn text(&mut self, text: &str) {
        let Some(first) = text.chars().next() else {
            return;
        };
        if std::mem::take(&mut self.pending_break) {
            // 和文に接する改行は空白にしない。
            let touches_cjk = self.last.is_some_and(is_wide) || is_wide(first);
            if !touches_cjk {
                self.out.push(' ');
                self.plain_len += 1;
            }
        }
        escape_into(text, &mut self.out);
        self.plain_len += text.len();
        self.last = text.chars().last();
    }

    fn wrapped(&mut self, open: &str, close: &str, children: &[Inline]) {
        self.out.push_str(open);
        self.inlines(children);
        self.out.push_str(close);
    }

    fn inlines(&mut self, inlines: &[Inline]) {
        for inline in inlines {
            match inline {
                Inline::Text(text) => self.text(text),
                Inline::Code(code) => {
                    // 改行由来の空白をコードの背景色の外に出すため、先に確定させる。
                    if std::mem::take(&mut self.pending_break) {
                        self.out.push(' ');
                        self.plain_len += 1;
                    }
                    self.out.push_str(&format!(
                        "<span font_family=\"{}\" background=\"{}\">",
                        escape(&self.style.fonts.mono),
                        self.style.palette.code_background.to_hex()
                    ));
                    escape_into(code, &mut self.out);
                    self.plain_len += code.len();
                    self.out.push_str("</span>");
                    self.last = code.chars().last().or(self.last);
                }
                Inline::Emph(children) => self.wrapped("<i>", "</i>", children),
                Inline::Strong(children) => self.wrapped("<b>", "</b>", children),
                Inline::Strike(children) => self.wrapped("<s>", "</s>", children),
                Inline::Link { url, children } => {
                    let open = format!(
                        "<span foreground=\"{}\" underline=\"single\">",
                        self.style.palette.accent.to_hex()
                    );
                    let start = self.plain_len;
                    self.wrapped(&open, "</span>", children);
                    self.links.push(LinkSpan {
                        start,
                        end: self.plain_len,
                        url: url.clone(),
                    });
                }
                Inline::SoftBreak => self.pending_break = true,
                Inline::LineBreak => {
                    self.pending_break = false;
                    self.out.push('\n');
                    self.plain_len += 1;
                    self.last = Some('\n');
                }
            }
        }
    }
}

/// 和文など、前後に空白を置かずに続ける文字かどうか。
fn is_wide(c: char) -> bool {
    matches!(u32::from(c), 0x2E80..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x3FFFF)
}
