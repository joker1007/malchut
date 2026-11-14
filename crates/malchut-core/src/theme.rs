//! テーマの読み込みと、スライドごとのスタイル解決。
//!
//! テーマは TOML で、組み込みの既定テーマへの差分として扱う。`[class.<name>]` 以下は
//! そのクラスを持つスライドだけに重ねる上書き分。

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::Table;

use crate::model::Align;

const DEFAULT_TOML: &str = include_str!("../themes/default.toml");
const DARK_TOML: &str = include_str!("../themes/dark.toml");

pub const BUILTIN_THEMES: &[&str] = &["default", "dark"];

#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("テーマ \"{0}\" が見つかりません")]
    NotFound(String),
    #[error("テーマファイル {path} を読めません: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("テーマ \"{name}\" の内容が不正です: {source}")]
    Invalid {
        name: String,
        source: Box<toml::de::Error>,
    },
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    /// 既定テーマに差分を重ねた結果。`class` テーブルを含む。
    table: Table,
    base: Style,
    fingerprint: u64,
}

impl Theme {
    pub fn builtin(name: &str) -> Option<Theme> {
        let overlay = match name {
            "default" => "",
            "dark" => DARK_TOML,
            _ => return None,
        };
        Some(Self::from_overlay(name, overlay).expect("組み込みテーマは常に有効"))
    }

    /// 名前からテーマを探す。組み込み、`<deck_dir>/themes/<name>/theme.toml`、
    /// `$XDG_CONFIG_HOME/malchut/themes/<name>/theme.toml` の順。
    pub fn resolve(name: &str, deck_dir: &Path) -> Result<Theme, ThemeError> {
        if let Some(theme) = Self::builtin(name) {
            return Ok(theme);
        }
        for dir in theme_dirs(deck_dir) {
            let path = dir.join(name).join("theme.toml");
            if path.is_file() {
                let text = std::fs::read_to_string(&path)
                    .map_err(|source| ThemeError::Io { path, source })?;
                return Self::from_overlay(name, &text);
            }
        }
        Err(ThemeError::NotFound(name.to_owned()))
    }

    /// 選択できるテーマ名の一覧 (組み込み、続いてディレクトリにあるものを名前順)。
    pub fn available(deck_dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = BUILTIN_THEMES.iter().map(|&n| n.to_owned()).collect();
        let mut found: Vec<String> = theme_dirs(deck_dir)
            .iter()
            .filter_map(|dir| std::fs::read_dir(dir).ok())
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().join("theme.toml").is_file())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| !names.contains(name))
            .collect();
        found.sort();
        found.dedup();
        names.extend(found);
        names
    }

    /// 内容が同じテーマなら同じ値になる識別子 (レイアウト結果のキャッシュ用)。
    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    fn from_overlay(name: &str, overlay: &str) -> Result<Theme, ThemeError> {
        let invalid = |source: toml::de::Error| ThemeError::Invalid {
            name: name.to_owned(),
            source: Box::new(source),
        };
        let mut table: Table = DEFAULT_TOML.parse().expect("既定テーマは常に有効");
        merge(&mut table, overlay.parse().map_err(invalid)?);
        let base = style_of(&table, &[]).map_err(invalid)?;
        let mut hasher = DefaultHasher::new();
        table.to_string().hash(&mut hasher);
        let theme = Theme {
            name: name.to_owned(),
            table,
            base,
            fingerprint: hasher.finish(),
        };
        // クラスごとの上書きも読み込み時に検証し、描画時には失敗しないようにする。
        for class in theme.class_names() {
            style_of(&theme.table, &[class]).map_err(invalid)?;
        }
        Ok(theme)
    }

    fn class_names(&self) -> Vec<String> {
        match self.table.get("class") {
            Some(toml::Value::Table(classes)) => classes.keys().cloned().collect(),
            _ => Vec::new(),
        }
    }

    /// 指定クラスの上書きを順に重ねたスタイル。
    pub fn style(&self, classes: &[String]) -> Style {
        if classes.is_empty() {
            return self.base.clone();
        }
        style_of(&self.table, classes).unwrap_or_else(|_| self.base.clone())
    }
}

/// テーマを探すディレクトリ (`<deck_dir>/themes`、`$XDG_CONFIG_HOME/malchut/themes`)。
pub fn theme_dirs(deck_dir: &Path) -> Vec<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    [Some(deck_dir.to_path_buf()), config_home.map(|c| c.join("malchut"))]
        .into_iter()
        .flatten()
        .map(|dir| dir.join("themes"))
        .collect()
}

fn style_of(table: &Table, classes: &[String]) -> Result<Style, toml::de::Error> {
    let mut resolved = table.clone();
    let class_table = resolved.remove("class");
    if let Some(toml::Value::Table(class_table)) = class_table {
        for class in classes {
            if let Some(toml::Value::Table(overlay)) = class_table.get(class) {
                merge(&mut resolved, overlay.clone());
            }
        }
    }
    resolved.try_into()
}

/// `overlay` を `base` に再帰的に重ねる。テーブル以外の値は置き換える。
fn merge(base: &mut Table, overlay: Table) {
    for (key, value) in overlay {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(base_child)), toml::Value::Table(child)) => {
                merge(base_child, child)
            }
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Color {
    pub fn from_rgb8(r: u8, g: u8, b: u8) -> Color {
        Color {
            r: f64::from(r) / 255.0,
            g: f64::from(g) / 255.0,
            b: f64::from(b) / 255.0,
            a: 1.0,
        }
    }

    /// `#rrggbb` または `#rrggbbaa`。
    pub fn parse(text: &str) -> Option<Color> {
        let hex = text.strip_prefix('#')?;
        if !matches!(hex.len(), 6 | 8) || !hex.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        let mut color = Color::from_rgb8(byte(0)?, byte(2)?, byte(4)?);
        if hex.len() == 8 {
            color.a = f64::from(byte(6)?) / 255.0;
        }
        Some(color)
    }

    /// Pango マークアップ用の `#rrggbb` 表記 (アルファは落とす)。
    pub fn to_hex(self) -> String {
        let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!("#{:02x}{:02x}{:02x}", byte(self.r), byte(self.g), byte(self.b))
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Color::parse(&text).ok_or_else(|| {
            serde::de::Error::custom(format!("色は #rrggbb か #rrggbbaa で指定してください: {text}"))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VAlign {
    Top,
    Center,
    Bottom,
}

/// あるスライドに適用する、解決済みのスタイル。
#[derive(Debug, Clone, Deserialize)]
pub struct Style {
    pub palette: Palette,
    pub fonts: Fonts,
    pub slide: SlideStyle,
    pub text: TextStyle,
    pub heading: HeadingStyle,
    pub list: ListStyle,
    pub code: CodeStyle,
    pub quote: QuoteStyle,
    pub table: TableStyle,
    pub footer: FooterStyle,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Palette {
    pub background: Color,
    pub foreground: Color,
    pub accent: Color,
    pub muted: Color,
    pub code_background: Color,
    pub line_highlight: Color,
    pub border: Color,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Fonts {
    pub body: String,
    pub heading: String,
    pub mono: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SlideStyle {
    pub padding_x: f64,
    pub padding_y: f64,
    pub align: Align,
    pub valign: VAlign,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TextStyle {
    pub size: f64,
    pub line_height: f64,
    pub block_spacing: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HeadingStyle {
    pub sizes: [f64; 6],
    pub weight: u32,
    pub spacing: f64,
    /// 省略時は `palette.foreground`。
    pub color: Option<Color>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListStyle {
    pub indent: f64,
    pub item_spacing: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CodeStyle {
    pub size: f64,
    pub min_size: f64,
    pub line_height: f64,
    pub padding: f64,
    pub radius: f64,
    /// syntect のテーマ名 (two-face 同梱のもの)。
    pub theme: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QuoteStyle {
    pub bar_width: f64,
    pub indent: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TableStyle {
    pub padding_x: f64,
    pub padding_y: f64,
    pub border_width: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FooterStyle {
    pub size: f64,
    pub margin: f64,
}
