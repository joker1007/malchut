//! Markdown を [`Deck`] に変換する。

use comrak::nodes::{AstNode, ListType, NodeValue, TableAlignment};
use comrak::{Arena, Options, parse_document};
use serde_yaml_ng::Value;

use crate::model::*;

const FRONT_MATTER_DELIMITER: &str = "---";

pub fn parse(src: &str) -> Deck {
    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.front_matter_delimiter = Some(FRONT_MATTER_DELIMITER.to_owned());
    let root = parse_document(&arena, src, &options);

    let mut parser = Parser::default();
    for node in root.children() {
        parser.top_level(node);
    }
    parser.finish_slide();

    let mut deck = Deck {
        meta: parser.meta,
        slides: parser.slides,
        diagnostics: parser.diagnostics,
    };
    for (url, line) in parser.internal_links {
        if deck.link_target(&url).is_none() {
            deck.diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                line,
                message: format!("リンク先のスライドがありません: {url}"),
            });
        }
    }
    deck
}

/// スライド 1 枚だけに効くディレクティブ (`_class` など) の上書き分。
#[derive(Default)]
struct LocalProps {
    class: Option<Vec<String>>,
    paginate: Option<bool>,
    header: Option<Option<String>>,
    footer: Option<Option<String>>,
}

#[derive(Default)]
struct Parser {
    meta: DeckMeta,
    slides: Vec<Slide>,
    diagnostics: Vec<Diagnostic>,
    /// 以降のスライドすべてに引き継がれる設定。
    base: SlideProps,
    local: LocalProps,
    blocks: Vec<Block>,
    notes: Vec<String>,
    backgrounds: Vec<Image>,
    side: Option<Image>,
    floats: Vec<Image>,
    /// インライン要素の走査中に見つけた画像。段落などの単位で振り分ける。
    pending_images: Vec<Image>,
    anchors: Vec<String>,
    /// アンカー名の重複に連番を振るため、デッキ全体で 1 つを使い回す。
    anchorizer: comrak::Anchorizer,
    /// スライド内参照 (`#...`) のリンクと行番号。全スライドを読んでから検証する。
    internal_links: Vec<(String, usize)>,
}

impl Parser {
    fn top_level<'a>(&mut self, node: &'a AstNode<'a>) {
        let line = line_of(node);
        // borrow を match の外へ持ち出さないよう、先に種別だけ判定する。
        enum Kind {
            FrontMatter(String),
            Break,
            Other,
        }
        let kind = match &node.data.borrow().value {
            NodeValue::FrontMatter(raw) => Kind::FrontMatter(raw.clone()),
            NodeValue::ThematicBreak => Kind::Break,
            _ => Kind::Other,
        };
        match kind {
            Kind::FrontMatter(raw) => self.front_matter(&raw, line),
            Kind::Break => self.finish_slide(),
            Kind::Other => {
                if let Some(block) = self.block(node) {
                    self.blocks.push(block);
                }
            }
        }
    }

    fn finish_slide(&mut self) {
        let local = std::mem::take(&mut self.local);
        let mut props = self.base.clone();
        if let Some(class) = local.class {
            props.class = class;
        }
        if let Some(paginate) = local.paginate {
            props.paginate = paginate;
        }
        if let Some(header) = local.header {
            props.header = header;
        }
        if let Some(footer) = local.footer {
            props.footer = footer;
        }
        self.slides.push(Slide {
            blocks: std::mem::take(&mut self.blocks),
            props,
            notes: std::mem::take(&mut self.notes),
            backgrounds: std::mem::take(&mut self.backgrounds),
            side: self.side.take(),
            floats: std::mem::take(&mut self.floats),
            anchors: std::mem::take(&mut self.anchors),
        });
    }

    fn warn(&mut self, line: usize, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic {
            severity: Severity::Warning,
            line,
            message: message.into(),
        });
    }

    fn front_matter(&mut self, raw: &str, line: usize) {
        let body: String = raw
            .lines()
            .filter(|l| l.trim() != FRONT_MATTER_DELIMITER)
            .collect::<Vec<_>>()
            .join("\n");
        match serde_yaml_ng::from_str::<Value>(&body) {
            Ok(Value::Mapping(map)) => {
                for (key, value) in &map {
                    let Some(key) = key.as_str() else {
                        self.warn(line, "front matter のキーが文字列ではありません");
                        continue;
                    };
                    match key {
                        "theme" => match value.as_str() {
                            Some(name) => self.meta.theme = Some(name.to_owned()),
                            None => self.warn(line, "theme は文字列で指定してください"),
                        },
                        "size" => match value.as_str() {
                            Some("16:9") => self.meta.size = SlideSize::Wide,
                            Some("4:3") => self.meta.size = SlideSize::Standard,
                            _ => self.warn(line, "size は \"16:9\" か \"4:3\" で指定してください"),
                        },
                        _ => {
                            if !self.directive(key, value, line) {
                                self.warn(line, format!("未知の front matter キー: {key}"));
                            }
                        }
                    }
                }
            }
            Ok(Value::Null) => {}
            Ok(_) => self.warn(line, "front matter はキーと値の組で書いてください"),
            Err(err) => self.diagnostics.push(Diagnostic {
                severity: Severity::Error,
                line,
                message: format!("front matter を解釈できません: {err}"),
            }),
        }
    }

    /// ディレクティブを 1 つ適用する。未知のキーなら `false` を返す。
    fn directive(&mut self, key: &str, value: &Value, line: usize) -> bool {
        let (name, local) = match key.strip_prefix('_') {
            Some(name) => (name, true),
            None => (key, false),
        };
        match name {
            "class" => {
                let class: Vec<String> = match value {
                    Value::String(s) => s.split_whitespace().map(str::to_owned).collect(),
                    Value::Sequence(seq) => seq
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect(),
                    Value::Null => Vec::new(),
                    _ => {
                        self.warn(line, "class は文字列で指定してください");
                        return true;
                    }
                };
                if local {
                    self.local.class = Some(class);
                } else {
                    self.base.class = class;
                }
            }
            "paginate" => match value.as_bool() {
                Some(paginate) if local => self.local.paginate = Some(paginate),
                Some(paginate) => self.base.paginate = paginate,
                None => self.warn(line, "paginate は true か false で指定してください"),
            },
            "header" | "footer" => {
                let text = match value {
                    Value::String(s) if !s.is_empty() => Some(s.clone()),
                    Value::String(_) | Value::Null => None,
                    _ => {
                        self.warn(line, format!("{name} は文字列で指定してください"));
                        return true;
                    }
                };
                match (name, local) {
                    ("header", true) => self.local.header = Some(text),
                    ("header", false) => self.base.header = text,
                    (_, true) => self.local.footer = Some(text),
                    (_, false) => self.base.footer = text,
                }
            }
            _ => return false,
        }
        true
    }

    /// HTML コメントの中身を、ディレクティブか発表者ノートとして取り込む。
    fn comment(&mut self, body: &str, line: usize) {
        let body = body.trim();
        if body.is_empty() {
            return;
        }
        if let Ok(Value::Mapping(map)) = serde_yaml_ng::from_str::<Value>(body) {
            let keys: Vec<&str> = map.keys().filter_map(Value::as_str).collect();
            let is_directive = keys.len() == map.len()
                && keys.iter().any(|k| {
                    matches!(
                        k.trim_start_matches('_'),
                        "class" | "paginate" | "header" | "footer"
                    )
                });
            if is_directive {
                for (key, value) in &map {
                    let key = key.as_str().unwrap_or_default();
                    if !self.directive(key, value, line) {
                        self.warn(line, format!("未知のディレクティブ: {key}"));
                    }
                }
                return;
            }
        }
        self.notes.push(body.to_owned());
    }

    /// HTML 断片からコメントをすべて取り込む。コメント以外が残れば警告する。
    fn html(&mut self, literal: &str, line: usize) {
        let mut rest = literal;
        let mut leftover = false;
        while let Some(start) = rest.find("<!--") {
            leftover |= !rest[..start].trim().is_empty();
            let after = &rest[start + 4..];
            match after.find("-->") {
                Some(end) => {
                    self.comment(&after[..end], line);
                    rest = &after[end + 3..];
                }
                None => {
                    self.comment(after, line);
                    rest = "";
                }
            }
        }
        leftover |= !rest.trim().is_empty();
        if leftover {
            self.warn(line, "HTML は非対応です (コメントのみ解釈します)");
        }
    }

    fn blocks_of<'a>(&mut self, node: &'a AstNode<'a>) -> Vec<Block> {
        node.children().filter_map(|c| self.block(c)).collect()
    }

    fn block<'a>(&mut self, node: &'a AstNode<'a>) -> Option<Block> {
        let line = line_of(node);
        let value = node.data.borrow().value.clone();
        match value {
            NodeValue::Paragraph => {
                let inlines = self.inlines(node);
                let images = self.place_images(line);
                let has_text = inlines.iter().any(|inline| match inline {
                    Inline::Text(text) => !text.trim().is_empty(),
                    Inline::SoftBreak | Inline::LineBreak => false,
                    _ => true,
                });
                match (has_text, images.is_empty()) {
                    (true, true) => Some(Block::Paragraph(inlines)),
                    (false, false) => Some(Block::Images(images)),
                    (true, false) => {
                        self.warn(line, "文中の画像は未対応です (画像だけの段落にしてください)");
                        Some(Block::Paragraph(inlines))
                    }
                    (false, true) => None,
                }
            }
            NodeValue::Heading(heading) => {
                let inlines = self.inlines(node);
                self.reject_flow_images(line);
                let anchor = self.anchorizer.anchorize(&plain_text(node));
                self.anchors.push(anchor);
                Some(Block::Heading {
                    level: heading.level,
                    inlines,
                })
            }
            NodeValue::List(list) => {
                let mut items = Vec::new();
                for item in node.children() {
                    let checked = match &item.data.borrow().value {
                        NodeValue::TaskItem(task) => Some(task.symbol.is_some()),
                        _ => None,
                    };
                    items.push(ListItem {
                        checked,
                        blocks: self.blocks_of(item),
                    });
                }
                Some(Block::List(List {
                    start: (list.list_type == ListType::Ordered).then_some(list.start),
                    items,
                }))
            }
            NodeValue::CodeBlock(code) => Some(self.code_block(&code.info, &code.literal, line)),
            NodeValue::BlockQuote => Some(Block::Quote(self.blocks_of(node))),
            NodeValue::Table(table) => {
                let mut header = Vec::new();
                let mut rows = Vec::new();
                for row in node.children() {
                    let is_header = matches!(row.data.borrow().value, NodeValue::TableRow(true));
                    let cells: Vec<Vec<Inline>> =
                        row.children().map(|cell| self.inlines(cell)).collect();
                    self.reject_flow_images(line);
                    if is_header {
                        header = cells;
                    } else {
                        rows.push(cells);
                    }
                }
                Some(Block::Table(Table {
                    alignments: table
                        .alignments
                        .iter()
                        .map(|a| match a {
                            TableAlignment::Center => Align::Center,
                            TableAlignment::Right => Align::Right,
                            TableAlignment::Left | TableAlignment::None => Align::Left,
                        })
                        .collect(),
                    header,
                    rows,
                }))
            }
            NodeValue::HtmlBlock(html) => {
                self.html(&html.literal, line);
                None
            }
            NodeValue::ThematicBreak => None,
            other => {
                self.warn(line, format!("未対応のブロックです: {}", node_name(&other)));
                None
            }
        }
    }

    fn code_block(&mut self, info: &str, literal: &str, line: usize) -> Block {
        let code = literal.strip_suffix('\n').unwrap_or(literal).to_owned();
        let mut tokens = info.split_whitespace();
        let lang = tokens.next().filter(|t| !t.starts_with('{'));
        if lang == Some("mermaid") {
            return Block::Diagram {
                kind: DiagramKind::Mermaid,
                source: code,
            };
        }
        let mut block = CodeBlock {
            lang: lang.map(str::to_owned),
            code,
            ..CodeBlock::default()
        };
        // 言語名が省略されて `{..}` から始まる場合も、同じ規則で読む。
        let rest = info.split_whitespace().skip(usize::from(lang.is_some()));
        for token in rest {
            if token == "numbered" {
                block.numbered = true;
            } else if let Some(lines) = token
                .strip_prefix('{')
                .and_then(|t| t.strip_suffix('}'))
                .and_then(parse_line_ranges)
            {
                block.highlight_lines = lines;
            } else {
                self.warn(line, format!("未知のコードブロック指定: {token}"));
            }
        }
        Block::Code(block)
    }

    /// 走査中に見つけた画像のうち、背景・片側・絶対配置のものをスライドに登録し、
    /// 本文の流れに置くものだけを返す。
    fn place_images(&mut self, line: usize) -> Vec<Image> {
        let mut flow = Vec::new();
        for image in std::mem::take(&mut self.pending_images) {
            match image.placement {
                Placement::Flow => flow.push(image),
                Placement::Background { .. } => self.backgrounds.push(image),
                Placement::At { .. } => self.floats.push(image),
                Placement::Side(_) => {
                    if self.side.replace(image).is_some() {
                        self.warn(line, "side の画像は 1 スライドに 1 枚までです");
                    }
                }
            }
        }
        flow
    }

    /// 見出しや表のセルなど、流れの中の画像を置けない場所での後始末。
    fn reject_flow_images(&mut self, line: usize) {
        if !self.place_images(line).is_empty() {
            self.warn(line, "ここには画像を置けません (画像だけの段落にしてください)");
        }
    }

    /// 画像の alt テキストに並べた指定を読む。`key:value` 形式でない語は alt として残す。
    fn image(&mut self, url: &str, title: &str, alt: &str, line: usize) -> Image {
        let mut image = Image {
            src: url.to_owned(),
            alt: String::new(),
            caption: (!title.is_empty()).then(|| title.to_owned()),
            width: None,
            height: None,
            placement: Placement::Flow,
            align: None,
            crop: None,
            round: Num(0.0),
            shadow: false,
            opacity: Num(1.0),
        };
        let mut background = false;
        let mut fit = Fit::Cover;
        let mut split = None;
        let mut at = None;
        let mut anchor = Anchor::TopLeft;
        let mut side = None;
        let mut words = Vec::new();
        for token in alt.split_whitespace() {
            let (key, value) = token.split_once(':').unwrap_or((token, ""));
            let ok = match (key, value) {
                ("bg", "") => {
                    background = true;
                    true
                }
                ("cover", "") => {
                    fit = Fit::Cover;
                    true
                }
                ("contain" | "fit", "") => {
                    fit = Fit::Contain;
                    true
                }
                ("shadow", "") => {
                    image.shadow = true;
                    true
                }
                ("left" | "right", _) => {
                    let which = if key == "left" { Side::Left } else { Side::Right };
                    let percent = if value.is_empty() {
                        Some(50.0)
                    } else {
                        value.strip_suffix('%').and_then(|v| v.parse::<f64>().ok())
                    };
                    percent
                        .filter(|p| (1.0..=99.0).contains(p))
                        .map(|p| split = Some((which, Num(p))))
                        .is_some()
                }
                ("w", _) => parse_length(value).map(|l| image.width = Some(l)).is_some(),
                ("h", _) => parse_length(value).map(|l| image.height = Some(l)).is_some(),
                ("at", _) => value
                    .split_once(',')
                    .and_then(|(x, y)| Some((parse_length(x)?, parse_length(y)?)))
                    .map(|position| at = Some(position))
                    .is_some(),
                ("anchor", _) => parse_anchor(value).map(|a| anchor = a).is_some(),
                ("align", _) => match value {
                    "left" => Some(Align::Left),
                    "center" => Some(Align::Center),
                    "right" => Some(Align::Right),
                    _ => None,
                }
                .map(|a| image.align = Some(a))
                .is_some(),
                ("side", "left") => {
                    side = Some(Side::Left);
                    true
                }
                ("side", "right") => {
                    side = Some(Side::Right);
                    true
                }
                ("crop", _) => {
                    let parts: Vec<Length> = value.split(',').filter_map(parse_length).collect();
                    <[Length; 4]>::try_from(parts)
                        .map(|crop| image.crop = Some(crop))
                        .is_ok()
                }
                ("round", _) => value.parse().map(|r| image.round = Num(r)).is_ok(),
                ("opacity", _) => value
                    .parse::<f64>()
                    .ok()
                    .filter(|o| (0.0..=1.0).contains(o))
                    .map(|o| image.opacity = Num(o))
                    .is_some(),
                // `key:value` の形でない語は、ただの代替テキスト。
                (_, "") if !token.contains(':') => {
                    words.push(token);
                    true
                }
                _ => false,
            };
            if !ok {
                self.warn(line, format!("画像の指定を解釈できません: {token}"));
            }
        }
        image.alt = words.join(" ");
        image.placement = if background {
            Placement::Background { fit, split }
        } else if let Some((x, y)) = at {
            Placement::At { x, y, anchor }
        } else if let Some(side) = side {
            Placement::Side(side)
        } else {
            if split.is_some() {
                self.warn(line, "left / right は bg と組み合わせて使います");
            }
            Placement::Flow
        };
        image
    }

    fn inlines<'a>(&mut self, node: &'a AstNode<'a>) -> Vec<Inline> {
        let mut out = Vec::new();
        for child in node.children() {
            let line = line_of(child);
            let value = child.data.borrow().value.clone();
            match value {
                NodeValue::Text(text) => out.push(Inline::Text(text.into_owned())),
                NodeValue::Code(code) => out.push(Inline::Code(code.literal)),
                NodeValue::Emph => out.push(Inline::Emph(self.inlines(child))),
                NodeValue::Strong => out.push(Inline::Strong(self.inlines(child))),
                NodeValue::Strikethrough => out.push(Inline::Strike(self.inlines(child))),
                NodeValue::Link(link) => {
                    if link.url.starts_with('#') {
                        self.internal_links.push((link.url.clone(), line));
                    }
                    out.push(Inline::Link {
                        url: link.url,
                        children: self.inlines(child),
                    });
                }
                NodeValue::SoftBreak => out.push(Inline::SoftBreak),
                NodeValue::LineBreak => out.push(Inline::LineBreak),
                NodeValue::HtmlInline(html) => self.html(&html, line),
                NodeValue::Image(link) => {
                    let alt = plain_text(child);
                    let image = self.image(&link.url, &link.title, &alt, line);
                    self.pending_images.push(image);
                }
                other => {
                    self.warn(line, format!("未対応のインライン要素です: {}", node_name(&other)));
                    out.extend(self.inlines(child));
                }
            }
        }
        // コメントだけを取り除いた結果、行頭・行末に残った改行は捨てる。
        while matches!(out.first(), Some(Inline::SoftBreak | Inline::LineBreak)) {
            out.remove(0);
        }
        while matches!(out.last(), Some(Inline::SoftBreak | Inline::LineBreak)) {
            out.pop();
        }
        out
    }
}

fn line_of<'a>(node: &'a AstNode<'a>) -> usize {
    node.data.borrow().sourcepos.start.line
}

/// ノード以下のテキストをつなげる (画像の alt 用)。
fn plain_text<'a>(node: &'a AstNode<'a>) -> String {
    let mut out = String::new();
    for child in node.descendants() {
        match &child.data.borrow().value {
            NodeValue::Text(text) => out.push_str(text),
            NodeValue::Code(code) => out.push_str(&code.literal),
            NodeValue::SoftBreak | NodeValue::LineBreak => out.push(' '),
            _ => {}
        }
    }
    out
}

/// `300`、`300px`、`40%` を長さとして読む。
fn parse_length(text: &str) -> Option<Length> {
    let text = text.trim();
    if let Some(percent) = text.strip_suffix('%') {
        return percent.parse().ok().map(|p| Length::Percent(Num(p)));
    }
    text.strip_suffix("px")
        .unwrap_or(text)
        .parse()
        .ok()
        .map(|px| Length::Px(Num(px)))
}

fn parse_anchor(text: &str) -> Option<Anchor> {
    Some(match text {
        "top-left" => Anchor::TopLeft,
        "top" => Anchor::Top,
        "top-right" => Anchor::TopRight,
        "left" => Anchor::Left,
        "center" => Anchor::Center,
        "right" => Anchor::Right,
        "bottom-left" => Anchor::BottomLeft,
        "bottom" => Anchor::Bottom,
        "bottom-right" => Anchor::BottomRight,
        _ => return None,
    })
}

/// 診断メッセージ用のノード種別名 (`Image(..)` → `Image`)。
fn node_name(value: &NodeValue) -> String {
    let debug = format!("{value:?}");
    debug
        .split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// `2,4-6` 形式を行番号の列にする。
fn parse_line_ranges(spec: &str) -> Option<Vec<usize>> {
    let mut lines = Vec::new();
    for part in spec.split(',') {
        match part.split_once('-') {
            Some((start, end)) => {
                let start: usize = start.trim().parse().ok()?;
                let end: usize = end.trim().parse().ok()?;
                lines.extend(start..=end);
            }
            None => lines.push(part.trim().parse().ok()?),
        }
    }
    Some(lines)
}
