//! syntect によるコードハイライト。

use std::sync::LazyLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme};
use syntect::parsing::SyntaxSet;
use two_face::theme::{EmbeddedLazyThemeSet, EmbeddedThemeName};

use crate::theme::Color;

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);
static THEMES: LazyLock<EmbeddedLazyThemeSet> = LazyLock::new(two_face::theme::extra);

#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub text: String,
    pub color: Color,
    pub bold: bool,
    pub italic: bool,
}

fn find_theme(name: &str) -> &'static Theme {
    let embedded = EmbeddedLazyThemeSet::theme_names()
        .iter()
        .copied()
        .find(|t| t.as_name().eq_ignore_ascii_case(name))
        .unwrap_or(EmbeddedThemeName::InspiredGithub);
    THEMES.get(embedded)
}

/// コードを行ごとの色付きスパンにする。未知の言語はプレーンテキストとして扱う。
pub fn highlight(code: &str, lang: Option<&str>, theme_name: &str) -> Vec<Vec<Span>> {
    let theme = find_theme(theme_name);
    let syntax = lang
        .and_then(|lang| SYNTAXES.find_syntax_by_token(lang))
        .unwrap_or_else(|| SYNTAXES.find_syntax_plain_text());
    let mut highlighter = HighlightLines::new(syntax, theme);
    let fallback = theme
        .settings
        .foreground
        .map_or(Color::from_rgb8(0, 0, 0), |c| Color::from_rgb8(c.r, c.g, c.b));

    code.split('\n')
        .map(|line| {
            // 構文定義が改行込みの行を前提にしているので、解析時だけ改行を足す。
            let with_newline = format!("{line}\n");
            match highlighter.highlight_line(&with_newline, &SYNTAXES) {
                Ok(ranges) => ranges
                    .into_iter()
                    .map(|(style, text)| Span {
                        text: text.trim_end_matches('\n').to_owned(),
                        color: Color::from_rgb8(
                            style.foreground.r,
                            style.foreground.g,
                            style.foreground.b,
                        ),
                        bold: style.font_style.contains(FontStyle::BOLD),
                        italic: style.font_style.contains(FontStyle::ITALIC),
                    })
                    .filter(|span| !span.text.is_empty())
                    .collect(),
                Err(_) => vec![Span {
                    text: line.to_owned(),
                    color: fallback,
                    bold: false,
                    italic: false,
                }],
            }
        })
        .collect()
}
