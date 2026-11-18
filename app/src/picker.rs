//! Picker window controller (Slint glue).
//!
//! The window is created once in [`Picker::new`] and only shown and hidden. Every emoji is a
//! [`Tile`] that clips one sprite out of a decoded atlas page. The grid is a `ListView` over
//! [`RowsModel`], which builds the tiles of a row only when the `ListView` asks for it, so only
//! the atlas pages of visible rows are decoded.
//!
//! Closing follows the Task 1 spike: drop every tile from the UI, let one frame render (the
//! tile `source` bindings release their image clones only when re-evaluated), then hide and
//! clear the [`AtlasCache`]. Without that frame the decoded pages stay alive while hidden.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use emoji_core::config::Config;
use emoji_core::geometry::{atlas_size_for, place_picker, Point, Rect};
use emoji_core::hotkey::Hotkey;
use emoji_core::picker_state::{Nav, Outcome, PickerState};
use emoji_core::recents::Recents;
use emoji_core::search::SearchIndex;
use emoji_core::table::{cell_rect, Category, EmojiTable, SpriteRef};
use slint::winit_030::winit::event::{MouseScrollDelta, WindowEvent};
use slint::winit_030::winit::platform::windows::{IconExtWindows, WindowAttributesExtWindows};
use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::winit::window::{Icon, WindowAttributes};
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{
    ComponentHandle, Image, Model, ModelNotify, ModelRc, ModelTracker, PhysicalPosition,
    SharedString, Timer, TimerMode, VecModel,
};

use crate::assets::{bundle, sprite_strip, AtlasCache};
use crate::ui::{PickerWindow, Tile};
use crate::win::sys;

/// `Tile.index` offset for the recents row (grid indices are below it).
const RECENT_BASE: i32 = 1_000_000;
/// Tab icon hexcodes in `Category::ALL` order.
const TAB_ICONS: [&str; 8] = [
    "1F600", "1F43B", "1F354", "26BD", "1F697", "1F4A1", "2764", "1F3C1",
];
/// Hand shown as the skin tone swatch.
const TONE_HAND: &str = "270B";
/// Pixel wheel travel that counts as one wheel notch (`WHEEL_DELTA`).
const PIXELS_PER_NOTCH: f64 = 120.0;
/// Safety net when the closing frame never renders.
const CLOSE_TIMEOUT: Duration = Duration::from_millis(250);

/// Owner window for the next created Slint window (see [`window_attributes`]).
static NEXT_OWNER: AtomicIsize = AtomicIsize::new(0);

/// Winit attributes hook, installed on the backend in `main`. The picker window gets a hidden
/// owner window: owned popups have no taskbar button and no Alt+Tab entry, and unlike an
/// extended style this survives winit rewriting the style on every show and hide. Other
/// windows (settings, setup, about) get the app icon from the exe resources.
pub fn window_attributes(attrs: WindowAttributes) -> WindowAttributes {
    match NEXT_OWNER.swap(0, Ordering::SeqCst) {
        0 => {
            // Resource ordinal 1 is the icon embedded by `build.rs`.
            let small = Icon::from_resource(1, None).ok();
            let big = Icon::from_resource(1, None).ok();
            attrs.with_window_icon(small).with_taskbar_icon(big)
        }
        owner => attrs.with_owner_window(owner),
    }
}

pub struct OpenRequest {
    /// Foreground HWND when the hotkey was pressed.
    pub target: isize,
    pub caret: Option<Rect>,
    pub cursor: Point,
    pub at: Instant,
    /// Opened by the Win+. hook: retry a refused `SetForegroundWindow` once.
    pub via_hook: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertRequest {
    pub target: isize,
    /// Glyphs to type: the queue plus the final pick, in the chosen tone.
    pub text: String,
    /// Base hexcodes of the same emoji, for recents.
    pub base_hexcodes: Vec<String>,
}

/// Recents file next to the config file.
fn recents_path(config_path: &std::path::Path) -> PathBuf {
    config_path.with_file_name("recents.txt")
}

/// Loads `recents.txt` into `recents` once, the first time recents are needed while
/// enabled. Recents disabled at startup are never loaded, so without this the first pick
/// after enabling them at runtime would overwrite the file with a near-empty list.
fn load_recents_once(loaded: &Cell<bool>, recents: &RefCell<Recents>, path: &std::path::Path) {
    if loaded.replace(true) {
        return;
    }
    *recents.borrow_mut() = Recents::load(path);
}

/// Pushes picks in pick order, so the last pick ends up first.
fn push_picks(recents: &mut Recents, base_hexcodes: &[String]) {
    for hex in base_hexcodes {
        recents.push(hex);
    }
}

/// Splits the visible emoji list into grid rows of `cols` entries; the last row may be short.
fn build_rows(indices: &[u32], cols: usize) -> Vec<Vec<u32>> {
    indices.chunks(cols.max(1)).map(<[u32]>::to_vec).collect()
}

/// Adds `notches` of Ctrl+wheel travel to `acc`. Returns one zoom step (+1 bigger, -1 smaller)
/// when a whole notch is crossed, so touchpads with many small deltas step like a mouse. At most
/// one step per event; the travel starts again from zero after a step and on a direction change.
fn wheel_step(acc: &Cell<f64>, notches: f64) -> Option<i32> {
    let mut total = acc.get();
    if total * notches < 0.0 {
        total = 0.0;
    }
    total += notches;
    let step = if total >= 1.0 {
        Some(1)
    } else if total <= -1.0 {
        Some(-1)
    } else {
        None
    };
    acc.set(if step.is_some() { 0.0 } else { total });
    step
}

/// A tile for emoji `emoji` in `tone`, clipped from its page at `atlas_size`.
fn emoji_tile(
    table: &EmojiTable,
    cache: &mut AtlasCache,
    emoji: u32,
    tone: u8,
    atlas_size: u16,
    index: i32,
) -> Tile {
    let (_, sprite) = table.emojis[emoji as usize].variant(tone);
    clip_tile(
        cache.image(sprite.page, atlas_size),
        sprite.cell,
        atlas_size,
        index,
    )
}

fn clip_tile(image: Image, cell: u16, atlas_size: u16, index: i32) -> Tile {
    let (x, y, w, h) = cell_rect(cell, atlas_size);
    Tile {
        image,
        x: x as i32,
        y: y as i32,
        w: w as i32,
        h: h as i32,
        index,
    }
}

/// The grid rows. Tiles are built on demand, so the `ListView` only decodes visible pages.
struct RowsModel {
    rows: RefCell<Vec<Vec<u32>>>,
    cols: Cell<usize>,
    tone: Cell<u8>,
    atlas_size: Cell<u16>,
    cache: Rc<RefCell<AtlasCache>>,
    notify: ModelNotify,
}

impl RowsModel {
    fn set(&self, indices: &[u32], cols: usize, tone: u8, atlas_size: u16) {
        *self.rows.borrow_mut() = build_rows(indices, cols);
        self.cols.set(cols.max(1));
        self.tone.set(tone);
        self.atlas_size.set(atlas_size);
        self.notify.reset();
    }
}

impl Model for RowsModel {
    type Data = ModelRc<Tile>;

    fn row_count(&self) -> usize {
        self.rows.borrow().len()
    }

    fn row_data(&self, row: usize) -> Option<ModelRc<Tile>> {
        let rows = self.rows.borrow();
        let items = rows.get(row)?;
        let table = &bundle().table;
        let mut cache = self.cache.borrow_mut();
        let first = row * self.cols.get();
        let tiles: Vec<Tile> = items
            .iter()
            .enumerate()
            .map(|(i, &e)| {
                let index = (first + i) as i32;
                emoji_tile(
                    table,
                    &mut cache,
                    e,
                    self.tone.get(),
                    self.atlas_size.get(),
                    index,
                )
            })
            .collect();
        Some(ModelRc::new(VecModel::from(tiles)))
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Hidden,
    /// Shown, waiting for the first frame (open time is measured there).
    Opening,
    Open,
    /// Tiles dropped, waiting for the frame that releases them before hiding.
    Closing,
}

struct Inner {
    /// Weak handle to this `Inner`, for timers and callbacks.
    me: Weak<Inner>,
    ui: PickerWindow,
    cfg: RefCell<Arc<Config>>,
    recents: RefCell<Recents>,
    /// `recents` holds the contents of `recents.txt` (the caller loads it when recents are
    /// enabled at startup; otherwise it is loaded when they get enabled).
    recents_loaded: Cell<bool>,
    index: SearchIndex,
    /// Emoji indices per tab, in `Category::ALL` order.
    categories: Vec<Vec<u32>>,
    state: RefCell<PickerState>,
    /// Emoji indices of the multi-pick queue, parallel to `state.queue`.
    queued: RefCell<Vec<u32>>,
    /// The visible grid (category or search results).
    current: RefCell<Vec<u32>>,
    /// Emoji indices of the recents row.
    recent: RefCell<Vec<u32>>,
    cache: Rc<RefCell<AtlasCache>>,
    rows: Rc<RowsModel>,
    /// Tab icons and tone hands, keyed by atlas size.
    strip: RefCell<Option<(u16, Image)>>,
    strip_sprites: Vec<SpriteRef>,
    phase: Cell<Phase>,
    opened_at: Cell<Option<Instant>>,
    last_open_ms: Cell<Option<u32>>,
    pending_insert: RefCell<Option<InsertRequest>>,
    target: Cell<isize>,
    anchor: Cell<(Option<Rect>, Point)>,
    scale: Cell<f32>,
    hwnd: Cell<isize>,
    hovered: Cell<i32>,
    ctrl: Cell<bool>,
    /// Ctrl+wheel travel not yet turned into a zoom step, in wheel notches.
    wheel: Cell<f64>,
    /// The picker had keyboard focus at some point during this open; only then does losing
    /// it (a click elsewhere) close the picker.
    had_focus: Cell<bool>,
    config_path: PathBuf,
    close_timer: Timer,
    on_insert: Box<dyn Fn(InsertRequest)>,
}

pub struct Picker {
    inner: Rc<Inner>,
}

impl Picker {
    pub fn new(
        cfg: Arc<Config>,
        recents: Recents,
        on_insert: impl Fn(InsertRequest) + 'static,
    ) -> Picker {
        Picker::with_config_path(
            cfg,
            recents,
            on_insert,
            crate::config_dir().join("config.toml"),
        )
    }

    /// Like [`Picker::new`], saving size and tone changes to `config_path`. Tests pass a temp
    /// path here, so no test can ever write the user's real config.
    fn with_config_path(
        cfg: Arc<Config>,
        recents: Recents,
        on_insert: impl Fn(InsertRequest) + 'static,
        config_path: PathBuf,
    ) -> Picker {
        let table = &bundle().table;
        NEXT_OWNER.store(sys::create_owner_window(), Ordering::SeqCst);
        let ui = PickerWindow::new().expect("creating the picker window failed");
        NEXT_OWNER.store(0, Ordering::SeqCst);

        let categories = Category::ALL
            .iter()
            .map(|&c| table.in_category(c).collect())
            .collect::<Vec<Vec<u32>>>();
        // Tab icons: a typical emoji per tab, else the first one of the category.
        let mut strip_sprites: Vec<SpriteRef> = TAB_ICONS
            .iter()
            .zip(&categories)
            .filter_map(|(hex, list)| table.by_hexcode(hex).or(list.first().copied()))
            .map(|e| table.emojis[e as usize].sprite)
            .collect();
        if let Some(hand) = table.by_hexcode(TONE_HAND) {
            let hand = &table.emojis[hand as usize];
            strip_sprites.extend((0..=5).map(|t| hand.variant(t).1));
        }

        let cache = Rc::new(RefCell::new(AtlasCache::default()));
        let rows = Rc::new(RowsModel {
            rows: RefCell::new(Vec::new()),
            cols: Cell::new(9),
            tone: Cell::new(0),
            atlas_size: Cell::new(32),
            cache: cache.clone(),
            notify: ModelNotify::default(),
        });
        ui.set_rows(ModelRc::from(
            rows.clone() as Rc<dyn Model<Data = ModelRc<Tile>>>
        ));

        let recents_loaded = Cell::new(cfg.recents_enabled);
        let inner = Rc::new_cyclic(|me| Inner {
            me: me.clone(),
            ui,
            cfg: RefCell::new(cfg),
            recents: RefCell::new(recents),
            recents_loaded,
            index: SearchIndex::new(table),
            categories,
            state: RefCell::new(PickerState::default()),
            queued: RefCell::new(Vec::new()),
            current: RefCell::new(Vec::new()),
            recent: RefCell::new(Vec::new()),
            cache,
            rows,
            strip: RefCell::new(None),
            strip_sprites,
            phase: Cell::new(Phase::Hidden),
            opened_at: Cell::new(None),
            last_open_ms: Cell::new(None),
            pending_insert: RefCell::new(None),
            target: Cell::new(0),
            anchor: Cell::new((None, Point { x: 0, y: 0 })),
            scale: Cell::new(1.0),
            hwnd: Cell::new(0),
            hovered: Cell::new(-1),
            ctrl: Cell::new(false),
            wheel: Cell::new(0.0),
            had_focus: Cell::new(false),
            config_path,
            close_timer: Timer::default(),
            on_insert: Box::new(on_insert),
        });
        Inner::wire(&inner);

        // Build the small icon strip off the hotkey path (it decodes most pages once).
        let weak = Rc::downgrade(&inner);
        Timer::single_shot(Duration::from_millis(500), move || {
            if let Some(inner) = weak.upgrade() {
                let cursor = crate::win::foreground::cursor();
                let (_, scale) = sys::monitor_work_area(None, cursor);
                inner.scale.set(scale);
                inner.ensure_strip();
                log!(
                    "picker: icon strip ready (private {} KB)",
                    sys::private_bytes() / 1024
                );
            }
        });

        Picker { inner }
    }

    /// Shows the picker next to the caret (or cursor). A second open while it is up closes it.
    pub fn open(&self, ev: OpenRequest) {
        self.inner.open(ev);
    }

    /// Adopts `cfg` without saving it (see [`Picker::save_config`]).
    pub fn set_config(&self, cfg: Arc<Config>) {
        let inner = &self.inner;
        *inner.cfg.borrow_mut() = cfg;
        if inner.cfg().recents_enabled {
            inner.ensure_recents();
        }
        if matches!(inner.phase.get(), Phase::Opening | Phase::Open) {
            inner.apply_size();
            inner.refresh_all();
            inner.place();
        }
    }

    /// Saves `cfg` to the config file and adopts it. The picker is the only writer of the
    /// config file: settings changes come through here, so they are built on the newest copy
    /// (tone and size changed in the picker included).
    pub fn save_config(&self, cfg: Arc<Config>) {
        let path = &self.inner.config_path;
        if let Err(e) = cfg.save(path) {
            crate::settings::config_save_failed(path, &e);
        }
        self.set_config(cfg);
    }

    /// Forgets the recently used emoji: clears the list and deletes `recents.txt`. Turning
    /// recents on again later starts with an empty list.
    pub fn forget_recents(&self) {
        let inner = &self.inner;
        inner.recents.borrow_mut().items.clear();
        inner.recent.borrow_mut().clear();
        // Nothing left to load: the file is gone.
        inner.recents_loaded.set(true);
        let path = recents_path(&inner.config_path);
        match std::fs::remove_file(&path) {
            Ok(()) => log!("picker: deleted {}", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log!("picker: deleting {} failed: {e}", path.display()),
        }
    }

    /// Changes only the hotkey in the current config and saves it (Win+. hook fallback).
    pub fn save_hotkey(&self, hotkey: Hotkey) {
        self.inner.update_config(|c| c.hotkey = Some(hotkey));
    }

    /// The config as last changed by the picker (size and tone are saved from here).
    pub fn config(&self) -> Arc<Config> {
        self.inner.cfg.borrow().clone()
    }

    /// Milliseconds from the hotkey to the first presented frame of the last open.
    pub fn last_open_ms(&self) -> Option<u32> {
        self.inner.last_open_ms.get()
    }
}

impl Inner {
    fn wire(this: &Rc<Inner>) {
        let ui = &this.ui;
        macro_rules! handler {
            (|$inner:ident $(, $arg:ident)*| $body:expr) => {{
                let weak: Weak<Inner> = Rc::downgrade(this);
                move |$($arg),*| {
                    if let Some($inner) = weak.upgrade() {
                        $body
                    }
                }
            }};
        }
        ui.on_query_changed(handler!(|inner, _q| inner.show_current()));
        ui.on_key(handler!(|inner, name, shift| inner.key(&name, shift)));
        ui.on_tile_clicked(handler!(|inner, index, shift| {
            if let Some(e) = inner.emoji_at(index) {
                inner.pick(e, shift);
            }
        }));
        ui.on_tile_hovered(handler!(|inner, index| {
            inner.hovered.set(index);
            inner.update_name();
        }));
        ui.on_category_clicked(handler!(|inner, c| inner.set_category(c)));
        ui.on_tone_clicked(handler!(|inner| inner.cycle_tone()));
        ui.on_zoom(handler!(|inner, dir| inner.zoom(dir)));

        let weak = Rc::downgrade(this);
        ui.window().on_winit_window_event(move |_, event| {
            let Some(inner) = weak.upgrade() else {
                return EventResult::Propagate;
            };
            inner.winit_event(event)
        });
    }

    fn winit_event(&self, event: &WindowEvent) -> EventResult {
        let weak = &self.me;
        // Work that touches the UI runs on the next loop turn, outside winit's event dispatch.
        let later = |f: fn(&Inner)| {
            let weak = weak.clone();
            Timer::single_shot(Duration::ZERO, move || {
                if let Some(inner) = weak.upgrade() {
                    f(&inner);
                }
            });
        };
        match event {
            // The zero timer runs after this redraw has been drawn and presented.
            WindowEvent::RedrawRequested => match self.phase.get() {
                Phase::Opening => later(Inner::frame_shown),
                Phase::Closing => later(Inner::finish_close),
                _ => {}
            },
            WindowEvent::Focused(true) => {
                if matches!(self.phase.get(), Phase::Opening | Phase::Open) {
                    self.had_focus.set(true);
                }
            }
            WindowEvent::Focused(false) => {
                if matches!(self.phase.get(), Phase::Opening | Phase::Open) {
                    if self.had_focus.get() {
                        later(Inner::deactivated);
                    } else {
                        log!("picker: focus lost before it was ever gained: staying open");
                    }
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                let ctrl = m.state().control_key();
                if !ctrl {
                    self.wheel.set(0.0);
                }
                self.ctrl.set(ctrl);
            }
            WindowEvent::MouseWheel { delta, .. } if self.ctrl.get() => {
                let notches = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    MouseScrollDelta::PixelDelta(p) => p.y / PIXELS_PER_NOTCH,
                };
                if let Some(dir) = wheel_step(&self.wheel, notches) {
                    let weak = weak.clone();
                    Timer::single_shot(Duration::ZERO, move || {
                        if let Some(inner) = weak.upgrade() {
                            inner.ui.invoke_zoom(dir);
                        }
                    });
                }
                return EventResult::PreventDefault;
            }
            _ => {}
        }
        EventResult::Propagate
    }

    fn cfg(&self) -> Arc<Config> {
        self.cfg.borrow().clone()
    }

    fn tile_px(&self) -> u16 {
        self.cfg.borrow().size.tile_px()
    }

    fn cols(&self) -> usize {
        self.ui.get_cols().max(1) as usize
    }

    fn atlas_size(&self) -> u16 {
        atlas_size_for(self.tile_px(), self.scale.get())
    }

    fn open(&self, ev: OpenRequest) {
        match self.phase.get() {
            Phase::Hidden => {}
            Phase::Opening | Phase::Open => {
                self.state.borrow_mut().escape();
                self.queued.borrow_mut().clear();
                self.close(None);
                return;
            }
            Phase::Closing => return,
        }
        self.target.set(ev.target);
        self.anchor.set((ev.caret, ev.cursor));
        let (_, scale) = sys::monitor_work_area(ev.caret, ev.cursor);
        self.scale.set(scale);

        self.ui.set_dark(!sys::apps_use_light_theme());
        self.ui.set_query(SharedString::new());
        *self.state.borrow_mut() = PickerState::default();
        self.queued.borrow_mut().clear();
        self.hovered.set(-1);
        self.wheel.set(0.0);
        self.had_focus.set(false);
        self.apply_size();
        self.refresh_all();
        self.place();

        self.phase.set(Phase::Opening);
        self.opened_at.set(Some(ev.at));
        if let Err(e) = self.ui.show() {
            log!("picker: show failed: {e}");
            self.phase.set(Phase::Hidden);
            return;
        }
        if self.hwnd.get() == 0 {
            self.hwnd.set(self.native_hwnd());
        }
        let hwnd = self.hwnd.get();
        if hwnd != 0 {
            sys::set_tool_window(hwnd);
            if !sys::activate(hwnd) && !(ev.via_hook && self.retry_activate(hwnd)) {
                log!("picker: SetForegroundWindow refused: open without focus, closes on Esc after a click into it, a pick or the hotkey");
            }
        }
        self.ui.invoke_focus_search();
        self.ui.invoke_scroll_to_top();
        self.ui.window().request_redraw();
    }

    /// The Win+. hook ate the key, so the target app, not this process, got the last input
    /// and Windows may refuse the foreground. The service already called
    /// `AllowSetForegroundWindow(ASFW_ANY)`; inject a tap of the unassigned mask key (input
    /// from this process, no effect in any app) and try once more.
    fn retry_activate(&self, hwnd: isize) -> bool {
        crate::win::ll_hook::tap_mask_key();
        let ok = sys::activate(hwnd);
        log!("picker: SetForegroundWindow retry after mask key: {ok}");
        ok
    }

    fn native_hwnd(&self) -> isize {
        self.ui
            .window()
            .with_winit_window(|w| match w.window_handle().ok()?.as_raw() {
                RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
                _ => None,
            })
            .flatten()
            .unwrap_or(0)
    }

    /// First frame after show: record the open time.
    fn frame_shown(&self) {
        if self.phase.get() != Phase::Opening {
            return;
        }
        self.phase.set(Phase::Open);
        if let Some(at) = self.opened_at.take() {
            let ms = at.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
            self.last_open_ms.set(Some(ms));
            log!(
                "picker: open {ms} ms (private {} KB, {} pages)",
                sys::private_bytes() / 1024,
                self.cache.borrow().len()
            );
        }
    }

    fn deactivated(&self) {
        if matches!(self.phase.get(), Phase::Opening | Phase::Open) {
            let outcome = self.state.borrow_mut().deactivated();
            self.queued.borrow_mut().clear();
            debug_assert_eq!(outcome, Outcome::Close);
            self.close(None);
        }
    }

    /// Drops every tile, lets one frame render, then hides (see the module docs).
    fn close(&self, insert: Option<InsertRequest>) {
        if matches!(self.phase.get(), Phase::Hidden | Phase::Closing) {
            return;
        }
        self.phase.set(Phase::Closing);
        *self.pending_insert.borrow_mut() = insert;
        self.rows.set(&[], self.cols(), 0, self.atlas_size());
        self.current.borrow_mut().clear();
        self.recent.borrow_mut().clear();
        self.ui.set_recent(ModelRc::default());
        self.ui.set_queue(ModelRc::default());
        self.ui.set_hovered_name(SharedString::new());
        self.ui.window().request_redraw();
        let weak = self.me.clone();
        self.close_timer
            .start(TimerMode::SingleShot, CLOSE_TIMEOUT, move || {
                if let Some(inner) = weak.upgrade() {
                    log!("picker: closing frame timed out");
                    inner.finish_close();
                }
            });
    }

    fn finish_close(&self) {
        if self.phase.get() != Phase::Closing {
            return;
        }
        self.close_timer.stop();
        if let Err(e) = self.ui.hide() {
            log!("picker: hide failed: {e}");
        }
        self.cache.borrow_mut().clear();
        self.phase.set(Phase::Hidden);
        log!(
            "picker: hidden (private {} KB)",
            sys::private_bytes() / 1024
        );
        let pending = self.pending_insert.borrow_mut().take();
        if let Some(req) = pending {
            let picked = req.base_hexcodes.clone();
            (self.on_insert)(req);
            self.record_picks(&picked);
        }
    }

    /// Adds the inserted emoji to recents and saves them, unless recents are disabled (then
    /// they are not touched at all).
    fn record_picks(&self, base_hexcodes: &[String]) {
        if !self.cfg().recents_enabled {
            return;
        }
        self.ensure_recents();
        let mut recents = self.recents.borrow_mut();
        push_picks(&mut recents, base_hexcodes);
        let path = recents_path(&self.config_path);
        if let Err(e) = recents.save(&path) {
            log!("picker: saving {} failed: {e}", path.display());
        }
    }

    /// Loads `recents.txt` if it was not loaded yet (recents were off at startup).
    fn ensure_recents(&self) {
        load_recents_once(
            &self.recents_loaded,
            &self.recents,
            &recents_path(&self.config_path),
        );
    }

    fn apply_size(&self) {
        let cfg = self.cfg();
        self.ui.set_tile_px(i32::from(cfg.size.tile_px()));
        self.ui.set_recents_on(cfg.recents_enabled);
        self.ui.set_tone(i32::from(cfg.skin_tone));
    }

    /// Moves the window next to the stored anchor, sized for the current tile size.
    fn place(&self) {
        let (caret, cursor) = self.anchor.get();
        let (work, scale) = sys::monitor_work_area(caret, cursor);
        let w = (self.ui.get_win_w() * scale).ceil() as i32;
        let h = (self.ui.get_win_h() * scale).ceil() as i32;
        let at = place_picker(caret, cursor, (w, h), work);
        self.ui
            .window()
            .set_position(PhysicalPosition::new(at.x, at.y));
    }

    /// Rebuilds tabs, recents, queue and grid (after open, tone or size changes).
    fn refresh_all(&self) {
        self.ensure_strip();
        self.refresh_recents();
        self.refresh_queue();
        self.show_current();
    }

    fn ensure_strip(&self) {
        let size = atlas_size_for(
            (f32::from(self.tile_px()) * 0.8).ceil() as u16,
            self.scale.get(),
        );
        let image = {
            let mut strip = self.strip.borrow_mut();
            match &*strip {
                Some((s, image)) if *s == size => image.clone(),
                _ => {
                    let image = sprite_strip(&self.strip_sprites, size);
                    *strip = Some((size, image.clone()));
                    image
                }
            }
        };
        let tabs: Vec<Tile> = (0..Category::ALL.len().min(self.strip_sprites.len()))
            .map(|i| clip_tile(image.clone(), i as u16, size, -1))
            .collect();
        self.ui.set_tabs(ModelRc::new(VecModel::from(tabs)));
        let tone = u16::from(self.cfg.borrow().skin_tone.min(5));
        let hand = Category::ALL.len() as u16 + tone;
        if usize::from(hand) < self.strip_sprites.len() {
            self.ui.set_tone_tile(clip_tile(image, hand, size, -1));
        }
    }

    fn refresh_recents(&self) {
        let cfg = self.cfg();
        let table = &bundle().table;
        let list: Vec<u32> = if cfg.recents_enabled {
            self.recents
                .borrow()
                .items
                .iter()
                .filter_map(|h| table.by_hexcode(h))
                .take(self.cols())
                .collect()
        } else {
            Vec::new()
        };
        let size = self.atlas_size();
        let mut cache = self.cache.borrow_mut();
        let tiles: Vec<Tile> = list
            .iter()
            .enumerate()
            .map(|(i, &e)| {
                emoji_tile(
                    table,
                    &mut cache,
                    e,
                    cfg.skin_tone,
                    size,
                    RECENT_BASE + i as i32,
                )
            })
            .collect();
        drop(cache);
        *self.recent.borrow_mut() = list;
        self.ui.set_recent(ModelRc::new(VecModel::from(tiles)));
    }

    fn refresh_queue(&self) {
        let table = &bundle().table;
        let tone = self.cfg.borrow().skin_tone;
        let size = self.atlas_size();
        let mut cache = self.cache.borrow_mut();
        let tiles: Vec<Tile> = self
            .queued
            .borrow()
            .iter()
            .map(|&e| emoji_tile(table, &mut cache, e, tone, size, -1))
            .collect();
        drop(cache);
        self.ui.set_queue(ModelRc::new(VecModel::from(tiles)));
    }

    /// Fills the grid from the search query, or from the selected category when it is blank.
    fn show_current(&self) {
        let table = &bundle().table;
        let query = self.ui.get_query();
        let cfg = self.cfg();
        let empty = Recents::default();
        let recents = self.recents.borrow();
        let boost = if cfg.recents_enabled {
            &*recents
        } else {
            &empty
        };
        let started = Instant::now();
        let list = match self.index.search(&query, boost, table) {
            Some(hits) => hits,
            None => {
                let c = self.ui.get_category().clamp(0, 7) as usize;
                self.categories[c].clone()
            }
        };
        drop(recents);
        if !query.is_empty() {
            log!(
                "picker: search {query:?}: {} hits in {:?}",
                list.len(),
                started.elapsed()
            );
        }
        let cols = self.cols();
        self.rows.set(&list, cols, cfg.skin_tone, self.atlas_size());
        self.state.borrow_mut().reset(cols, list.len());
        *self.current.borrow_mut() = list;
        self.ui.set_selected(0);
        self.ui.invoke_scroll_to_top();
        self.update_name();
    }

    fn emoji_at(&self, index: i32) -> Option<u32> {
        if index >= RECENT_BASE {
            self.recent
                .borrow()
                .get((index - RECENT_BASE) as usize)
                .copied()
        } else if index >= 0 {
            self.current.borrow().get(index as usize).copied()
        } else {
            None
        }
    }

    fn update_name(&self) {
        let hovered = self.hovered.get();
        let emoji = if hovered >= 0 {
            self.emoji_at(hovered)
        } else {
            let sel = self.state.borrow().selected as i32;
            self.emoji_at(sel)
        };
        let name = emoji
            .map(|e| bundle().table.emojis[e as usize].name.as_str())
            .unwrap_or("");
        self.ui.set_hovered_name(name.into());
    }

    fn key(&self, name: &str, shift: bool) {
        if !matches!(self.phase.get(), Phase::Opening | Phase::Open) {
            return;
        }
        let nav = match name {
            "left" => Some(Nav::Left),
            "right" => Some(Nav::Right),
            "up" => Some(Nav::Up),
            "down" => Some(Nav::Down),
            _ => None,
        };
        if let Some(nav) = nav {
            let selected = {
                let mut state = self.state.borrow_mut();
                state.nav(nav);
                state.selected
            };
            self.hovered.set(-1);
            self.ui.set_selected(selected as i32);
            self.ui.invoke_show_row((selected / self.cols()) as i32);
            self.update_name();
            return;
        }
        match name {
            "enter" => {
                let sel = self.state.borrow().selected;
                let emoji = self.current.borrow().get(sel).copied();
                if let Some(e) = emoji {
                    self.pick(e, shift);
                }
            }
            "tab" => {
                let n = Category::ALL.len() as i32;
                let c = self.ui.get_category();
                self.set_category((c + if shift { n - 1 } else { 1 }) % n);
            }
            "escape" => {
                let outcome = self.state.borrow_mut().escape();
                self.queued.borrow_mut().clear();
                debug_assert_eq!(outcome, Outcome::Close);
                self.close(None);
            }
            _ => {}
        }
    }

    fn pick(&self, emoji: u32, shift: bool) {
        if !matches!(self.phase.get(), Phase::Opening | Phase::Open) {
            return;
        }
        let table = &bundle().table;
        let e = &table.emojis[emoji as usize];
        let (glyph, _) = e.variant(self.cfg.borrow().skin_tone);
        let outcome = self.state.borrow_mut().pick(glyph, shift);
        match outcome {
            Outcome::Stay => {
                self.queued.borrow_mut().push(emoji);
                self.refresh_queue();
            }
            Outcome::Insert(text) => {
                let mut base_hexcodes: Vec<String> = self
                    .queued
                    .borrow_mut()
                    .drain(..)
                    .map(|q| table.emojis[q as usize].hexcode.clone())
                    .collect();
                base_hexcodes.push(e.hexcode.clone());
                let req = InsertRequest {
                    target: self.target.get(),
                    text,
                    base_hexcodes,
                };
                self.close(Some(req));
            }
            Outcome::Close => self.close(None),
        }
    }

    fn set_category(&self, c: i32) {
        self.ui
            .set_category(c.clamp(0, Category::ALL.len() as i32 - 1));
        self.ui.set_query(SharedString::new());
        self.show_current();
    }

    /// Saves a changed config and keeps it as the current one.
    fn update_config(&self, change: impl FnOnce(&mut Config)) {
        let mut cfg = (*self.cfg()).clone();
        change(&mut cfg);
        let path = &self.config_path;
        if let Err(e) = cfg.save(path) {
            crate::settings::config_save_failed(path, &e);
        }
        *self.cfg.borrow_mut() = Arc::new(cfg);
    }

    fn cycle_tone(&self) {
        if !matches!(self.phase.get(), Phase::Opening | Phase::Open) {
            return;
        }
        self.update_config(|c| c.skin_tone = (c.skin_tone + 1) % 6);
        self.apply_size();
        let selected = self.state.borrow().selected;
        self.ensure_strip();
        self.refresh_recents();
        self.refresh_queue();
        let tone = self.cfg.borrow().skin_tone;
        let list = self.current.borrow().clone();
        self.rows.set(&list, self.cols(), tone, self.atlas_size());
        self.ui.set_selected(selected as i32);
    }

    fn zoom(&self, dir: i32) {
        if !matches!(self.phase.get(), Phase::Opening | Phase::Open) {
            return;
        }
        let old = self.cfg.borrow().size;
        let new = if dir > 0 { old.next() } else { old.prev() };
        if new == old {
            return;
        }
        self.update_config(|c| c.size = new);
        log!("picker: size {old:?} -> {new:?}");
        self.apply_size();
        // Pages of the old atlas size are no longer used once the grid is rebuilt.
        self.cache.borrow_mut().clear();
        let query_selected = self.state.borrow().selected;
        self.ensure_strip();
        self.refresh_recents();
        self.refresh_queue();
        let tone = self.cfg.borrow().skin_tone;
        let list = self.current.borrow().clone();
        self.rows.set(&list, self.cols(), tone, self.atlas_size());
        self.ui.set_selected(query_selected as i32);
        self.ui
            .invoke_show_row((query_selected / self.cols()) as i32);
        self.place();
    }
}

/// Diagnostics driver (`EMOJI_PICKER_SELFTEST=<cycles>`, debug and `diag` builds only).
#[cfg(any(debug_assertions, feature = "diag"))]
impl Picker {
    /// Opens and closes the picker `cycles` times through the normal code paths, without
    /// synthetic input, logging open time and private bytes, then quits the event loop.
    /// Every step waits 1.5 s. Cycle 2 types `fire` letter by letter, cycle 3 zooms to Huge
    /// and back, cycle 4 steps through all six tones. Size and tone end where they started.
    pub fn selftest(&self, cycles: u32) {
        const STEPS: u32 = 4;
        let weak = Rc::downgrade(&self.inner);
        let tick = Cell::new(0u32);
        let start_size = Cell::new(self.inner.cfg().size);
        let open_ms: RefCell<Vec<u32>> = RefCell::new(Vec::new());
        let hidden_kb: RefCell<Vec<usize>> = RefCell::new(Vec::new());
        let open_kb: RefCell<Vec<usize>> = RefCell::new(Vec::new());
        let kb = || sys::private_bytes() / 1024;
        let timer: &'static Timer = Box::leak(Box::new(Timer::default()));
        log!("selftest: {cycles} cycles, idle private {} KB", kb());
        timer.start(TimerMode::Repeated, Duration::from_millis(1500), move || {
            let Some(inner) = weak.upgrade() else { return };
            let t = tick.get();
            tick.set(t + 1);
            let cycle = t / STEPS + 1;
            if cycle > cycles {
                timer.stop();
                let mut ms = open_ms.borrow().clone();
                let cold = ms.first().copied();
                let warm: Vec<u32> = ms.drain(1..).collect::<Vec<_>>();
                let mut sorted = warm.clone();
                sorted.sort_unstable();
                let median = sorted.get(sorted.len() / 2).copied();
                log!(
                    "selftest: done. cold {cold:?} ms, warm {warm:?} ms (median {median:?}), open KB {:?}, hidden KB {:?}",
                    open_kb.borrow(),
                    hidden_kb.borrow()
                );
                let _ = slint::quit_event_loop();
                return;
            }
            match t % STEPS {
                0 => inner.open(OpenRequest {
                    target: 0,
                    caret: None,
                    cursor: crate::win::foreground::cursor(),
                    at: Instant::now(),
                    via_hook: false,
                }),
                1 => {
                    if let Some(ms) = inner.last_open_ms.take() {
                        open_ms.borrow_mut().push(ms);
                    }
                    log!("selftest: cycle {cycle} open, private {} KB, phase {:?}", kb(), inner.phase.get());
                    open_kb.borrow_mut().push(kb());
                    match cycle {
                        2 => {
                            for q in ["f", "fi", "fir", "fire"] {
                                inner.ui.set_query(q.into());
                                inner.show_current();
                            }
                        }
                        3 => {
                            start_size.set(inner.cfg().size);
                            for _ in 0..3 {
                                inner.ui.invoke_zoom(1);
                            }
                        }
                        4 => inner.ui.invoke_tone_clicked(),
                        _ => {}
                    }
                }
                2 => {
                    log!(
                        "selftest: cycle {cycle} after action, private {} KB, {} pages, phase {:?}",
                        kb(),
                        inner.cache.borrow().len(),
                        inner.phase.get()
                    );
                    open_kb.borrow_mut().push(kb());
                    match cycle {
                        3 => {
                            // Zoom up stops at Huge, so step down until the start size again.
                            while inner.cfg().size != start_size.get() {
                                let before = inner.cfg().size;
                                inner.ui.invoke_zoom(-1);
                                if inner.cfg().size == before {
                                    break;
                                }
                            }
                        }
                        4 => {
                            for _ in 0..5 {
                                inner.ui.invoke_tone_clicked();
                            }
                        }
                        _ => {}
                    }
                    inner.key("escape", false);
                }
                _ => {
                    log!("selftest: cycle {cycle} hidden, private {} KB", kb());
                    hidden_kb.borrow_mut().push(kb());
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_pushed_last_first() {
        let mut recents = Recents {
            items: vec!["1F600".into(), "1F44D".into()],
        };
        let picks: Vec<String> = ["1F44D", "2764", "1F525"].map(String::from).to_vec();
        push_picks(&mut recents, &picks);
        assert_eq!(recents.items, ["1F525", "2764", "1F44D", "1F600"]);
        assert_eq!(
            recents_path(std::path::Path::new("dir/config.toml")),
            PathBuf::from("dir/recents.txt")
        );
    }

    #[test]
    fn recents_loaded_lazily_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recents.txt");
        std::fs::write(
            &path,
            "1F525
2764
",
        )
        .unwrap();
        let loaded = Cell::new(false);
        let recents = RefCell::new(Recents::default());
        load_recents_once(&loaded, &recents, &path);
        assert!(loaded.get());
        assert_eq!(recents.borrow().items, ["1F525", "2764"]);
        // A pick after the load is kept: a second call does not reload the file.
        recents.borrow_mut().push("1F600");
        load_recents_once(&loaded, &recents, &path);
        assert_eq!(recents.borrow().items, ["1F600", "1F525", "2764"]);
        // Already loaded at startup: the file is not read again.
        let loaded = Cell::new(true);
        let recents = RefCell::new(Recents::default());
        load_recents_once(&loaded, &recents, &path);
        assert!(recents.borrow().items.is_empty());
    }

    #[test]
    fn rows_split_by_cols() {
        assert_eq!(
            build_rows(&[1, 2, 3, 4, 5], 2),
            vec![vec![1, 2], vec![3, 4], vec![5]]
        );
    }

    /// Renders the real picker through the controller into PNGs with Slint's software renderer
    /// on a `MinimalSoftwareWindow`: no window is created on screen and no input is sent.
    /// `SNAPSHOT_DIR` selects the output directory (default: the temp dir).
    #[test]
    #[ignore = "writes PNG snapshots; run with --ignored"]
    fn render_snapshots() {
        use slint::platform::software_renderer::{
            MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
        };
        use slint::platform::{Platform, WindowAdapter};

        struct Headless(Rc<MinimalSoftwareWindow>);
        impl Platform for Headless {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }

        let config_dir = tempfile::tempdir().unwrap();
        let config_path = config_dir.path().join("config.toml");
        let out = std::env::var_os("SNAPSHOT_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();

        let recents = Recents {
            items: ["1F525", "1F44D", "2764", "1F602"]
                .map(String::from)
                .to_vec(),
        };
        let picker = Picker::with_config_path(
            Arc::new(Config::default()),
            recents,
            |_| {},
            config_path.clone(),
        );
        let inner = picker.inner.clone();
        let render = |name: &str| {
            let w = inner.ui.get_win_w().ceil() as u32;
            let h = inner.ui.get_win_h().ceil() as u32;
            window.set_size(slint::PhysicalSize::new(w, h));
            let mut buf = vec![PremultipliedRgbaColor::default(); (w * h) as usize];
            for _ in 0..3 {
                slint::platform::update_timers_and_animations();
                window.request_redraw();
                window.draw_if_needed(|r| {
                    r.render(&mut buf, w as usize);
                });
            }
            let bytes: Vec<u8> = buf
                .iter()
                .flat_map(|p| [p.red, p.green, p.blue, p.alpha])
                .collect();
            let path = out.join(name);
            let file = std::fs::File::create(&path).unwrap();
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header()
                .unwrap()
                .write_image_data(&bytes)
                .unwrap();
            eprintln!("wrote {} ({w}x{h})", path.display());
        };

        picker.open(OpenRequest {
            target: 0,
            caret: None,
            cursor: Point { x: 100, y: 100 },
            at: Instant::now(),
            via_hook: false,
        });
        inner.ui.set_dark(false);
        render("picker-medium-light.png");
        inner.key("down", false);
        inner.key("right", false);
        inner.ui.set_dark(true);
        render("picker-medium-dark.png");
        inner.ui.set_query("fire".into());
        inner.show_current();
        render("picker-search-fire.png");
        inner.set_category(0);
        inner.ui.invoke_tone_clicked();
        inner.ui.invoke_tone_clicked();
        inner.ui.invoke_tone_clicked();
        inner.pick(inner.current.borrow()[0], true);
        inner.pick(inner.current.borrow()[1], true);
        inner.set_category(7);
        render("picker-flags-tone3-queue.png");
        inner.ui.invoke_zoom(-1);
        render("picker-small.png");
        inner.ui.invoke_zoom(1);
        inner.ui.invoke_zoom(1);
        inner.ui.invoke_zoom(1);
        render("picker-huge.png");
        assert_eq!(inner.cfg().size, emoji_core::config::SizeStep::Huge);
        assert_eq!(inner.cfg().skin_tone, 3);
        let saved = Config::load(&config_path).0;
        assert_eq!(
            (saved.size, saved.skin_tone),
            (emoji_core::config::SizeStep::Huge, 3)
        );
        inner.key("escape", false);
        assert_eq!(inner.phase.get(), Phase::Closing);
        assert!(inner.queued.borrow().is_empty());
    }

    #[test]
    fn icon_hexcodes_exist() {
        let table = &bundle().table;
        for hex in TAB_ICONS.iter().chain([&TONE_HAND]) {
            assert!(table.by_hexcode(hex).is_some(), "{hex}");
        }
    }

    #[test]
    fn wheel_steps_once_per_notch() {
        let acc = Cell::new(0.0);
        // Mouse: one notch is one step.
        assert_eq!(wheel_step(&acc, 1.0), Some(1));
        assert_eq!(wheel_step(&acc, -1.0), Some(-1));
        // Touchpad: many small deltas give one step per whole notch.
        let steps: Vec<i32> = (0..25).filter_map(|_| wheel_step(&acc, 0.1)).collect();
        assert_eq!(steps, vec![1, 1]);
        // Reversing drops the travel collected the other way.
        assert_eq!(wheel_step(&acc, -0.6), None);
        assert_eq!(wheel_step(&acc, -0.6), Some(-1));
        // A fast flick of 3 notches gives one step and leaves nothing pending.
        assert_eq!(wheel_step(&acc, 3.0), Some(1));
        assert_eq!(wheel_step(&acc, 0.5), None);
    }

    #[test]
    fn rows_edge_cases() {
        assert!(build_rows(&[], 9).is_empty());
        assert_eq!(build_rows(&[1, 2], 0), vec![vec![1], vec![2]]);
        assert_eq!(build_rows(&[1, 2, 3], 3), vec![vec![1, 2, 3]]);
    }
}
