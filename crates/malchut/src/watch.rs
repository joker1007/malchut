//! ファイルの変更監視。変更をまとめて、メインループ上でコールバックを呼ぶ。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gtk4::glib;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// 連続した変更を 1 回にまとめるための待ち時間。
const DEBOUNCE: Duration = Duration::from_millis(80);

/// 動作中の監視。破棄すると監視が止まる。
pub struct Watch {
    watcher: RecommendedWatcher,
    /// スライドが参照している画像。監視スレッドと共有する。
    assets: Arc<Mutex<HashSet<PathBuf>>>,
    /// 画像のために監視を始めたディレクトリ。
    asset_dirs: HashSet<PathBuf>,
}

impl Watch {
    /// 監視する画像を入れ替える。画像のあるディレクトリは、初めて現れたときに監視へ加える。
    pub fn set_assets(&mut self, assets: HashSet<PathBuf>) {
        for dir in assets.iter().filter_map(|path| path.parent()) {
            if dir.is_dir() && self.asset_dirs.insert(dir.to_path_buf()) {
                let _ = self.watcher.watch(dir, RecursiveMode::NonRecursive);
            }
        }
        if let Ok(mut current) = self.assets.lock() {
            *current = assets;
        }
    }
}

/// `deck` と `theme_dirs` 以下の変更を監視する。
pub fn watch(
    deck: &Path,
    theme_dirs: &[PathBuf],
    on_change: impl Fn() + 'static,
) -> notify::Result<Watch> {
    let (sender, receiver) = async_channel::unbounded::<()>();
    let assets: Arc<Mutex<HashSet<PathBuf>>> = Arc::default();
    let watched_assets = assets.clone();
    let deck_name = deck.file_name().map(|name| name.to_owned());
    let themes: Vec<PathBuf> = theme_dirs.to_vec();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        // 読み取りアクセスまで拾うと、再読み込みが自分自身を呼び続ける。
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        let relevant = event.paths.iter().any(|path| {
            path.file_name() == deck_name.as_deref()
                || themes.iter().any(|dir| path.starts_with(dir))
                || watched_assets.lock().is_ok_and(|assets| assets.contains(path))
        });
        if relevant {
            let _ = sender.try_send(());
        }
    })?;

    // エディタは別名で書いてから rename することがあるので、ファイルではなく親ディレクトリを見る。
    let deck_dir = match deck.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    watcher.watch(deck_dir, RecursiveMode::NonRecursive)?;
    for dir in theme_dirs.iter().filter(|dir| dir.is_dir()) {
        watcher.watch(dir, RecursiveMode::Recursive)?;
    }

    glib::spawn_future_local(async move {
        while receiver.recv().await.is_ok() {
            glib::timeout_future(DEBOUNCE).await;
            while receiver.try_recv().is_ok() {}
            on_change();
        }
    });
    Ok(Watch {
        watcher,
        assets,
        asset_dirs: HashSet::new(),
    })
}
