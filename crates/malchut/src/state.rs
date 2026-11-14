//! ウィンドウに依存しないアプリの状態。全ウィンドウがこれを共有する。

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use malchut_core::layout::{DisplayList, Layouter};
use malchut_core::model::{Deck, DeckMeta, LinkTarget, Severity, Slide};
use malchut_core::parse::parse;
use malchut_core::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Next,
    Prev,
    First,
    Last,
    /// 0 始まりのスライド位置。
    Goto(usize),
    /// ファイルの変更に追従する再読み込み。
    Refresh,
    /// キャッシュを捨てる明示的な再読み込み。
    Reload,
    TimerToggle,
    TimerReset,
    NextTheme,
    PrevTheme,
}

#[derive(Debug, Default)]
pub struct Timer {
    accumulated: Duration,
    started: Option<Instant>,
}

impl Timer {
    pub fn toggle(&mut self) {
        match self.started.take() {
            Some(started) => self.accumulated += started.elapsed(),
            None => self.started = Some(Instant::now()),
        }
    }

    pub fn reset(&mut self) {
        *self = Timer::default();
    }

    pub fn running(&self) -> bool {
        self.started.is_some()
    }

    pub fn elapsed(&self) -> Duration {
        self.accumulated + self.started.map_or(Duration::ZERO, |s| s.elapsed())
    }
}

pub struct AppState {
    path: PathBuf,
    /// front matter より優先するテーマ名 (コマンドラインやキー操作で指定)。
    theme_override: Option<String>,
    layouter: Layouter,
    deck: Deck,
    theme: Theme,
    lists: Vec<Rc<DisplayList>>,
    /// スライドの内容とテーマから求めたキーで引くレイアウト結果。
    cache: HashMap<u64, Rc<DisplayList>>,
    current: usize,
    /// 直近の読み込みで起きたエラー。表示中の内容は直前の正常なもの。
    error: Option<String>,
    /// 直近にレイアウトを始めた時刻。これより新しい画像は読み直す。
    loaded_at: SystemTime,
    /// 変更されたスライドへ自動で移動するか。
    pub follow_changes: bool,
    pub timer: Timer,
}

impl AppState {
    pub fn open(path: &Path, theme_override: Option<String>) -> AppState {
        let mut state = AppState {
            path: path.to_path_buf(),
            theme_override,
            layouter: Layouter::new(),
            deck: Deck {
                meta: DeckMeta::default(),
                slides: Vec::new(),
                diagnostics: Vec::new(),
            },
            theme: Theme::builtin("default").expect("既定テーマは常に存在する"),
            lists: Vec::new(),
            cache: HashMap::new(),
            current: 0,
            error: None,
            loaded_at: SystemTime::UNIX_EPOCH,
            follow_changes: false,
            timer: Timer::default(),
        };
        state.load();
        state
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn deck_dir(&self) -> &Path {
        match self.path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        }
    }

    pub fn len(&self) -> usize {
        self.lists.len()
    }

    pub fn current(&self) -> usize {
        self.current
    }

    pub fn list(&self, index: usize) -> Option<&Rc<DisplayList>> {
        self.lists.get(index)
    }

    pub fn slide(&self, index: usize) -> Option<&Slide> {
        self.deck.slides.get(index)
    }

    pub fn link_target<'a>(&self, url: &'a str) -> Option<LinkTarget<'a>> {
        self.deck.link_target(url)
    }

    /// 表示中のスライドが参照している画像ファイル。
    pub fn assets(&self) -> HashSet<PathBuf> {
        self.lists
            .iter()
            .flat_map(|list| list.assets.iter().cloned())
            .collect()
    }

    pub fn theme_name(&self) -> &str {
        &self.theme.name
    }

    /// 画面に出すメッセージ (読み込みエラー、続いて警告)。
    pub fn messages(&self) -> Vec<String> {
        let warnings = self
            .deck
            .diagnostics
            .iter()
            .map(|d| format!("{} 行目: {}", d.line, d.message));
        self.error.iter().cloned().chain(warnings).collect()
    }

    pub fn apply(&mut self, action: Action) {
        let last = self.len().saturating_sub(1);
        match action {
            Action::Next => self.current = (self.current + 1).min(last),
            Action::Prev => self.current = self.current.saturating_sub(1),
            Action::First => self.current = 0,
            Action::Last => self.current = last,
            Action::Goto(index) => self.current = index.min(last),
            Action::Refresh => self.load(),
            Action::Reload => {
                self.cache.clear();
                self.layouter = Layouter::new();
                self.load();
            }
            Action::TimerToggle => self.timer.toggle(),
            Action::TimerReset => self.timer.reset(),
            Action::NextTheme => self.cycle_theme(1),
            Action::PrevTheme => self.cycle_theme(-1),
        }
    }

    fn cycle_theme(&mut self, step: isize) {
        let names = Theme::available(self.deck_dir());
        let position = names.iter().position(|n| *n == self.theme.name).unwrap_or(0);
        let next = (position as isize + step).rem_euclid(names.len() as isize) as usize;
        self.theme_override = Some(names[next].clone());
        self.load();
    }

    /// ファイルとテーマを読み直す。失敗したら表示中の内容を保ち、エラーだけ記録する。
    fn load(&mut self) {
        self.error = None;
        let source = match std::fs::read_to_string(&self.path) {
            Ok(source) => source,
            Err(err) => {
                self.error = Some(format!("{} を読めません: {err}", self.path.display()));
                return;
            }
        };
        let deck = parse(&source);
        if let Some(error) = deck
            .diagnostics
            .iter()
            .find(|d| d.severity == Severity::Error)
        {
            self.error = Some(format!("{} 行目: {}", error.line, error.message));
            return;
        }
        let name = self
            .theme_override
            .as_deref()
            .or(deck.meta.theme.as_deref())
            .unwrap_or("default");
        match Theme::resolve(name, self.deck_dir()) {
            Ok(theme) => self.theme = theme,
            // テーマだけが壊れている場合は、直前のテーマのまま内容を更新する。
            Err(err) => self.error = Some(err.to_string()),
        }

        let changed = deck
            .slides
            .iter()
            .zip(&self.deck.slides)
            .position(|(new, old)| new != old)
            .or((deck.slides.len() > self.deck.slides.len()).then_some(self.deck.slides.len()));
        let had_slides = !self.deck.slides.is_empty();
        self.deck = deck;
        self.relayout();
        if let (true, true, Some(changed)) = (self.follow_changes, had_slides, changed) {
            self.current = changed;
        }
        self.current = self.current.min(self.len().saturating_sub(1));
    }

    fn relayout(&mut self) {
        let started = SystemTime::now();
        let mut cache = HashMap::new();
        let mut lists = Vec::with_capacity(self.deck.slides.len());
        for (index, slide) in self.deck.slides.iter().enumerate() {
            let mut hasher = DefaultHasher::new();
            (slide, self.theme.fingerprint(), self.deck.meta.size, index).hash(&mut hasher);
            let key = hasher.finish();
            // 参照している画像が書き換わっていたら、内容が同じスライドも組み直す。
            let reusable = self.cache.get(&key).filter(|list| {
                list.assets.iter().all(|path| modified(path) <= Some(self.loaded_at))
            });
            let list = reusable.cloned().unwrap_or_else(|| {
                let style = self.theme.style(&slide.props.class);
                Rc::new(self.layouter.layout(
                    slide,
                    &style,
                    self.deck.meta.size,
                    index + 1,
                    self.deck_dir(),
                ))
            });
            cache.insert(key, list.clone());
            lists.push(list);
        }
        // 今回使わなかったレイアウト結果は、ここで捨てる。
        self.cache = cache;
        self.lists = lists;
        self.loaded_at = started;
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deck_file(name: &str, content: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("malchut-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("deck.md");
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn navigation_is_clamped() {
        let path = deck_file("nav", "# a\n\n---\n\n# b\n\n---\n\n# c\n");
        let mut state = AppState::open(&path, None);
        assert_eq!(state.len(), 3);
        state.apply(Action::Prev);
        assert_eq!(state.current(), 0);
        state.apply(Action::Goto(99));
        assert_eq!(state.current(), 2);
        state.apply(Action::Next);
        assert_eq!(state.current(), 2);
        state.apply(Action::First);
        assert_eq!(state.current(), 0);
    }

    #[test]
    fn refresh_reuses_unchanged_layouts_and_follows_the_change() {
        let path = deck_file("refresh", "# a\n\n---\n\n# b\n");
        let mut state = AppState::open(&path, None);
        state.follow_changes = true;
        let first = state.list(0).unwrap().clone();
        let second = state.list(1).unwrap().clone();

        std::fs::write(&path, "# a\n\n---\n\n# changed\n").unwrap();
        state.apply(Action::Refresh);
        assert!(Rc::ptr_eq(&first, state.list(0).unwrap()));
        assert!(!Rc::ptr_eq(&second, state.list(1).unwrap()));
        assert_eq!(state.current(), 1);

        state.apply(Action::Reload);
        assert!(!Rc::ptr_eq(&first, state.list(0).unwrap()));
    }

    #[test]
    fn broken_front_matter_keeps_the_last_good_deck() {
        let path = deck_file("broken", "# a\n\n---\n\n# b\n");
        let mut state = AppState::open(&path, None);
        std::fs::write(&path, "---\ntheme: [\n---\n\n# only\n").unwrap();
        state.apply(Action::Refresh);
        assert_eq!(state.len(), 2);
        assert_eq!(state.messages().len(), 1);

        std::fs::write(&path, "# only\n").unwrap();
        state.apply(Action::Refresh);
        assert_eq!(state.len(), 1);
        assert!(state.messages().is_empty());
    }

    #[test]
    fn themes_cycle() {
        let path = deck_file("theme", "# a\n");
        let mut state = AppState::open(&path, None);
        assert_eq!(state.theme_name(), "default");
        state.apply(Action::NextTheme);
        assert_eq!(state.theme_name(), "dark");
        state.apply(Action::PrevTheme);
        assert_eq!(state.theme_name(), "default");
    }

    #[test]
    fn timer_accumulates_only_while_running() {
        let mut timer = Timer::default();
        assert!(!timer.running());
        timer.toggle();
        std::thread::sleep(Duration::from_millis(20));
        timer.toggle();
        let paused = timer.elapsed();
        assert!(paused >= Duration::from_millis(20));
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(timer.elapsed(), paused);
        timer.reset();
        assert_eq!(timer.elapsed(), Duration::ZERO);
    }
}
