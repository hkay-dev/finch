//! The Settings window (finch#wg9e): a pane list on the system sidebar material at the left, the chosen pane's grouped
//! form on the right, as a SwiftUI Settings window is (Harrier's `settings/window.rs` is the model). One window: Cmd-, fronts the
//! open one. The pane last shown is remembered while the app runs. A background launch (`GPUI_EDITOR_BACKGROUND`) opens it
//! behind everything, neither key nor active, so it can be captured (`debug settings <pane>`).
//!
//! The window is one view that draws again when a setting changes (the Store's observers, a file edited by hand included) and
//! while a control's spring moves; at rest it asks for no frame. The controls are `settings_controls`, the panes'
//! rows `settings_panes`. Every write goes through `settings_controls::write`, so the observers that apply a setting everywhere do
//! their work as they do for the View menu.

use std::{collections::HashMap, sync::atomic::{AtomicUsize, Ordering}};

use gpui_kit::component::{ActiveTheme as _, input::InputState};
use gpui_kit::*;
use objc2::{msg_send, rc::Retained, runtime::AnyObject};

use crate::{
    control,
    hover::FadeHover as _,
    motion::{self, Fade},
    scrollbar::{self, Overlay, Scroller},
    settings,
    settings_controls::{self as controls, Look},
    theme, tokens, window_look,
};

/// The gap between the search field and the first pane of the list.
const SEARCH_GAP: f32 = 8.;

/// A pane of the list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pane {
    General,
    Editor,
    Writing,
    CodeColors,
    Text,
}

impl Pane {
    /// The panes in list order.
    pub const ALL: [Pane; 5] = [Pane::General, Pane::Editor, Pane::Writing, Pane::CodeColors, Pane::Text];

    pub fn title(self) -> &'static str {
        match self {
            Pane::General => "General",
            Pane::Editor => "Editor",
            Pane::Writing => "Writing",
            Pane::CodeColors => "Code colors",
            Pane::Text => "Text",
        }
    }

    /// The pane's SF Symbol, always a filled glyph, as System Settings sets every tile's.
    fn symbol(self) -> &'static str {
        match self {
            Pane::General => "gearshape.fill",
            Pane::Editor => "doc.text.fill",
            Pane::Writing => "text.book.closed.fill",
            Pane::CodeColors => "paintpalette.fill",
            Pane::Text => "textformat.size",
        }
    }

    fn tile(self) -> u32 {
        match self {
            Pane::General => tokens::SETTINGS_TILE_GRAY,
            Pane::Editor => tokens::SETTINGS_TILE_BLUE,
            Pane::Writing => tokens::SETTINGS_TILE_ORANGE,
            Pane::CodeColors => tokens::SETTINGS_TILE_INDIGO,
            Pane::Text => tokens::SETTINGS_TILE_PINK,
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|pane| *pane == self).unwrap_or(0)
    }

    /// The pane a name picks, in any case, with a dash or a space between words (`code-colors`).
    pub fn named(name: &str) -> Option<Pane> {
        let name = name.trim().replace('-', " ");
        Self::ALL.into_iter().find(|pane| pane.title().eq_ignore_ascii_case(&name))
    }
}

/// The pane shown last, as an index into `Pane::ALL`: kept while the app runs, so the window opens where it was left.
static LAST_PANE: AtomicUsize = AtomicUsize::new(0);

/// Where the window was last (display-local points, as `Window::bounds`) and on which display, kept while the app runs so
/// it reopens there. The width is fixed, so only the origin and the height are kept.
static LAST_FRAME: std::sync::Mutex<Option<(Point<Pixels>, Pixels, DisplayId)>> = std::sync::Mutex::new(None);

/// The frame to open at: the last one while its display is still connected (kept inside that display's visible area), else
/// centered by the caller.
fn remembered_frame(size: Size<Pixels>, cx: &App) -> Option<(Bounds<Pixels>, DisplayId)> {
    let (origin, height, id) = (*LAST_FRAME.lock().ok()?)?;
    let display = cx.displays().into_iter().find(|display| display.id() == id)?;
    let visible = display.visible_bounds();
    let size = Size { width: size.width, height: height.min(visible.size.height) };
    let max = visible.bottom_right() - point(size.width, size.height);
    let origin = point(origin.x.max(visible.origin.x).min(max.x), origin.y.max(visible.origin.y).min(max.y));
    Some((Bounds { origin, size }, id))
}

/// The open Settings window. The view is weak: the window's root holds it, so closing the window releases it at once
/// (`on_release` runs) instead of at the next open.
struct Open {
    handle: AnyWindowHandle,
    view: WeakEntity<SettingsWindow>,
}

impl Global for Open {}

pub struct SettingsWindow {
    pub(crate) selected: usize,
    window: WindowId,
    /// The pane the page shows: the chosen one, or while a search leaves it out, the first pane with a match. It is the one the
    /// crossfade has reached.
    shown: usize,
    /// The panes the list shows, as indexes into `Pane::ALL`: all of them, or those with a matching row while a search is typed.
    visible: Vec<usize>,
    /// How much of the chosen pane shows: at rest on 1, and a switch fades it up from 0 over `FADE_QUICK`.
    arriving: Fade,
    /// The panes a switch left, each fading from the level it was drawn at to 0 under the chosen one; gone when they land.
    leaving: Vec<(usize, Fade)>,
    /// Holds the keys of the pane list (Up and Down move the choice) and the window's own commands (Cmd-W).
    pub(crate) focus: FocusHandle,
    /// Each pane's scroll position and scrollbar.
    scrolls: Vec<(ScrollHandle, Scroller)>,
    /// The pane the window's title names, so a render sets the title only when the pane changes.
    titled: Option<usize>,
    /// The search field at the top of the list.
    pub(crate) search: Entity<InputState>,
    /// The scratch shortcut's text field.
    pub(crate) shortcut: Entity<InputState>,
    /// The hex field of each color override, by appearance (true: light) and role, with the subscription that writes it.
    pub(crate) hex: HashMap<(bool, String), (Entity<InputState>, Subscription)>,
    /// The Code colors preview's run colors, easing to the new scheme's when one is picked.
    pub(crate) glide: crate::settings_panes::Glide,
    _watch: Vec<Subscription>,
}

/// Opens the Settings window on `pane` (the last one shown when `None`), or fronts the open one. A background launch
/// opens it behind everything and never activates.
pub fn open(pane: Option<Pane>, background: bool, cx: &mut App) {
    let live = cx.try_global::<Open>().and_then(|open| Some((open.handle, open.view.upgrade()?))).filter(|(handle, _)| !window_look::is_closing(handle.window_id()));
    if let Some((handle, view)) = live {
        let shown = handle.update(cx, |_, window, cx| {
            if !background {
                window.activate_window();
            }
            view.update(cx, |this, cx| {
                if let Some(pane) = pane {
                    this.select(pane, cx);
                }
            });
        });
        if shown.is_ok() {
            return;
        }
    }
    let selected = pane.map_or_else(|| LAST_PANE.load(Ordering::Relaxed).min(Pane::ALL.len() - 1), Pane::index);
    crate::symbols::prewarm_settings();
    let (width, height) = (tokens::SETTINGS_WINDOW_WIDTH, tokens::SETTINGS_WINDOW_HEIGHT);
    let (bounds, display_id) = match remembered_frame(size(px(width), px(height)), cx) {
        Some((bounds, id)) => (bounds, Some(id)),
        None => (Bounds::centered(None, size(px(width), px(height)), cx), None),
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        display_id,
        titlebar: Some(TitlebarOptions {
            title: Some("Settings".into()),
            appears_transparent: true,
            // None: the unified toolbar (`window_look::unified_title_bar`) centers the lights in the top band.
            traffic_light_position: None,
        }),
        // The width is fixed: the least size is the whole width, and the most is set on the NSWindow (`tune_window`).
        window_min_size: Some(size(px(width), px(tokens::SETTINGS_WINDOW_MIN_HEIGHT))),
        focus: !background,
        // The sidebar's material shows through where nothing draws.
        window_background: WindowBackgroundAppearance::Transparent,
        // Unshown: GPUI would order the window front before its first frame (`window_look::fade_in`, `free_frame_pacing`).
        show: false,
        // No frame cap when the window isn't key (skill gpui, rule 10).
        inactive_frame_interval: None,
        ..Default::default()
    };
    let opened = gpui_kit::open_window(options, cx, |window, cx| {
        window_look::free_frame_pacing(window, background);
        tune_window(window);
        if !background {
            window_look::fade_in(window, cx);
        }
        theme::watch(window);
        // The red light fades the window out as Cmd-W does, rather than removing it at once.
        window.on_window_should_close(cx, |window, cx| {
            window_look::fade_out(window, cx);
            false
        });
        cx.new(|cx| SettingsWindow::new(selected, window, cx))
    });
    match opened {
        Ok((handle, view)) => cx.set_global(Open { handle, view: view.downgrade() }),
        Err(error) => eprintln!("finch: the Settings window didn't open: {error}"),
    }
}

/// `debug settings [pane] [height]`: opens the window on a pane (the last one shown when none is named), without activating in a
/// background launch, and sizes it to `height` points tall when one is given (the width is fixed). Replies with the pane's title.
pub fn show(args: &str, background: bool, cx: &mut App) -> Result<String, String> {
    let (name, height) = match args.trim().rsplit_once(char::is_whitespace) {
        Some((name, last)) if last.parse::<f32>().is_ok() => (name, last.parse::<f32>().ok()),
        _ if args.trim().parse::<f32>().is_ok() => ("", args.trim().parse::<f32>().ok()),
        _ => (args.trim(), None),
    };
    let pane = match name.trim() {
        "" => None,
        name => Some(Pane::named(name).ok_or_else(|| format!("no pane {name}; the panes are {}", Pane::ALL.map(|pane| pane.title().to_lowercase().replace(' ', "-")).join(", ")))?),
    };
    open(pane, background, cx);
    let (handle, shown) = cx.try_global::<Open>().and_then(|open| Some((open.handle, open.view.upgrade()?.read(cx).selected))).ok_or("the Settings window didn't open")?;
    if let Some(height) = height.filter(|height| *height >= tokens::SETTINGS_WINDOW_MIN_HEIGHT) {
        handle.update(cx, |_, window, _| window.resize(size(px(tokens::SETTINGS_WINDOW_WIDTH), px(height)))).map_err(|err| err.to_string())?;
    }
    Ok(Pane::ALL[shown].title().to_owned())
}

/// `settings-search <text>`: types `text` into the open Settings window's search field (nothing clears it), as typing does;
/// `settings-search --escape` presses Escape in it. Replies with the panes the list showed at the last draw, comma-joined.
pub fn search(args: &str, cx: &mut App) -> Result<String, String> {
    let (handle, view) = cx.try_global::<Open>().and_then(|open| Some((open.handle, open.view.upgrade()?))).ok_or("the Settings window isn't open")?;
    let args = args.trim();
    handle
        .update(cx, |_, window, cx| {
            view.update(cx, |this, cx| {
                if args == "--escape" {
                    this.escape_search(window, cx);
                } else {
                    let text = args.to_owned();
                    this.search.update(cx, |input, cx| input.set_value(text, window, cx));
                    cx.notify();
                }
                this.visible.iter().map(|at| Pane::ALL[*at].title()).collect::<Vec<_>>().join(", ")
            })
        })
        .map_err(|err| err.to_string())
}

/// The open Settings window's handle, `None` when it is closed or closing.
pub(crate) fn handle(cx: &App) -> Option<AnyWindowHandle> {
    let handle = cx.try_global::<Open>()?.handle;
    (cx.windows().contains(&handle) && !window_look::is_closing(handle.window_id())).then_some(handle)
}

/// Whether every window left is the Settings window (or none is): the editor windows are all closed.
pub fn only_settings_left(cx: &App) -> bool {
    let open = cx.try_global::<Open>().map(|open| open.handle);
    cx.windows().into_iter().all(|handle| Some(handle) == open)
}

/// Closes the open Settings window, if any.
pub fn close(cx: &mut App) {
    let Some(handle) = cx.try_global::<Open>().map(|open| open.handle) else { return };
    if cx.windows().contains(&handle) {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
}

impl SettingsWindow {
    fn new(selected: usize, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let id = window.window_handle().window_id();
        let number = window_look::number(window);
        let shortcut = cx.new(|cx| InputState::new(window, cx).placeholder("ctrl-alt-space"));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        crate::control::name_field("settings search", search.focus_handle(cx));
        let watch = vec![
            // A setting changed anywhere (a menu item, the file edited by hand) shows here.
            cx.observe_global::<settings::Store>(|_, cx| cx.notify()),
            cx.observe_global::<crate::explorer::icons::AccentTick>(|_, cx| cx.notify()),
            // A move or a resize is the frame the next open uses.
            cx.observe_window_bounds(window, |_, window, cx| {
                if let (WindowBounds::Windowed(bounds), Some(display)) = (window.window_bounds(), window.display(cx)) {
                    if let Ok(mut last) = LAST_FRAME.lock() {
                        *last = Some((bounds.origin, bounds.size.height, display.id()));
                    }
                }
            }),
            cx.observe_window_activation(window, |_, window, cx| {
                if window.is_window_active() && std::env::var_os("GPUI_EDITOR_BACKGROUND").is_some() {
                    window_look::deactivate_app();
                }
                cx.notify();
            }),
            cx.subscribe_in(&shortcut, window, |_, input, event: &gpui_kit::component::input::InputEvent, _, cx| {
                if matches!(event, gpui_kit::component::input::InputEvent::Focus | gpui_kit::component::input::InputEvent::Blur) {
                    cx.notify();
                }
                if matches!(event, gpui_kit::component::input::InputEvent::PressEnter { .. } | gpui_kit::component::input::InputEvent::Blur) {
                    // Only a finished entry saves: a prefix like cmd-s on the way to cmd-shift-j would register system-wide.
                    let text = input.read(cx).value().trim().to_owned();
                    if !text.is_empty() && crate::hotkey::parse(&text).is_ok() && settings::get(cx).scratch.shortcut != text {
                        controls::write(cx, move |settings| settings.scratch.shortcut = text);
                    }
                }
            }),
            // Typing in the search narrows the panes and rows on the next draw.
            cx.subscribe_in(&search, window, |_, _, event: &gpui_kit::component::input::InputEvent, _, cx| {
                if matches!(event, gpui_kit::component::input::InputEvent::Change | gpui_kit::component::input::InputEvent::Focus | gpui_kit::component::input::InputEvent::Blur) {
                    cx.notify();
                }
            }),
        ];
        let me = cx.entity_id();
        cx.on_release(move |_, cx| {
            motion::forget(id);
            crate::hover::forget(id);
            crate::float_field::forget(id);
            controls::forget(id);
            if let Some(number) = number {
                window_look::forget(number);
            }
            if cx.try_global::<Open>().is_some_and(|open| open.view.entity_id() == me) {
                cx.remove_global::<Open>();
            }
        })
        .detach();
        Self {
            selected,
            shown: selected,
            visible: (0..Pane::ALL.len()).collect(),
            window: id,
            arriving: Fade::with(1., tokens::FADE_QUICK),
            leaving: Vec::new(),
            focus: cx.focus_handle(),
            scrolls: Pane::ALL.iter().map(|_| (ScrollHandle::new(), Scroller::default())).collect(),
            titled: None,
            search,
            shortcut,
            hex: HashMap::new(),
            glide: Default::default(),
            _watch: watch,
        }
    }

    /// The pane chosen in the list. While a search leaves it out, the page shows the first pane with a match and this stays the
    /// pane that comes back when the search is cleared.
    fn select(&mut self, pane: Pane, cx: &mut Context<Self>) {
        self.selected = pane.index();
        LAST_PANE.store(self.selected, Ordering::Relaxed);
        cx.notify();
    }

    /// The page changes to pane `next`: a crossfade. The pane left fades out from where it is drawn, the one chosen fades in from
    /// where it is drawn (a pane still leaving that is chosen again turns around), so a switch interrupted turns around and never jumps.
    fn fade_to(&mut self, next: usize) {
        let now = motion::drawn_now(self.window);
        let mut out = Fade::with(self.arriving.level(now), tokens::FADE_QUICK);
        out.aim(0., now);
        self.leaving.push((self.shown, out));
        let mut coming = Fade::with(0., tokens::FADE_QUICK);
        if let Some(at) = self.leaving.iter().position(|(left, _)| *left == next) {
            coming = Fade::with(self.leaving.remove(at).1.level(now), tokens::FADE_QUICK);
        }
        coming.aim(1., now);
        self.arriving = coming;
        self.shown = next;
    }

    /// Cmd-F: the keys go to the search field, its text selected.
    fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.search.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        self.search.update(cx, |input, cx| input.select_all(window, cx));
    }

    /// The clear button's work, and the empty state's Clear search: empties the field and keeps the keys in it.
    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| input.set_value("", window, cx));
        self.focus_search(window, cx);
        cx.notify();
    }

    /// Escape: empties the field, every pane and row is back, and the pane list takes the keys.
    fn escape_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| input.set_value("", window, cx));
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The search field at the top of the list: the folder tree's capsule with a magnifier, "Search" for a placeholder and a
    /// clear button while it holds text.
    fn search_field(&self, look: Look, cx: &mut Context<Self>) -> Div {
        let has_text = !self.search.read(cx).value().is_empty();
        let ink = crate::hover::Ink { rest: theme::ink(tokens::ALPHA_SECONDARY, cx), lit: look.text, wash: look.text };
        let clear = div()
            .absolute()
            .top_0()
            .bottom_0()
            .right(tokens::FILTER_PAD_X - (tokens::TAB_CLOSE_HIT - tokens::TAB_CLOSE_SLOT) / 2.)
            .flex()
            .items_center()
            .child(
                crate::hover::icon_button("settings-search-clear", "xmark.circle.fill", tokens::TAB_CLOSE_HIT, tokens::FILTER_SYMBOL, ink)
                    .occlude()
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.clear_search(window, cx)))
                    .tooltip(crate::tooltip::tip("Clear search"))
                    .tooltip_show_delay(crate::tooltip::DELAY),
            );
        // The slot for the clear button stays, so the text never shifts when it appears.
        crate::float_field::float_field(&self.search, Some("magnifyingglass"), 0., cx)
            .w_full()
            .flex_none()
            .pr(tokens::TAB_CLOSE_SLOT)
            .children(has_text.then_some(clear))
            // The field answers Escape itself, ahead of the window.
            .capture_action(cx.listener(|this, _: &gpui_kit::component::input::Escape, window, cx| {
                this.escape_search(window, cx);
                cx.stop_propagation();
            }))
    }

    /// The pane list: the search field, then a row per pane the search leaves, the chosen one under a pill that glides to a new
    /// choice, in the accent while the window is key and in grey otherwise.
    fn sidebar(&mut self, window: &mut Window, look: Look, cx: &mut Context<Self>) -> Div {
        let active = control::window_key(window);
        let place = self.visible.iter().position(|at| *at == self.shown);
        let (level, sliding) = controls::spring(window, controls::key_of("sidebar-pill"), place.unwrap_or(0) as f32);
        let (lit, fading) = controls::spring(window, controls::key_of("sidebar-active"), if active { 1. } else { 0. });
        if sliding || fading {
            window.request_animation_frame();
        }
        let wash = hsla(0., 0., if look.light { 0. } else { 1. }, 1.);
        let idle = Hsla { a: tokens::SETTINGS_SIDEBAR_IDLE_ALPHA, ..wash };
        let hover = Hsla { a: tokens::SETTINGS_SIDEBAR_HOVER_ALPHA, ..wash };
        let on_fill = hsla(0., 0., 1., 1.);
        let row = tokens::SETTINGS_SIDEBAR_ROW;
        let inset = f32::from(tokens::ROW_WASH_INSET_Y);
        let pill = div()
            .absolute()
            .left_0()
            .right_0()
            .top(px(level * row + inset)) // design: the glide's level, in rows
            .h(px(row - 2. * inset))
            .rounded(tokens::RADIUS_ROW)
            .bg(crate::tab_bar::mix(idle, look.accent, lit));
        let panes = self.visible.clone();
        let shown = self.shown;
        let mut list = div()
            .relative()
            .w(px(tokens::SETTINGS_SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .pt(px(tokens::SETTINGS_SIDEBAR_TOP))
            .px(px(tokens::SETTINGS_SIDEBAR_INSET))
            .child(self.search_field(look, cx))
            .child(div().relative().flex().flex_col().mt(px(SEARCH_GAP)).children(place.map(|_| pill)).children(panes.into_iter().enumerate().map(|(position, at)| {
                let pane = Pane::ALL[at];
                // How much of the pill is under this row, so its name turns white as the pill arrives and back as it leaves.
                let under = (1. - (level - position as f32).abs()).clamp(0., 1.);
                let tile: Hsla = rgb(pane.tile()).into();
                let sheen = crate::tab_bar::mix(tile, on_fill, tokens::SETTINGS_TILE_SHEEN);
                let tile = linear_gradient(180., linear_color_stop(sheen, 0.), linear_color_stop(tile, 1.));
                let entry = div()
                    .id(pane.title())
                    .relative()
                    .h(px(row))
                    .px(px(tokens::SETTINGS_SIDEBAR_ROW_PAD))
                    .flex()
                    .items_center()
                    .gap(px(tokens::SETTINGS_SIDEBAR_ROW_PAD))
                    .rounded(tokens::RADIUS_ROW)
                    .text_color(crate::tab_bar::mix(look.text, on_fill, under * lit))
                    .child(
                        div()
                            .size(px(tokens::SETTINGS_TILE))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(tokens::SETTINGS_TILE_RADIUS))
                            .border_1()
                            .border_color(Hsla { a: under * tokens::SETTINGS_TILE_RING_ALPHA, ..on_fill })
                            .bg(tile)
                            .text_color(on_fill)
                            .child(crate::symbols::icon_weighted(pane.symbol(), tokens::SETTINGS_TILE_SYMBOL, crate::symbols::SEMIBOLD)),
                    )
                    .child(pane.title())
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.select(pane, cx)));
                // The row under the pointer fades a quiet wash in; the chosen one lets it fall away under its pill.
                entry.fade_hover(("settings-row", at), None, hover).covered(at == shown).into_any_element()
            })));
        // The sidebar's own edge: a hairline down its right side.
        let tint = if look.light { hsla(0., 0., 0., tokens::SETTINGS_SIDEBAR_FILL_ALPHA_LIGHT) } else { hsla(0., 0., 1., tokens::SETTINGS_SIDEBAR_FILL_ALPHA) };
        list = list.bg(tint);
        list = list.child(div().absolute().top_0().bottom_0().right_0().w(px(tokens::SETTINGS_RULE_HEIGHT)).child(theme::hairline(look.rule, true).size_full()));
        list
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = window.window_handle().window_id();
        // One reading of the frame clock per frame, for every spring and fade below.
        let now = motion::frame_now(id);
        let look = Look::of(window, cx);
        controls::set_look(look);
        window_look::sidebar_material(window, tokens::SETTINGS_SIDEBAR_WIDTH, theme::pin(cx));
        let query = self.search.read(cx).value().to_string();
        controls::set_query(&query);
        // While a search is typed every pane is built to see whether it keeps a row; the chosen pane's sections are reused below.
        let mut built: Vec<(usize, Vec<AnyElement>)> = Vec::new();
        self.visible = if controls::filtering() {
            let mut found = Vec::new();
            for (at, each) in Pane::ALL.into_iter().enumerate() {
                let sections = self.sections(each, window, cx);
                if controls::kept() > 0 {
                    found.push(at);
                    built.push((at, sections));
                }
            }
            found
        } else {
            (0..Pane::ALL.len()).collect()
        };
        // The chosen pane while the search leaves it, else the first it leaves; the choice itself stays for when the search is cleared.
        let selected = self.selected.min(Pane::ALL.len() - 1);
        let shown = if self.visible.contains(&selected) { selected } else { self.visible.first().copied().unwrap_or(selected) };
        if shown != self.shown {
            self.fade_to(shown);
        }
        let pane = Pane::ALL[shown];
        let title_at = if self.visible.is_empty() { usize::MAX } else { shown };
        if self.titled != Some(title_at) {
            self.titled = Some(title_at);
            window.set_window_title(if self.visible.is_empty() { "Settings" } else { pane.title() });
        }
        self.follow_file(window, cx);
        // Cmd-W reaches the window's own action listener only along the focus path, so the root keeps the keys unless a field holds them.
        if control::window_key(window) && !self.focus.contains_focused(window, cx) {
            window.focus(&self.focus, cx);
        }
        let sidebar = self.sidebar(window, look, cx);
        self.leaving.retain(|(_, fade)| fade.is_moving(now));
        if !self.leaving.is_empty() || self.arriving.is_moving(now) {
            window.request_animation_frame();
        }
        let sections = match built.into_iter().find(|(at, _)| *at == shown) {
            Some((_, sections)) => sections,
            None if self.visible.is_empty() => Vec::new(),
            None => self.sections(pane, window, cx),
        };
        let (scroll, scroller) = self.scrolls[shown].clone();
        let page = |at: usize, scroll: &ScrollHandle, sections: Vec<AnyElement>| {
            div()
                .id(("settings-page", at))
                .size_full()
                .overflow_y_scroll()
                .track_scroll(scroll)
                .flex()
                .flex_col()
                .gap(px(tokens::SETTINGS_SECTION_GAP))
                .px(px(tokens::SETTINGS_FORM_INSET))
                .pt(px(tokens::SETTINGS_FORM_TOP))
                .pb(px(tokens::SETTINGS_FORM_INSET))
                .children(sections)
        };
        let title = |text: &'static str, level: f32| {
            let title = div()
                .absolute()
                .inset_0()
                .px(px(tokens::SETTINGS_FORM_INSET))
                .flex()
                .items_center()
                .text_size(px(tokens::SETTINGS_TITLE_SIZE))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(look.text)
                .child(text);
            if level < 1. { title.opacity(level) } else { title }
        };
        let arriving = self.arriving.level(now);
        let leaving: Vec<(usize, f32)> = self.leaving.iter().map(|(at, fade)| (*at, fade.level(now))).collect();
        // The panes a switch left sit under the chosen one, in layers that take no mouse: the chosen pane's layer blocks it.
        let mut scrolling = div().relative().flex_1().min_h_0();
        let mut titles = div().relative().h(tokens::TOP_BAND).flex_none().child(title(pane.title(), arriving));
        for (at, level) in leaving {
            let sections = self.sections(Pane::ALL[at], window, cx);
            let layer = div().absolute().inset_0().child(page(at, &self.scrolls[at].0, sections));
            scrolling = scrolling.child(if level < 1. { layer.opacity(level) } else { layer });
            titles = titles.child(title(Pane::ALL[at].title(), level));
        }
        let here = div()
            .relative()
            .size_full()
            .occlude()
            .child(scrollbar::watch(&scroller))
            .child(page(shown, &scroll, sections))
            .child(scrollbar::overlay(Overlay { handle: std::rc::Rc::new(scroll), scroller, ground: Some(look.ground) }));
        let scrolling = scrolling.child(if arriving < 1. { here.opacity(arriving) } else { here });
        // No pane has a match: the empty state takes the page, under no title.
        let (titles, scrolling) = if self.visible.is_empty() {
            let query = self.search.read(cx).value().trim().to_owned();
            let clear = crate::empty_state::Action {
                id: "settings-clear-search",
                label: "Clear search",
                key: Some("\u{238b}"),
                run: std::rc::Rc::new(cx.listener(|this, _: &ClickEvent, window, cx| this.clear_search(window, cx))),
            };
            let empty = crate::empty_state::view("settings-no-results", "magnifyingglass", "No Results", format!("for \u{201c}{query}\u{201d}"), vec![clear], cx);
            (div().h(tokens::TOP_BAND).flex_none(), div().relative().flex_1().min_h_0().child(empty))
        } else {
            (titles, scrolling)
        };
        let detail = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(look.ground)
            // The pane's title, level with the traffic lights.
            .child(titles)
            .child(scrolling);
        let theme = cx.theme();
        div()
            .size_full()
            .flex()
            .track_focus(&self.focus)
            .font_family(theme.font_family.clone())
            .text_size(tokens::TEXT_BODY)
            .line_height(tokens::TEXT_BODY_LINE)
            .text_color(look.text)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let escape = event.keystroke.key == "escape" && !event.keystroke.modifiers.modified();
                // Esc with the list holding the keys and a search typed clears the search.
                if escape && this.focus.is_focused(window) && !this.search.read(cx).value().is_empty() {
                    this.escape_search(window, cx);
                    cx.stop_propagation();
                    return;
                }
                // Esc in a field puts the saved text back and hands the keys to the list (a field keeps what it didn't use).
                if escape && !this.focus.is_focused(window) {
                    this.revert_fields(window, cx);
                    cx.stop_propagation();
                    return;
                }
                // The list takes the arrows only while no field holds the keys; it moves among the panes the search leaves.
                if !this.focus.is_focused(window) {
                    return;
                }
                let Some(at) = this.visible.iter().position(|pane| *pane == this.shown) else { return };
                let last = this.visible.len() - 1;
                let next = match event.keystroke.key.as_str() {
                    "down" => (at + 1).min(last),
                    "up" => at.saturating_sub(1),
                    "home" => 0,
                    "end" => last,
                    _ => return,
                };
                if this.visible[next] != this.shown {
                    this.select(Pane::ALL[this.visible[next]], cx);
                }
            }))
            // Cmd-W and Shift-Cmd-W close Settings, and must not close a tab or a window of the editor behind it.
            .on_action(cx.listener(|_, _: &crate::app::CloseTab, window, cx| window_look::fade_out(window, cx)))
            .on_action(cx.listener(|_, _: &crate::app::CloseWindow, window, cx| window_look::fade_out(window, cx)))
            // Cmd-F (Edit > Find) puts the keys in the search field.
            .on_action(cx.listener(|this, _: &gpui_kit::component::input::Search, window, cx| this.focus_search(window, cx)))
            .child(sidebar)
            .child(detail)
    }
}

/// What AppKit gets that GPUI has no option for: the width is fixed (the content's most is its least), and the window has no
/// minimize and no zoom, as System Settings has none (both lights show disabled).
fn tune_window(window: &Window) {
    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else { return };
    let raw_window_handle::RawWindowHandle::AppKit(handle) = handle.as_raw() else { return };
    let view: *mut AnyObject = handle.ns_view.as_ptr().cast();
    // SAFETY: GPUI's view and its window, on the main thread; `standardWindowButton:` and `setEnabled:` are plain AppKit
    // calls and the button may be nil.
    unsafe {
        let native: Option<Retained<AnyObject>> = msg_send![view, window];
        let Some(native) = native else { return };
        let most = window_look::Size { width: f64::from(tokens::SETTINGS_WINDOW_WIDTH), height: 100_000. };
        let _: () = msg_send![&*native, setContentMaxSize: most];
        for button in [1_isize, 2] {
            let light: Option<Retained<AnyObject>> = msg_send![&*native, standardWindowButton: button];
            if let Some(light) = light {
                let _: () = msg_send![&*light, setEnabled: false];
            }
        }
    }
}
