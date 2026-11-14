mod state;
mod ui;
mod watch;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use malchut_core::model::{Deck, Severity};
use malchut_core::theme::Theme;

#[derive(Parser)]
#[command(
    version,
    about = "Markdown からスライドショーを作るツール",
    args_conflicts_with_subcommands = true,
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// 表示する Markdown ファイル
    input: Option<PathBuf>,
    /// front matter より優先するテーマ名
    #[arg(long)]
    theme: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// スライドを PDF (または PNG) に書き出す
    Export {
        /// 入力の Markdown ファイル
        input: PathBuf,
        /// 出力先。省略時は入力と同名の .pdf
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// front matter より優先するテーマ名
        #[arg(long)]
        theme: Option<String>,
        /// PDF の代わりに、スライドごとの PNG をこのディレクトリに書き出す
        #[arg(long, conflicts_with = "output")]
        png_dir: Option<PathBuf>,
    },
}

fn main() -> Result<ExitCode> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Export {
            input,
            output,
            theme,
            png_dir,
        }) => {
            let (deck, theme) = load(&input, theme.as_deref())?;
            let base_dir = deck_dir(&input);
            match png_dir {
                Some(dir) => malchut_core::export::export_png(&deck, &theme, base_dir, &dir, 1.0)?,
                None => {
                    let output = output.unwrap_or_else(|| input.with_extension("pdf"));
                    malchut_core::export::export_pdf(&deck, &theme, base_dir, &output)?;
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        None => {
            let input = cli.input.context("Markdown ファイルを指定してください")?;
            // 監視イベントのパスと突き合わせられるよう、絶対パスにしておく。
            let input = std::fs::canonicalize(&input)
                .with_context(|| format!("{} が見つかりません", input.display()))?;
            let code = ui::run(input, cli.theme);
            Ok(ExitCode::from(code.get()))
        }
    }
}

/// Markdown とテーマを読み込む。警告は標準エラーに出し、エラーがあれば失敗させる。
fn load(input: &Path, theme_override: Option<&str>) -> Result<(Deck, Theme)> {
    let source = std::fs::read_to_string(input)
        .with_context(|| format!("{} を読めません", input.display()))?;
    let deck = malchut_core::parse::parse(&source);
    let mut failed = false;
    for diagnostic in &deck.diagnostics {
        let label = match diagnostic.severity {
            Severity::Warning => "warning",
            Severity::Error => {
                failed = true;
                "error"
            }
        };
        eprintln!(
            "{}:{}: {label}: {}",
            input.display(),
            diagnostic.line,
            diagnostic.message
        );
    }
    if failed {
        bail!("{} にエラーがあります", input.display());
    }
    let name = theme_override
        .or(deck.meta.theme.as_deref())
        .unwrap_or("default");
    let theme = Theme::resolve(name, deck_dir(input))?;
    Ok((deck, theme))
}

/// Markdown ファイルのあるディレクトリ (テーマと画像を探す起点)。
fn deck_dir(input: &Path) -> &Path {
    match input.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    }
}
