//! Markdown から組み立てる型付きのスライドモデル。レイアウトの入力になる。

#[derive(Debug, Clone, PartialEq)]
pub struct Deck {
    pub meta: DeckMeta,
    pub slides: Vec<Slide>,
    pub diagnostics: Vec<Diagnostic>,
}

/// リンクの行き先。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkTarget<'a> {
    /// 0 始まりのスライド位置。
    Slide(usize),
    /// 外部の URL やファイル。
    External(&'a str),
}

impl Deck {
    /// リンク先を解決する。`#3` はスライド番号、`#名前` は見出しのアンカー、それ以外は外部リンク。
    /// スライド内参照の行き先が無ければ `None`。
    pub fn link_target<'a>(&self, url: &'a str) -> Option<LinkTarget<'a>> {
        let Some(fragment) = url.strip_prefix('#') else {
            return Some(LinkTarget::External(url));
        };
        let index = match fragment.parse::<usize>() {
            Ok(number) => number.checked_sub(1).filter(|&i| i < self.slides.len()),
            Err(_) => self
                .slides
                .iter()
                .position(|slide| slide.anchors.iter().any(|anchor| anchor == fragment)),
        };
        index.map(LinkTarget::Slide)
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DeckMeta {
    pub theme: Option<String>,
    pub size: SlideSize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SlideSize {
    #[default]
    Wide,
    Standard,
}

impl SlideSize {
    /// 論理座標でのスライドサイズ。高さは常に 1080。
    pub fn logical(self) -> (f64, f64) {
        match self {
            SlideSize::Wide => (1920.0, 1080.0),
            SlideSize::Standard => (1440.0, 1080.0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Slide {
    pub blocks: Vec<Block>,
    pub props: SlideProps,
    pub notes: Vec<String>,
    /// 背景画像 (`bg`)。複数あれば横に並べる。
    pub backgrounds: Vec<Image>,
    /// 本文の片側に置く画像 (`side:`)。
    pub side: Option<Image>,
    /// 絶対配置の画像 (`at:`)。
    pub floats: Vec<Image>,
    /// このスライドの見出しから作ったアンカー名 (`#名前` で参照できる)。
    pub anchors: Vec<String>,
}

/// ディレクティブで決まるスライド単位の設定。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct SlideProps {
    pub class: Vec<String>,
    pub paginate: bool,
    pub header: Option<String>,
    pub footer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Block {
    Heading { level: u8, inlines: Vec<Inline> },
    Paragraph(Vec<Inline>),
    List(List),
    Code(CodeBlock),
    Diagram { kind: DiagramKind, source: String },
    Quote(Vec<Block>),
    Table(Table),
    /// 画像だけの段落。複数あれば横に並べる。
    Images(Vec<Image>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct List {
    /// 番号付きリストなら開始番号。
    pub start: Option<usize>,
    pub items: Vec<ListItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ListItem {
    /// タスクリスト項目ならチェック状態。
    pub checked: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct CodeBlock {
    pub lang: Option<String>,
    pub code: String,
    /// 強調する行番号 (1 始まり)。
    pub highlight_lines: Vec<usize>,
    pub numbered: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagramKind {
    Mermaid,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Table {
    pub alignments: Vec<Align>,
    pub header: Vec<Vec<Inline>>,
    pub rows: Vec<Vec<Vec<Inline>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Inline {
    Text(String),
    Code(String),
    Emph(Vec<Inline>),
    Strong(Vec<Inline>),
    Strike(Vec<Inline>),
    Link { url: String, children: Vec<Inline> },
    SoftBreak,
    LineBreak,
}

/// ハッシュと等値比較ができる数値。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Num(pub f64);

impl Eq for Num {}

impl std::hash::Hash for Num {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Length {
    /// 論理座標での px。
    Px(Num),
    /// 基準の長さに対する割合 (0〜100)。
    Percent(Num),
}

impl Length {
    pub fn resolve(self, reference: f64) -> f64 {
        match self {
            Length::Px(px) => px.0,
            Length::Percent(percent) => reference * percent.0 / 100.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Image {
    /// 画像のパス (Markdown ファイルからの相対パス、または絶対パス)。
    pub src: String,
    pub alt: String,
    pub caption: Option<String>,
    pub width: Option<Length>,
    pub height: Option<Length>,
    pub placement: Placement,
    /// 流れの中での横位置。省略時は中央。
    pub align: Option<Align>,
    /// 元画像から切り出す範囲 (x, y, 幅, 高さ)。
    pub crop: Option<[Length; 4]>,
    /// 角の丸みの半径。
    pub round: Num,
    pub shadow: bool,
    pub opacity: Num,
    /// 上に載せる文字を読みやすくするための明るさの調整。
    pub tint: Option<Tint>,
}

/// 画像を黒または白に寄せる調整。値は寄せる強さ (0〜1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tint {
    Darken(Num),
    Lighten(Num),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Placement {
    /// 本文の流れの中に置く。
    Flow,
    /// スライド上の指定位置に置く。
    At { x: Length, y: Length, anchor: Anchor },
    /// 本文の片側に置く。
    Side(Side),
    /// 背景に敷く。`split` があれば、その側の指定割合だけを使い、本文は残りに置く。
    Background { fit: Fit, split: Option<(Side, Num)> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fit {
    /// 領域を埋める (はみ出す分は切り落とす)。
    Cover,
    /// 全体が収まるように縮める。
    Contain,
}

/// `at:` で指定した位置に合わせる、画像側の基準点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Anchor {
    /// 基準点の位置を、画像の幅・高さに対する割合 (0〜1) で返す。
    pub fn fraction(self) -> (f64, f64) {
        match self {
            Anchor::TopLeft => (0.0, 0.0),
            Anchor::Top => (0.5, 0.0),
            Anchor::TopRight => (1.0, 0.0),
            Anchor::Left => (0.0, 0.5),
            Anchor::Center => (0.5, 0.5),
            Anchor::Right => (1.0, 0.5),
            Anchor::BottomLeft => (0.0, 1.0),
            Anchor::Bottom => (0.5, 1.0),
            Anchor::BottomRight => (1.0, 1.0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Diagnostic {
    pub severity: Severity,
    /// ソース上の行番号 (1 始まり)。
    pub line: usize,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    Warning,
    Error,
}
