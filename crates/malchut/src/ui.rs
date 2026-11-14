//! GTK のウィンドウ。状態は [`AppState`] に集約し、各ウィンドウはそれを映すだけにする。

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use malchut_core::layout::DisplayList;
use malchut_core::model::LinkTarget;
use malchut_core::paint::{paint, render_image};
use malchut_core::theme::theme_dirs;

use crate::state::{Action, AppState};
use crate::watch;

const CONTENT: &str = "content";
const OVERVIEW: &str = "overview";
/// サムネイルの幅 (px)。
const THUMBNAIL_WIDTH: i32 = 320;
/// 画面に並べるメッセージの最大件数。
const MAX_MESSAGES: usize = 5;

const CSS: &str = r#"
.malchut-slide { background: black; }
.malchut-banner {
    background: rgba(150, 30, 30, 0.92);
    color: white;
    padding: 8px 14px;
    font-size: 13pt;
}
.malchut-dark { background: #16181c; color: #e6edf3; }
.malchut-notes { font-size: 20pt; }
.malchut-timer { font-size: 44pt; font-weight: bold; font-feature-settings: "tnum"; }
.malchut-info { font-size: 22pt; font-feature-settings: "tnum"; }
.malchut-caption { font-size: 11pt; opacity: 0.7; }
"#;

/// ウィンドウの内容を状態に合わせて更新する関数。
type RefreshFn = Box<dyn Fn(&Shared, &AppState)>;

/// 描画済みサムネイルの置き場。キーはレイアウト結果のアドレスで、値に実体を持たせて
/// アドレスが再利用されないようにする。
type Thumbnails = Rc<RefCell<HashMap<usize, (Rc<DisplayList>, gtk::cairo::ImageSurface)>>>;

struct Shared {
    app: gtk::Application,
    state: RefCell<AppState>,
    main: OnceCell<Rc<Win>>,
    presenter: RefCell<Option<Rc<Win>>>,
    /// プレゼン中に全画面表示しているモニタ (スライド側, 発表者側)。
    monitors: RefCell<Option<(gdk::Monitor, gdk::Monitor)>>,
    inhibit_cookie: Cell<Option<u32>>,
    tick: RefCell<Option<glib::SourceId>>,
    watcher: RefCell<Option<watch::Watch>>,
}

struct Win {
    window: gtk::ApplicationWindow,
    /// 通常の内容とサムネイル一覧を切り替える。
    stack: gtk::Stack,
    overview: Overview,
    /// 番号指定ジャンプの入力途中の数字。
    digits: RefCell<String>,
    refresh: RefreshFn,
}

impl Win {
    fn overview_open(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some(OVERVIEW)
    }

    fn set_overview(&self, state: &AppState, open: bool) {
        if open {
            self.stack.set_visible_child_name(OVERVIEW);
            self.overview.focus(state.current());
        } else {
            self.stack.set_visible_child_name(CONTENT);
        }
    }
}

pub fn run(path: PathBuf, theme: Option<String>) -> glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id("dev.malchut.Malchut")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(move |app| activate(app, &path, theme.clone()));
    // コマンドライン引数は clap で解釈済みなので、GTK には渡さない。
    app.run_with_args::<&str>(&[])
}

fn activate(app: &gtk::Application, path: &std::path::Path, theme: Option<String>) {
    if let Some(display) = gdk::Display::default() {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let mut state = AppState::open(path, theme);
    state.follow_changes = true;
    let ctx = Rc::new(Shared {
        app: app.clone(),
        state: RefCell::new(state),
        main: OnceCell::new(),
        presenter: RefCell::new(None),
        monitors: RefCell::new(None),
        inhibit_cookie: Cell::new(None),
        tick: RefCell::new(None),
        watcher: RefCell::new(None),
    });
    let main = build_main(&ctx);
    main.window.present();
    let _ = ctx.main.set(main);
    refresh(&ctx);

    let watcher = {
        let state = ctx.state.borrow();
        let on_change = {
            let ctx = ctx.clone();
            move || dispatch(&ctx, Action::Refresh)
        };
        watch::watch(state.path(), &theme_dirs(state.deck_dir()), on_change)
    };
    match watcher {
        Ok(watcher) => *ctx.watcher.borrow_mut() = Some(watcher),
        Err(err) => eprintln!("ファイルの監視を開始できません: {err}"),
    }
    watch_assets(&ctx);
}

/// 表示中のスライドが参照している画像を、監視対象に反映する。
fn watch_assets(ctx: &Shared) {
    if let Some(watcher) = ctx.watcher.borrow_mut().as_mut() {
        watcher.set_assets(ctx.state.borrow().assets());
    }
}

fn dispatch(ctx: &Rc<Shared>, action: Action) {
    ctx.state.borrow_mut().apply(action);
    if matches!(action, Action::Refresh | Action::Reload) {
        watch_assets(ctx);
    }
    refresh(ctx);
}

fn windows(ctx: &Shared) -> Vec<Rc<Win>> {
    ctx.main
        .get()
        .cloned()
        .into_iter()
        .chain(ctx.presenter.borrow().clone())
        .collect()
}

fn refresh(ctx: &Rc<Shared>) {
    let state = ctx.state.borrow();
    for win in windows(ctx) {
        (win.refresh)(ctx, &state);
        win.overview.sync(ctx, &state);
    }
}

fn presenting(ctx: &Shared) -> bool {
    ctx.presenter.borrow().is_some()
}

/// スライド 1 枚を、領域に収まる最大の大きさで中央に描く。
fn draw_slide(cr: &gtk::cairo::Context, list: &DisplayList, width: f64, height: f64, mark: bool) {
    let scale = (width / list.width).min(height / list.height);
    cr.translate(
        (width - list.width * scale) / 2.0,
        (height - list.height * scale) / 2.0,
    );
    cr.scale(scale, scale);
    cr.rectangle(0.0, 0.0, list.width, list.height);
    cr.clip();
    let _ = paint(cr, list);
    if mark && list.overflow {
        // 内容が枠からあふれていることを、下端の赤い帯で示す。
        cr.set_source_rgba(0.9, 0.1, 0.1, 0.85);
        cr.rectangle(0.0, list.height - 14.0, list.width, 14.0);
        let _ = cr.fill();
    }
}

/// 現在位置から `offset` 枚先のスライドを表示する領域。
fn slide_area(ctx: &Rc<Shared>, offset: usize, mark_overflow: bool) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder().hexpand(true).vexpand(true).build();
    area.add_css_class("malchut-slide");
    {
        let ctx = ctx.clone();
        area.set_draw_func(move |_, cr, width, height| {
            let state = ctx.state.borrow();
            if let Some(list) = state.list(state.current() + offset) {
                let mark = mark_overflow && !presenting(&ctx);
                draw_slide(cr, list, f64::from(width), f64::from(height), mark);
            }
        });
    }

    // リンクの上ではポインタの形を変え、クリックでリンク先を開く。
    let motion = gtk::EventControllerMotion::new();
    {
        let (ctx, area) = (ctx.clone(), area.clone());
        motion.connect_motion(move |_, x, y| {
            let over_link = link_under(&ctx, &area, offset, x, y).is_some();
            area.set_cursor_from_name(over_link.then_some("pointer"));
        });
    }
    area.add_controller(motion);
    let click = gtk::GestureClick::new();
    {
        let (ctx, area) = (ctx.clone(), area.clone());
        click.connect_released(move |_, _, x, y| {
            if let Some(url) = link_under(&ctx, &area, offset, x, y) {
                open_link(&ctx, &area, &url);
            }
        });
    }
    area.add_controller(click);
    area
}

/// 表示領域上の位置 (px) にあるリンクのリンク先。
fn link_under(
    ctx: &Shared,
    area: &gtk::DrawingArea,
    offset: usize,
    x: f64,
    y: f64,
) -> Option<String> {
    let state = ctx.state.borrow();
    let list = state.list(state.current() + offset)?;
    // draw_slide と同じ変換を逆にたどり、論理座標に直す。
    let (width, height) = (f64::from(area.width()), f64::from(area.height()));
    let scale = (width / list.width).min(height / list.height);
    let slide_x = (x - (width - list.width * scale) / 2.0) / scale;
    let slide_y = (y - (height - list.height * scale) / 2.0) / scale;
    list.link_at(slide_x, slide_y).map(str::to_owned)
}

/// スライド内参照ならそのスライドへ移動し、外部リンクなら既定のアプリ (ブラウザなど) で開く。
fn open_link(ctx: &Rc<Shared>, area: &gtk::DrawingArea, url: &str) {
    let target = ctx.state.borrow().link_target(url);
    match target {
        Some(LinkTarget::Slide(index)) => dispatch(ctx, Action::Goto(index)),
        Some(LinkTarget::External(url)) => {
            // スキームの無いものは、Markdown ファイルからの相対パスとして扱う。
            let uri = match glib::Uri::peek_scheme(url) {
                Some(_) => url.to_owned(),
                None => {
                    let path = ctx.state.borrow().deck_dir().join(url);
                    gio::File::for_path(path).uri().to_string()
                }
            };
            let window = area.root().and_downcast::<gtk::Window>();
            gtk::UriLauncher::new(&uri).launch(
                window.as_ref(),
                gio::Cancellable::NONE,
                move |result| {
                    if let Err(err) = result {
                        eprintln!("リンクを開けません ({uri}): {err}");
                    }
                },
            );
        }
        None => {}
    }
}

struct Overview {
    scrolled: gtk::ScrolledWindow,
    flow: gtk::FlowBox,
    thumbnails: Thumbnails,
    /// 現在の子の構成 (枚数, サムネイルの高さ)。
    shape: Cell<(usize, i32)>,
}

impl Overview {
    fn new(ctx: &Rc<Shared>, stack: &gtk::Stack) -> Overview {
        let flow = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::Browse)
            .homogeneous(true)
            .valign(gtk::Align::Start)
            .min_children_per_line(1)
            .max_children_per_line(16)
            .row_spacing(12)
            .column_spacing(12)
            .margin_top(16)
            .margin_bottom(16)
            .margin_start(16)
            .margin_end(16)
            .build();
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&flow)
            .build();
        scrolled.add_css_class("malchut-dark");
        let ctx = ctx.clone();
        let stack = stack.clone();
        flow.connect_child_activated(move |_, child| {
            stack.set_visible_child_name(CONTENT);
            dispatch(&ctx, Action::Goto(child.index().max(0) as usize));
        });
        Overview {
            scrolled,
            flow,
            thumbnails: Rc::default(),
            shape: Cell::new((0, 0)),
        }
    }

    /// 状態に合わせてサムネイルを作り直す、または描き直す。
    fn sync(&self, ctx: &Rc<Shared>, state: &AppState) {
        let lists: Vec<&Rc<DisplayList>> = (0..state.len()).filter_map(|i| state.list(i)).collect();
        self.thumbnails
            .borrow_mut()
            .retain(|_, (list, _)| lists.iter().any(|current| Rc::ptr_eq(current, list)));

        let height = lists.first().map_or(0, |list| {
            (f64::from(THUMBNAIL_WIDTH) * list.height / list.width).round() as i32
        });
        let shape = (lists.len(), height);
        if self.shape.replace(shape) == shape {
            let mut child = self.flow.first_child();
            while let Some(widget) = child {
                // FlowBoxChild の中の描画領域まで再描画を伝える。
                if let Some(area) = thumbnail_area(&widget) {
                    area.queue_draw();
                }
                child = widget.next_sibling();
            }
            return;
        }

        self.flow.remove_all();
        for index in 0..lists.len() {
            let area = gtk::DrawingArea::builder()
                .content_width(THUMBNAIL_WIDTH)
                .content_height(height)
                .halign(gtk::Align::Center)
                .build();
            let ctx = ctx.clone();
            let thumbnails = self.thumbnails.clone();
            area.set_draw_func(move |area, cr, width, _| {
                let state = ctx.state.borrow();
                let Some(list) = state.list(index) else { return };
                let factor = f64::from(area.scale_factor());
                let pixel_width = (f64::from(width) * factor).round() as i32;
                let mut thumbnails = thumbnails.borrow_mut();
                let key = Rc::as_ptr(list) as usize;
                let stale = thumbnails
                    .get(&key)
                    .is_none_or(|(_, surface)| surface.width() != pixel_width);
                if stale {
                    let Ok(surface) = render_image(list, f64::from(pixel_width) / list.width) else {
                        return;
                    };
                    thumbnails.insert(key, (list.clone(), surface));
                }
                let (_, surface) = &thumbnails[&key];
                cr.scale(1.0 / factor, 1.0 / factor);
                let _ = cr.set_source_surface(surface, 0.0, 0.0);
                let _ = cr.paint();
            });
            let label = gtk::Label::new(Some(&(index + 1).to_string()));
            label.add_css_class("malchut-caption");
            let cell = gtk::Box::new(gtk::Orientation::Vertical, 4);
            cell.append(&area);
            cell.append(&label);
            self.flow.append(&cell);
        }
    }

    /// 指定のスライドを選択し、キー操作を受けられるようフォーカスを移す。
    fn focus(&self, index: usize) {
        if let Some(child) = self.flow.child_at_index(index as i32) {
            self.flow.select_child(&child);
            child.grab_focus();
        }
    }

    /// 選択中のスライド位置。
    fn selected(&self) -> Option<usize> {
        let child = self.flow.selected_children().into_iter().next()?;
        usize::try_from(child.index()).ok()
    }

    /// 選択を列方向に `dx`、行方向に `dy` だけ動かす。
    fn move_selection(&self, dx: i32, dy: i32) {
        let count = self.shape.get().0 as i32;
        if count == 0 {
            return;
        }
        let current = self.selected().unwrap_or(0) as i32;
        let target = (current + dx + dy * self.columns()).clamp(0, count - 1);
        self.focus(target as usize);
    }

    /// 1 行に並んでいるサムネイルの数 (先頭行と同じ高さにある子を数える)。
    fn columns(&self) -> i32 {
        let top_of = |widget: &gtk::Widget| widget.compute_bounds(&self.flow).map(|b| b.y());
        let Some(first_top) = self.flow.first_child().as_ref().and_then(top_of) else {
            return 1;
        };
        let mut columns = 0;
        let mut child = self.flow.first_child();
        while let Some(widget) = child {
            if top_of(&widget) != Some(first_top) {
                break;
            }
            columns += 1;
            child = widget.next_sibling();
        }
        columns.max(1)
    }
}

/// FlowBox の子 (FlowBoxChild → Box → DrawingArea) から描画領域を取り出す。
fn thumbnail_area(child: &gtk::Widget) -> Option<gtk::DrawingArea> {
    child
        .first_child()?
        .first_child()?
        .downcast::<gtk::DrawingArea>()
        .ok()
}

/// 内容とサムネイル一覧を持つウィンドウを組み立て、キー操作をつなぐ。
fn build_window(
    ctx: &Rc<Shared>,
    content: &impl IsA<gtk::Widget>,
    refresh: RefreshFn,
) -> Rc<Win> {
    let stack = gtk::Stack::new();
    let overview = Overview::new(ctx, &stack);
    stack.add_named(content, Some(CONTENT));
    stack.add_named(&overview.scrolled, Some(OVERVIEW));
    let window = gtk::ApplicationWindow::builder()
        .application(&ctx.app)
        .default_width(1280)
        .default_height(720)
        .child(&stack)
        .build();
    let win = Rc::new(Win {
        window,
        stack,
        overview,
        digits: RefCell::new(String::new()),
        refresh,
    });

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let ctx = ctx.clone();
    let weak = Rc::downgrade(&win);
    keys.connect_key_pressed(move |_, key, _, modifiers| match weak.upgrade() {
        Some(win) => on_key(&ctx, &win, key, modifiers),
        None => glib::Propagation::Proceed,
    });
    win.window.add_controller(keys);
    win
}

fn build_main(ctx: &Rc<Shared>) -> Rc<Win> {
    let area = slide_area(ctx, 0, true);
    let banner = gtk::Label::builder()
        .valign(gtk::Align::End)
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .build();
    banner.add_css_class("malchut-banner");
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&area));
    overlay.add_overlay(&banner);

    let window_slot: Rc<OnceCell<gtk::ApplicationWindow>> = Rc::default();
    let refresh = {
        let window_slot = window_slot.clone();
        Box::new(move |ctx: &Shared, state: &AppState| {
            area.queue_draw();
            let messages = state.messages();
            let shown: Vec<&str> = messages.iter().take(MAX_MESSAGES).map(String::as_str).collect();
            banner.set_text(&shown.join("\n"));
            // 発表中のスライド画面には、警告を出さない。
            banner.set_visible(!shown.is_empty() && !presenting(ctx));
            if let Some(window) = window_slot.get() {
                let name = state.path().file_name().unwrap_or_default().to_string_lossy();
                let title = format!("{name} ({}) [{}]", position(state), state.theme_name());
                window.set_title(Some(&title));
            }
        })
    };
    let win = build_window(ctx, &overlay, refresh);
    let _ = window_slot.set(win.window.clone());
    win
}

fn build_presenter(ctx: &Rc<Shared>) -> Rc<Win> {
    let caption = |text: &str| {
        let label = gtk::Label::builder().label(text).xalign(0.0).build();
        label.add_css_class("malchut-caption");
        label
    };
    let current = slide_area(ctx, 0, false);
    let next = slide_area(ctx, 1, false);
    next.set_hexpand(false);
    next.set_vexpand(false);
    next.set_content_height(300);

    let notes = gtk::Label::builder()
        .xalign(0.0)
        .yalign(0.0)
        .wrap(true)
        .build();
    notes.add_css_class("malchut-notes");
    let notes_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&notes)
        .build();

    let side = gtk::Box::new(gtk::Orientation::Vertical, 8);
    side.set_width_request(540);
    side.append(&caption("次のスライド"));
    side.append(&next);
    side.append(&caption("ノート"));
    side.append(&notes_scroll);

    let slides = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    slides.append(&current);
    slides.append(&side);

    let timer = gtk::Label::new(None);
    timer.add_css_class("malchut-timer");
    // ボタンがフォーカスを持つと Space がボタンに取られるので、フォーカスさせない。
    let button = |label: &str| {
        gtk::Button::builder()
            .label(label)
            .focusable(false)
            .focus_on_click(false)
            .valign(gtk::Align::Center)
            .build()
    };
    let toggle = button("開始");
    let reset = button("リセット");
    for (widget, action) in [(&toggle, Action::TimerToggle), (&reset, Action::TimerReset)] {
        let ctx = ctx.clone();
        widget.connect_clicked(move |_| dispatch(&ctx, action));
    }
    let page = gtk::Label::builder().hexpand(true).xalign(1.0).build();
    page.add_css_class("malchut-info");
    let clock = gtk::Label::new(None);
    clock.add_css_class("malchut-info");

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    bar.append(&timer);
    bar.append(&toggle);
    bar.append(&reset);
    bar.append(&page);
    bar.append(&clock);

    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();
    root.add_css_class("malchut-dark");
    root.append(&slides);
    root.append(&bar);

    // 時計とタイマーは、スライドの再描画とは別に定期更新する。
    let update_clock = {
        let (timer, clock) = (timer.clone(), clock.clone());
        Rc::new(move |state: &AppState| {
            let seconds = state.timer.elapsed().as_secs();
            timer.set_text(&format!(
                "{:02}:{:02}:{:02}",
                seconds / 3600,
                seconds / 60 % 60,
                seconds % 60
            ));
            let now = glib::DateTime::now_local().and_then(|now| now.format("%H:%M"));
            clock.set_text(now.as_deref().unwrap_or_default());
        })
    };
    let tick = {
        let (ctx, update_clock) = (ctx.clone(), update_clock.clone());
        glib::timeout_add_local(Duration::from_millis(250), move || {
            update_clock(&ctx.state.borrow());
            glib::ControlFlow::Continue
        })
    };
    *ctx.tick.borrow_mut() = Some(tick);

    let refresh = Box::new(move |_: &Shared, state: &AppState| {
        current.queue_draw();
        next.queue_draw();
        let text = state
            .slide(state.current())
            .map(|slide| slide.notes.join("\n\n"))
            .unwrap_or_default();
        notes.set_text(&text);
        toggle.set_label(if state.timer.running() { "一時停止" } else { "開始" });
        page.set_text(&position(state));
        update_clock(state);
    });
    let win = build_window(ctx, &root, refresh);
    win.window.set_title(Some("malchut 発表者画面"));
    let ctx = ctx.clone();
    win.window.connect_close_request(move |_| {
        stop_presentation(&ctx);
        glib::Propagation::Stop
    });
    win
}

/// `3 / 12` 形式の現在位置。
fn position(state: &AppState) -> String {
    format!("{} / {}", (state.current() + 1).min(state.len()), state.len())
}

fn start_presentation(ctx: &Rc<Shared>) {
    if presenting(ctx) {
        return;
    }
    let Some(main) = ctx.main.get() else { return };
    // 発表中は、ファイルが変わっても表示位置を勝手に動かさない。
    ctx.state.borrow_mut().follow_changes = false;
    let presenter = build_presenter(ctx);
    *ctx.presenter.borrow_mut() = Some(presenter.clone());
    presenter.window.present();

    // モニタが複数あれば、今いる側を発表者画面、もう一方をスライド画面にして全画面化する。
    // Wayland ではウィンドウ位置を指定できないので、モニタ指定の全画面化で配置する。
    let display = gtk::prelude::WidgetExt::display(&main.window);
    let monitors: Vec<gdk::Monitor> = display
        .monitors()
        .iter::<gdk::Monitor>()
        .filter_map(Result::ok)
        .collect();
    let here = main
        .window
        .surface()
        .and_then(|surface| display.monitor_at_surface(&surface))
        .or_else(|| monitors.first().cloned());
    let other = monitors.iter().find(|monitor| Some(*monitor) != here.as_ref());
    if let (Some(here), Some(other)) = (here, other) {
        main.window.fullscreen_on_monitor(other);
        presenter.window.fullscreen_on_monitor(&here);
        *ctx.monitors.borrow_mut() = Some((other.clone(), here));
    }

    let cookie = ctx.app.inhibit(
        Some(&main.window),
        gtk::ApplicationInhibitFlags::IDLE,
        Some("プレゼンテーション中"),
    );
    ctx.inhibit_cookie.set(Some(cookie));
    refresh(ctx);
}

fn stop_presentation(ctx: &Rc<Shared>) {
    let Some(presenter) = ctx.presenter.borrow_mut().take() else {
        return;
    };
    if let Some(tick) = ctx.tick.borrow_mut().take() {
        tick.remove();
    }
    presenter.window.destroy();
    if let Some(cookie) = ctx.inhibit_cookie.take() {
        ctx.app.uninhibit(cookie);
    }
    if ctx.monitors.borrow_mut().take().is_some()
        && let Some(main) = ctx.main.get()
    {
        main.window.unfullscreen();
    }
    ctx.state.borrow_mut().follow_changes = true;
    refresh(ctx);
}

/// スライド画面と発表者画面のモニタを入れ替える。
fn swap_screens(ctx: &Rc<Shared>) {
    let (Some(main), Some(presenter)) = (ctx.main.get(), ctx.presenter.borrow().clone()) else {
        return;
    };
    let mut monitors = ctx.monitors.borrow_mut();
    if let Some((audience, speaker)) = monitors.take() {
        main.window.fullscreen_on_monitor(&speaker);
        presenter.window.fullscreen_on_monitor(&audience);
        *monitors = Some((speaker, audience));
    }
}

fn on_key(
    ctx: &Rc<Shared>,
    win: &Rc<Win>,
    key: gdk::Key,
    modifiers: gdk::ModifierType,
) -> glib::Propagation {
    use glib::Propagation::{Proceed, Stop};

    let ctrl = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
    let shift = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
    let character = key.to_unicode();
    if ctrl {
        match character {
            Some('q') => ctx.app.quit(),
            Some('r') => dispatch(ctx, Action::Reload),
            _ => return Proceed,
        }
        return Stop;
    }

    // 番号指定ジャンプ: 数字を打って Enter。
    if let Some(digit) = character.filter(char::is_ascii_digit) {
        win.digits.borrow_mut().push(digit);
        return Stop;
    }
    let digits = std::mem::take(&mut *win.digits.borrow_mut());
    let overview = win.overview_open();
    if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter) {
        return match digits.parse::<usize>() {
            Ok(number) => {
                win.stack.set_visible_child_name(CONTENT);
                dispatch(ctx, Action::Goto(number.saturating_sub(1)));
                Stop
            }
            Err(_) => {
                // サムネイル一覧では、選択中のスライドへジャンプする。
                if let Some(index) = win.overview.selected().filter(|_| overview) {
                    win.stack.set_visible_child_name(CONTENT);
                    dispatch(ctx, Action::Goto(index));
                }
                Stop
            }
        };
    }

    if overview {
        let step = match (key, character) {
            (gdk::Key::Left, _) | (_, Some('h')) => Some((-1, 0)),
            (gdk::Key::Right, _) | (_, Some('l')) => Some((1, 0)),
            (gdk::Key::Up, _) | (_, Some('k')) => Some((0, -1)),
            (gdk::Key::Down, _) | (_, Some('j')) => Some((0, 1)),
            _ => None,
        };
        if let Some((dx, dy)) = step {
            win.overview.move_selection(dx, dy);
            return Stop;
        }
    }

    let action = match (key, character) {
        (gdk::Key::Right, _) if shift => Action::NextTheme,
        (gdk::Key::Left, _) if shift => Action::PrevTheme,
        (gdk::Key::Right | gdk::Key::Down | gdk::Key::Page_Down, _)
        | (_, Some(' ' | 'l' | 'j')) => Action::Next,
        (gdk::Key::Left | gdk::Key::Up | gdk::Key::Page_Up | gdk::Key::BackSpace, _)
        | (_, Some('h' | 'k')) => Action::Prev,
        (gdk::Key::Home, _) => Action::First,
        (gdk::Key::End, _) => Action::Last,
        (_, Some('t')) => Action::TimerToggle,
        (_, Some('T')) => Action::TimerReset,
        (_, Some('q')) => {
            ctx.app.quit();
            return Stop;
        }
        (_, Some('g')) => {
            win.set_overview(&ctx.state.borrow(), !overview);
            return Stop;
        }
        (gdk::Key::Escape, _) => {
            if overview {
                win.set_overview(&ctx.state.borrow(), false);
            } else if presenting(ctx) {
                stop_presentation(ctx);
            } else if win.window.is_fullscreen() {
                win.window.unfullscreen();
            }
            return Stop;
        }
        (gdk::Key::F5, _) => {
            start_presentation(ctx);
            return Stop;
        }
        (gdk::Key::F11, _) | (_, Some('f')) => {
            if win.window.is_fullscreen() {
                win.window.unfullscreen();
            } else {
                win.window.fullscreen();
            }
            return Stop;
        }
        (_, Some('s')) => {
            swap_screens(ctx);
            return Stop;
        }
        _ => return Proceed,
    };
    dispatch(ctx, action);
    Stop
}
