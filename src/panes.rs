//! The editor and the preview as views of their own, so each redraws only when it has
//! something to say: the workspace embeds them (and the explorer) as cached siblings, and a
//! preview parse rebuilds the preview pane alone. The caret is a
//! view of its own too (`caret_layer.rs`), drawn over the editor pane, so a blink rebuilds
//! that view and not the editor pane (gpui-editor#grwg).
//!
//! The workspace creates both in its `new` and tells them what to show through `set`; neither
//! reads the workspace. The editor pane is a cached sibling of the tab bar's `Pane` in the
//! workspace's middle panel and is drawn only while the pane has a tab, so a tab hover never
//! rebuilds the editor. It also owns the crossfade between
//! two tabs' editors, asking its frames itself and only while one fades. It leaves the editor's top
//! and place in an `EditorLink` each time it builds, which the preview pane's follow and the
//! window's input handler read, since a view that reads the editor itself is rebuilt by every
//! edit and scroll.

use std::{cell::{Cell, RefCell}, path::PathBuf, rc::Rc, time::{Duration, Instant}};

use gpui_kit::component::{
    ActiveTheme as _, WindowExt as _,
    input::{self as kit, Editor, EditorState},
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use gpui_kit::base::ScrollbarHandle as _;

use crate::{
    explorer::RevealCurrentFile,
    empty_state,
    native_menu::{self, COMMAND, Entry, Item, SHIFT},
    caret_layer::CaretLayer,
    status_line::StatusLine,
    document::Document,
    motion::{ease_in, ease_out, Glide},
    preview::{Driver, EditorLink, WashLayer, render::{forget_embedded_owner, release_embedded_images, release_unseen_embedded_images, text_view}},
    settings::{self, SavedPanes, SplitAxis, Typeface},
    tokens::{COLUMN_SLIDE, FADE_QUICK, PRELAYOUT_SETTLE_FRAMES, PRELAYOUT_SLICE, STATUS_HEIGHT, TOP_BAND},
};

/// What a hidden editor's wrap depends on besides its text: the pane's drawn width and the text size.
type Fit = (Pixels, Pixels);

/// An editor that was showing and is fading out over the one that came forward. The fade clock
/// starts on the frame after the new editor's first draw: that draw shapes the text (60 to 90 ms
/// for a big file), and a clock started at the switch would be mostly run out by then.
struct Leaving {
    editor: Entity<EditorState>,
    /// How much of it showed when it began to fade.
    from: f32,
    /// When the fade began: `None` until the new editor has been drawn once.
    since: Option<Instant>,
    /// A render has drawn the new editor since the switch.
    drawn: bool,
    /// The style it was drawn in when it went (`StyleDip::shown`), kept while it fades.
    style: Typeface,
}

impl Leaving {
    fn alpha(&self, now: Instant) -> f32 {
        let Some(since) = self.since else { return self.from };
        let u = now.saturating_duration_since(since).as_secs_f32() / FADE_QUICK;
        if u >= 1. { 0. } else { self.from * (1. - ease_out(u)) }
    }

    /// Called by each render: the first one draws the new editor, the next starts the clock.
    fn start(&mut self, now: Instant) {
        if self.since.is_none() {
            if self.drawn {
                self.since = Some(now);
            } else {
                self.drawn = true;
            }
        }
    }
}

/// The time a fade is sampled at when a switch interrupts it: the frame clock the fade is drawn
/// by, so the level it turns around from is the level on screen; the real time before the pane
/// has drawn once.
fn sampled(window: Option<WindowId>) -> Instant {
    window.map_or_else(Instant::now, crate::motion::drawn_now)
}

/// The `rects` row that says pane `pane`'s editor drew in the frame its `editor:<pane>` rect is from.
fn fresh_id(pane: EntityId) -> String {
    format!("editor-fresh:{}", pane.as_u64())
}

/// Takes pane `pane`'s `editor-fresh` row from `window`'s rects (no editor to draw, or the pane closed), so the rect it left behind reads as stale. Nothing with rects off.
fn unmark(window: WindowId, pane: EntityId) {
    if crate::control::rects_enabled() {
        crate::control::unrecord(window, &fresh_id(pane));
    }
}

impl Drop for EditorPane {
    fn drop(&mut self) {
        if let Some(window) = self.window {
            unmark(window, self.me);
        }
    }
}

/// The editor, drawn under the window's top band.
pub struct EditorPane {
    /// The showing editor; `None` once the pane has no tab, so a closed tab's document is freed.
    editor: Option<Entity<EditorState>>,
    /// The preview panel is open: the preview follows, and only the active pane wakes it.
    shows: bool,
    /// This is the window's active pane: only it wakes the preview pane when its top moves.
    active: bool,
    /// How tall the band over the editor is: the top inset.
    band: Pixels,
    /// How much of the status line's room and the line itself show: 1 normally, easing to 0 in Minimal mode.
    status_level: f32,
    link: Rc<EditorLink>,
    /// Tells the link where the caret is (`watch_caret`).
    _caret: Option<Subscription>,
    /// The file the showing editor holds, for a pasted image to land beside (`paste_image.rs`); `None` for an Untitled page.
    note: crate::paste_image::NotePath,
    /// The preview pane to wake when the editor's top moves.
    preview: EntityId,
    /// `set` has been called since the pane last had no tab.
    shown: bool,
    /// Editors fading out over the showing one, oldest first.
    leaving: Vec<Leaving>,
    /// Editors this pane has shown, which a switch can come back to.
    known: Vec<WeakEntity<EditorState>>,
    /// Whether each known editor is prose (Markdown or plain text), so a hidden tab of the other kind isn't wrapped in the showing tab's font.
    kinds: Vec<(EntityId, bool)>,
    /// The showing editor's drawn width and text size, from the probe's prepaint.
    width: Rc<Cell<Option<Fit>>>,
    /// The width and text size the hidden editors were last laid out for.
    laid: Option<Fit>,
    /// A new width and size and how many frames they have held.
    settle: Option<(Fit, u32)>,
    /// Hidden editors already laid out, and the width and size they were laid out for: a width
    /// and size that come back skip them.
    fitted: Vec<(EntityId, Fit)>,
    /// The scrollbar marks of each editor this pane has drawn, so a tab's last scan survives the
    /// frames it isn't drawn; pruned with `known`.
    marks: Vec<(EntityId, crate::scrollbar::Marks)>,
    /// Hidden editors still to lay out for `laid`, the next one last.
    queue: Vec<WeakEntity<EditorState>>,
    /// The caret, drawn over the editor by a view of its own (`caret_layer.rs`).
    layer: Entity<CaretLayer>,
    /// The strip at the pane's foot (`status_line.rs`): a view of its own, so a caret move rebuilds it and not the editor pane.
    status: Entity<StatusLine>,
    /// How much of the caret shows while old editors fade out over the new one; shared with the layer.
    cover: Rc<Cell<f32>>,
    /// The editor the caret layer draws the scrollbar of; the renderers of the others paint their own knob.
    layer_shows: Rc<Cell<Option<EntityId>>>,
    /// The window this pane last drew in: where a switch reads the frame clock.
    window: Option<WindowId>,
    /// The last tab went and no editor has shown since: the next one fades in.
    empty: bool,
    /// The first editor after `empty`, fading in over the empty state.
    entering: Option<Entering>,
    /// The showing editor is Markdown or plain text, so it draws in the prose style.
    prose: bool,
    /// The text style the showing editor draws in, and the dip that changes it.
    style: StyleDip,
    /// This view's id, for `unmark` when it drops.
    me: EntityId,
    /// Minimal mode is on (`set_minimal`).
    minimal: bool,
}

/// Says an old editor or the first editor of an empty pane has landed: the workspace redraws.
pub struct Landed;

impl EventEmitter<Landed> for EditorPane {}

/// An editor fading in over the empty state. Its clock starts like `Leaving`'s, on the frame
/// after its first draw, which shapes the text.
#[derive(Default)]
struct Entering {
    since: Option<Instant>,
    drawn: bool,
}

impl Entering {
    /// How much of the editor shows.
    fn alpha(&self, now: Instant) -> f32 {
        let Some(since) = self.since else { return 0. };
        let u = now.saturating_duration_since(since).as_secs_f32() / FADE_QUICK;
        if u >= 1. { 1. } else { ease_out(u) }
    }

    fn start(&mut self, now: Instant) {
        if self.since.is_none() {
            if self.drawn {
                self.since = Some(now);
            } else {
                self.drawn = true;
            }
        }
    }
}

/// How long the text takes to fade out and back in when its style changes: the old style dips to
/// nothing, the new one is set while nothing shows (the one re-wrap hides in the dip), and rises.
const DIP_DOWN: f32 = 0.09;
const DIP_UP: f32 = 0.16;

/// The text style the showing editor draws in, and the dip that changes it. A tab of the other
/// kind (prose or code) coming forward sets its style at once: the tab crossfade already covers it.
struct StyleDip {
    /// Whether the editor drawn last was prose.
    prose: bool,
    /// The style being drawn.
    shown: Typeface,
    /// The style the dip is heading for.
    to: Typeface,
    phase: DipPhase,
}

#[derive(Clone, Copy)]
enum DipPhase {
    Rest,
    /// Fading out from `from`.
    Down { since: Instant, from: f32 },
    /// Fading in with the new style: `since` starts on the frame after the one that drew it first,
    /// which shapes the text.
    Rise { since: Option<Instant>, drawn: bool },
}

impl StyleDip {
    fn at(style: Typeface) -> Self {
        Self { prose: false, shown: style, to: style, phase: DipPhase::Rest }
    }

    /// How much of the text shows at `now`.
    fn alpha(&self, now: Instant) -> f32 {
        let since = |since: Instant, length: f32| (now.saturating_duration_since(since).as_secs_f32() / length).min(1.);
        match self.phase {
            DipPhase::Rest => 1.,
            DipPhase::Down { since: at, from } => from * (1. - ease_in(since(at, DIP_DOWN))),
            DipPhase::Rise { since: None, .. } => 0.,
            DipPhase::Rise { since: Some(at), .. } => ease_out(since(at, DIP_UP)),
        }
    }

    /// Frames are wanted until the text has risen.
    fn moving(&self) -> bool {
        !matches!(self.phase, DipPhase::Rest)
    }

    /// Called by each render with the style the settings want for this kind of file; returns how
    /// much of the text shows. The style to draw with is `shown`, read after this.
    fn step(&mut self, wanted: Typeface, prose: bool, now: Instant) -> f32 {
        if self.prose != prose {
            *self = Self { prose, ..Self::at(wanted) };
            return 1.;
        }
        if wanted != self.to {
            self.phase = DipPhase::Down { since: now, from: self.alpha(now) };
            self.to = wanted;
        }
        if let DipPhase::Down { since, .. } = self.phase {
            if now.saturating_duration_since(since).as_secs_f32() >= DIP_DOWN {
                self.shown = self.to;
                self.phase = DipPhase::Rise { since: None, drawn: false };
            }
        }
        if let DipPhase::Rise { since, drawn } = &mut self.phase {
            match since {
                None if *drawn => *since = Some(now),
                None => *drawn = true,
                Some(at) => {
                    if now.saturating_duration_since(*at).as_secs_f32() >= DIP_UP {
                        self.phase = DipPhase::Rest;
                    }
                }
            }
        }
        self.alpha(now)
    }
}

impl EditorPane {
    /// How many entries the editor-side bookkeeping holds, for the control socket's `tab-state`:
    /// `known`, `fitted`, `marks` and `queue`; and the width the showing editor's wrap map last
    /// wrapped at, in points (`None` with wrap off or no tab).
    pub fn state_sizes(&self, cx: &App) -> ([usize; 4], Option<f32>) {
        let wrap = self.editor.as_ref().and_then(|editor| editor.read(cx).display_map().wrap_width()).map(f32::from);
        ([self.known.len(), self.fitted.len(), self.marks.len(), self.queue.len()], wrap)
    }

    pub fn new(editor: Entity<EditorState>, link: Rc<EditorLink>, preview: EntityId, cx: &mut Context<Self>) -> Self {
        let cover = Rc::new(Cell::new(1.));
        let layer = cx.new(|cx| CaretLayer::new(editor.clone(), cover.clone(), cx));
        let status = cx.new(|_| StatusLine::new());
        let caret = Self::watch_caret(&editor, &link, cx);
        Self {
            _caret: Some(caret),
            editor: Some(editor),
            shows: false,
            link,
            note: Rc::default(),
            preview,
            active: false,
            band: TOP_BAND,
            status_level: 1.,
            shown: false,
            window: None,
            leaving: Vec::new(),
            known: Vec::new(),
            kinds: Vec::new(),
            width: Rc::new(Cell::new(None)),
            laid: None,
            settle: None,
            fitted: Vec::new(),
            marks: Vec::new(),
            queue: Vec::new(),
            layer,
            status,
            cover,
            layer_shows: Rc::new(Cell::new(None)),
            empty: false,
            entering: None,
            prose: false,
            style: StyleDip::at(settings::get(cx).editor.code_style),
            me: cx.entity_id(),
            minimal: false,
        }
    }

    /// The caret's view: the workspace draws it over this pane, as its own cached sibling, so a
    /// blink rebuilds it and not the editor.
    pub fn layer(&self) -> Entity<CaretLayer> {
        self.layer.clone()
    }

    /// Tells the link where the caret is each time the editor notifies, and wakes the preview's
    /// wash view (never the preview pane) when it moved. It returns without a notify of its own:
    /// a view that read the editor would be rebuilt by every edit, scroll and blink.
    fn watch_caret(editor: &Entity<EditorState>, link: &EditorLink, cx: &mut Context<Self>) -> Subscription {
        // The caret where the pane first sees it: the first move is measured from here.
        link.caret_moved(editor, editor.read(cx).cursor(), editor.read(cx).text().len());
        cx.observe(editor, |pane, editor, cx| {
            let moved = pane.link.caret_moved(&editor, editor.read(cx).cursor(), editor.read(cx).text().len());
            if moved && pane.shows && pane.active {
                if let Some(layer) = pane.link.layer.get() {
                    App::notify(cx, layer);
                }
            }
        })
    }

    /// The pane is closing: its caret layer stops drawing, so the editor paints its own line
    /// number again.
    pub fn release_caret(&mut self, cx: &mut Context<Self>) {
        self.layer.update(cx, |layer, cx| layer.release(cx));
        cx.notify();
    }

    /// Says how tall the band over this pane is, `TOP_BAND` or `PANE_BAND`: the editor's top inset.
    pub fn set_band(&mut self, band: Pixels, cx: &mut Context<Self>) {
        if self.band != band {
            self.band = band;
            cx.notify();
        }
    }

    /// Says how much of the status line shows, 1 to 0 (Minimal mode): its room under the editor and its opacity ease together, and at 0 it isn't drawn.
    pub fn set_status(&mut self, status: f32, cx: &mut Context<Self>) {
        if self.status_level != status {
            self.status_level = status;
            cx.notify();
        }
    }

    /// Tells the pasting what file the showing editor holds (`None` for an Untitled page).
    pub fn set_note(&mut self, path: Option<PathBuf>) {
        *self.note.borrow_mut() = path;
    }

    /// Minimal mode (finch#m6nn) on or off for this pane: the gutter's line numbers hidden without
    /// touching `[editor] line_numbers`, and the reading column forced on (70 characters for prose,
    /// 100 for code, in any width), for the editor shown now, the ones fading out or drawn before,
    /// and any shown later while on. Both change before the next prepaint, so the editor wraps once.
    /// The same value does nothing. The scrollbar already shows only while the hand scrolls.
    pub fn set_minimal(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.minimal == on {
            return;
        }
        self.minimal = on;
        let editors: Vec<_> = self
            .editor
            .iter()
            .cloned()
            .chain(self.leaving.iter().map(|leaving| leaving.editor.clone()))
            .chain(self.known.iter().filter_map(|known| known.upgrade()))
            .collect();
        for editor in editors {
            crate::document::set_minimal(&editor, on, cx);
        }
        cx.notify();
    }

    /// Shows `editor`, saying whether the preview panel `shows` and whether the tab is an `untitled` page (which shows the "Start typing…" hint while empty). Another editor than the one showing crossfades: the old one stays on top and
    /// fades out over `FADE_QUICK`, from the share it still shows if it was already fading.
    pub fn set(&mut self, editor: Entity<EditorState>, shows: bool, active: bool, untitled: bool, prose: bool, cx: &mut Context<Self>) {
        let editor_id = editor.entity_id();
        // An Untitled page hints; a file, even an empty one, never does. The hint fades with the text on the editor's own frames.
        editor.update(cx, |state, cx| state.set_hint(if untitled { "Start typing\u{2026}" } else { "" }, crate::tokens::HINT_ALPHA, FADE_QUICK, ease_out, cx));
        let swapped = self.editor.as_ref() != Some(&editor);
        // An editor this pane shows for the first time (or the same document come from another window) takes the pane's minimal mode.
        if swapped || !self.shown {
            crate::document::set_minimal(&editor, self.minimal, cx);
        }
        if swapped || self.shows != shows || self.active != active || !self.shown {
            // A tab that has drawn and is laid out for the pane's width draws no slower than the
            // one leaving, so its first frame is already the frame to start the fade on.
            let warm = self.laid.is_some()
                && self.laid == self.width.get()
                && self.settle.is_none()
                && self.known.iter().any(|known| known.entity_id() == editor.entity_id())
                && !self.queue.iter().any(|queued| queued.entity_id() == editor.entity_id())
                && editor.read(cx).drawn_width().is_some();
            // A shown tab's layout follows its draw, so what it was fitted to no longer says anything.
            self.fitted.retain(|(id, _)| *id != editor.entity_id());
            let now = sampled(self.window);
            self.drop_faded(now, Some(editor.entity_id()), cx);
            if let Some(old) = self.editor.take().filter(|old| self.shown && *old != editor) {
                let from = self.leaving.last().map_or(1., |leaving| 1. - leaving.alpha(now));
                let from = self.entering.as_ref().map_or(from, |entering| entering.alpha(now));
                self.leaving.push(Leaving { editor: old, from: from * self.style.alpha(now), since: None, drawn: warm, style: self.style.shown });
                if self.leaving.len() > 3 {
                    let oldest = self.leaving.remove(0);
                    self.let_go(oldest, Some(editor.entity_id()), cx);
                }
            }
            // The first editor after the pane was empty fades in over the empty state, also while the
            // last one is still fading out.
            if std::mem::take(&mut self.empty) {
                self.entering = Some(Entering::default());
            }
            self.shown = true;
            self.known.retain(|known| known.upgrade().is_some());
            self.kinds.retain(|(id, _)| self.known.iter().any(|known| known.entity_id() == *id));
            self.marks.retain(|(id, _)| self.known.iter().any(|known| known.entity_id() == *id));
            if !self.known.iter().any(|known| known.entity_id() == editor.entity_id()) {
                self.known.push(editor.downgrade());
            }
            if swapped {
                self._caret = Some(Self::watch_caret(&editor, &self.link, cx));
            }
            self.layer.update(cx, |layer, cx| layer.set(editor.clone(), cx));
            self.status.update(cx, |status, cx| status.set(&editor, cx));
            self.editor = Some(editor);
            self.shows = shows;
            self.active = active;
            cx.notify();
        }
        match self.kinds.iter_mut().find(|(id, _)| *id == editor_id) {
            Some(kind) => kind.1 = prose,
            None => self.kinds.push((editor_id, prose)),
        }
        if std::mem::replace(&mut self.prose, prose) != prose {
            cx.notify();
        }
    }

    /// The pane has no tab and this view isn't drawn: it lets go of the editor it showed, and the
    /// next editor shows in place, not crossfaded from the one that went.
    pub fn blank(&mut self) {
        self.shown = false;
        self.editor = None;
        self.leaving.clear();
        self.entering = None;
    }

    /// The pane's last tab closed: its editor stays drawn and fades out over the empty state the
    /// workspace draws under it (`FADE_QUICK`, from the share it showed), then is dropped, so a
    /// closed tab's text is still freed once the fade lands. The next editor fades in from nothing.
    pub fn fade_out(&mut self, cx: &mut Context<Self>) {
        let now = sampled(self.window);
        if let Some(old) = self.editor.take().filter(|_| self.shown) {
            self.drop_faded(now, Some(old.entity_id()), cx);
            // Under any older fade it was fully drawn, or as far as it had faded in.
            let from = self.entering.take().map_or(1., |entering| entering.alpha(now)) * self.style.alpha(now);
            // The new editor draws nothing to wait for, so the clock starts at the first frame.
            self.leaving.push(Leaving { editor: old, from, since: None, drawn: true, style: self.style.shown });
            cx.notify();
        }
        self.shown = false;
        self.empty = true;
        if self.leaving.is_empty() {
            self.let_go_marks(cx);
        }
    }

    /// The pane has no editor to show and none fading: the scan results and the bar the caret layer holds for the old tab go, so an empty pane keeps no ticks.
    fn let_go_marks(&mut self, cx: &mut Context<Self>) {
        self.marks.clear();
        self.layer.update(cx, |layer, cx| layer.clear_bar(cx));
    }

    /// An editor is still fading out over the empty state: the workspace keeps drawing this pane.
    pub fn fading(&self) -> bool {
        !self.leaving.is_empty()
    }

    /// Drops the editors whose fade is done. `showing` is the editor drawn in their place.
    fn drop_faded(&mut self, now: Instant, showing: Option<EntityId>, cx: &mut App) {
        let (kept, faded): (Vec<_>, Vec<_>) = std::mem::take(&mut self.leaving).into_iter().partition(|leaving| leaving.alpha(now) > 0.01);
        self.leaving = kept;
        let any = !faded.is_empty();
        for leaving in faded {
            self.let_go(leaving, showing, cx);
        }
        if !any || !self.leaving.is_empty() {
            return;
        }
        // The faded tabs are gone for good when their documents closed: no mark, fit or queue entry outlives its editor.
        self.known.retain(|known| known.upgrade().is_some());
        self.queue.retain(|queued| queued.upgrade().is_some());
        self.marks.retain(|(id, _)| self.known.iter().any(|known| known.entity_id() == *id));
        self.fitted.retain(|(id, _)| self.known.iter().any(|known| known.entity_id() == *id));
    }

    /// A tab that faded out has painted on every frame of the fade, so its shaped lines were
    /// refilled and would stay until the next switch (`pane.rs` lets go of the hidden ones
    /// only then): drop them with the fade, and the matches of its open Find with them (they
    /// were drawn until now). Not for the editor that is showing (again) or
    /// fading still, nor for one that moved to another pane, which draws it now.
    fn let_go(&self, gone: Leaving, showing: Option<EntityId>, cx: &mut App) {
        let id = gone.editor.entity_id();
        let held = showing == Some(id) || self.editor.as_ref().is_some_and(|editor| editor.entity_id() == id) || self.leaving.iter().any(|leaving| leaving.editor.entity_id() == id) || !self.known.iter().any(|known| known.entity_id() == id);
        if !held {
            gone.editor.update(cx, |state, cx| {
                state.release_layout();
                // The matches of an open Find go too; the tab scans them again when it shows.
                state.release_search_matches(cx);
            });
        }
    }

    /// An editor is still fading in over the empty state: the workspace keeps drawing the empty state.
    pub fn entering(&self) -> bool {
        self.entering.is_some()
    }

    /// `editor` moved to another pane: this one no longer lays it out ahead.
    pub fn forget(&mut self, editor: EntityId) {
        self.known.retain(|known| known.entity_id() != editor);
        self.queue.retain(|queued| queued.entity_id() != editor);
        self.fitted.retain(|(id, _)| *id != editor);
        self.marks.retain(|(id, _)| *id != editor);
    }

    /// Records the editor's top and place for the other panes, in the frame's prepaint right
    /// after the editor's own (a canvas the same size, laid over it), and wakes the preview pane
    /// for the next frame when the top moved while it shows, or a wheel went over the editor
    /// (its glide first moves in the frame after the wheel's own, and the preview pane woken
    /// then follows in that frame). A wheel the editor can't take wakes nothing: one up at its
    /// top or one down at its bottom. A wheel or a click over the editor makes it lead the
    /// preview. With rects on, it also records
    /// `editor-fresh:<pane>`, the rect of a pane whose editor drew this frame: a pane's
    /// `editor:<pane>` rect with no such row beside it is stale (`unmark`).
    fn probe(&self, editor: Entity<EditorState>, pane: EntityId, lays: bool, feed: crate::scrollbar::Feed) -> impl IntoElement {
        let (link, preview, shows) = (self.link.clone(), self.preview, self.shows && self.active);
        let (mark, width) = (link.clone(), self.width.clone());
        let drawn_editor = crate::control::rects_enabled().then(|| editor.clone());
        canvas(
            move |bounds, window, cx| {
                link.bounds.set(bounds);
                if crate::control::rects_enabled() {
                    crate::control::record(window, fresh_id(pane), bounds);
                }
                // A wheel over the editor wakes the preview too (see `EditorLink::take_wheel`), unless
                // the editor is at the end the wheel pushes toward, by the extents the scrollbar's feed
                // holds from the editor's own prepaint: `up` is the wheel's way (0 for a gesture with none).
                let wheeled = link.take_wheel();
                let moved = link.editor_moved(&editor, cx);
                let wheeled = wheeled.is_some_and(|up| {
                    let offset = editor.read(cx).scroll_offset().y;
                    let max = (feed.content_size().height - feed.viewport_bounds().size.height).max(px(0.));
                    !(if up > 0. { offset >= px(0.) } else if up < 0. { -offset >= max - px(1. / window.scale_factor()) } else { false })
                });
                if shows && (moved | wheeled) {
                    window.on_next_frame(move |_, cx| cx.notify(preview));
                }
                // The editor's last paint left its width (the previous frame's): a new width wakes the
                // pane, but only when it has hidden editors to lay out (`lays`).
                let drawn = editor.read(cx).drawn_width().zip(editor.read(cx).drawn_size());
                if width.replace(drawn) != drawn && lays {
                    window.on_next_frame(move |_, cx| cx.notify(pane));
                }
            },
            move |bounds, (), window, cx| {
                if shows {
                    mark.mark(Driver::Editor, bounds, window);
                }
                // The editor painted before this canvas, so its layout is this frame's.
                if crate::control::rects_enabled() {
                    if let Some((starts, _, Some(wrap))) = drawn_editor.as_ref().and_then(|editor| editor.read(cx).drawn_column()) {
                        let column = Bounds::new(point(bounds.origin.x + starts, bounds.origin.y), size(wrap, bounds.size.height));
                        crate::control::record(window, format!("column:{}", pane.as_u64()), column);
                    }
                }
            },
        )
        .absolute()
        .inset_0()
    }

    /// Lays out the tabs this pane showed before, now hidden, for the editor's new width, so
    /// switching to one finds its rows wrapped (a big file's wrap is most of the first frame's
    /// cost). Waits `PRELAYOUT_SETTLE_FRAMES` for a width that holds still, then wraps tabs for
    /// one `PRELAYOUT_SLICE` a frame; frames are asked only while there is something left to do.
    fn prelayout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(fit) = self.width.get() else { return };
        let width = fit.0;
        let pane = cx.entity_id();
        if self.laid != Some(fit) {
            let held = match self.settle {
                Some((was, frames)) if was == fit => frames + 1,
                _ => 0,
            };
            if held < PRELAYOUT_SETTLE_FRAMES {
                self.settle = Some((fit, held));
                window.on_next_frame(move |_, cx| cx.notify(pane));
                return;
            }
            self.settle = None;
            self.laid = Some(fit);
            self.known.retain(|known| known.upgrade().is_some());
            let showing = self.editor.as_ref().map(|editor| editor.entity_id());
            self.fitted.retain(|(id, _)| self.known.iter().any(|known| known.entity_id() == *id));
            self.marks.retain(|(id, _)| self.known.iter().any(|known| known.entity_id() == *id));
            let fitted = &self.fitted;
            self.queue = self
                .known
                .iter()
                .filter(|known| Some(known.entity_id()) != showing && !fitted.contains(&(known.entity_id(), fit)))
                .cloned()
                .collect();
        }
        // The showing tab's look: a tab that hasn't drawn yet has none, so the queue waits a frame for its first draw.
        if self.queue.is_empty() {
            return;
        }
        let Some(look) = self.editor.as_ref().and_then(|showing| showing.read(cx).drawn_look()) else {
            window.on_next_frame(move |_, cx| cx.notify(pane));
            return;
        };
        let differ = settings::get(cx).editor.prose_style != settings::get(cx).editor.code_style;
        // One frame's slice goes to as many tabs as it holds: tabs already wrapped cost almost
        // nothing and are done at once, and a big one takes what is left and carries on next frame.
        let started = Instant::now();
        while let Some(queued) = self.queue.last() {
            let left = PRELAYOUT_SLICE.saturating_sub(started.elapsed());
            if left.is_zero() {
                break;
            }
            let editor = queued.upgrade().filter(|editor| self.editor.as_ref() != Some(editor));
            if let Some(editor) = &editor {
                // Out of `fitted` while its wrap is under way, so a slice cut short leaves no stale entry.
                let id = editor.entity_id();
                self.fitted.retain(|(fitted, _)| *fitted != id);
                // A hidden tab of the other kind draws in a style of its own: it keeps its font and row share, at the pane's size.
                let own = if differ && self.kinds.iter().any(|(kind, prose)| *kind == id && *prose != self.prose) {
                    editor.read(cx).drawn_look().map(|mut own| {
                        own.line_height = own.line_height * (look.size / own.size);
                        own.size = look.size;
                        own
                    })
                } else {
                    None
                };
                if editor.update(cx, |state, cx| state.prelayout(width, own.as_ref().unwrap_or(&look), left, window, cx)) {
                    break;
                }
                self.fitted.push((id, fit));
            }
            self.queue.pop();
        }
        if !self.queue.is_empty() {
            window.on_next_frame(move |_, cx| cx.notify(pane));
        }
    }
}

impl Render for EditorPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = settings::get(cx);
        let (weight, editor_settings, size) = (settings.text.weight, settings.editor.clone(), settings.editor.font_size);
        let (prose_style, code_style) = (editor_settings.prose_style, editor_settings.code_style);
        let mono_family = editor_settings.drawn_family(cx);
        let note = self.note.clone();
        let styled = |state: &Entity<EditorState>, style: Typeface| {
            // Weak: Kit stores this closure on the editor state, so a strong handle is a cycle that keeps a closed tab's text alive.
            let menu_for = state.downgrade();
            let paste_for = state.downgrade();
            let note = note.clone();
            Editor::new(state)
                // Right-click shows ours; Kit's builder gets an empty menu back, which shows nothing.
                .context_menu(move |menu, window, cx| {
                    if let Some(editor) = menu_for.upgrade() {
                        show_editor_menu(&editor, window, cx);
                    }
                    menu
                })
                // An image on the pasteboard in a saved Markdown file lands beside it (`paste_image.rs`); anything else pastes as before.
                .on_paste(move |item, window, cx| crate::paste_image::paste(&note, &paste_for, item, window, cx))
                .bordered(false)
                .size_full()
                // One family, weight and row height for the whole editor (View > Text style): rows are fixed height.
                .font_weight(style.weight(weight))
                .when_some(style.family(mono_family.clone()), |editor, family| editor.font_family(family))
                .when_some(style.line_height(), |editor, height| editor.line_height(relative(height)))
                .when_some(size, |editor, size| editor.text_size(px(size))) // design: settings-driven font size
        };
        let showing = self.editor.as_ref().map(|editor| editor.entity_id());
        let lays = !self.queue.is_empty() || self.known.iter().any(|known| Some(known.entity_id()) != showing);
        self.prelayout(window, cx);
        let id = window.window_handle().window_id();
        self.window = Some(id);
        let now = crate::motion::drawn_now(id);
        let had_leaving = !self.leaving.is_empty();
        for leaving in &mut self.leaving {
            leaving.start(now);
        }
        self.drop_faded(now, showing, cx);
        let mut landed = had_leaving && self.leaving.is_empty();
        if let Some(entering) = &mut self.entering {
            entering.start(now);
        }
        let level = self.entering.as_ref().map(|entering| entering.alpha(now));
        if level.is_some_and(|level| level >= 1.) {
            self.entering = None;
            landed = true;
        }
        // The style the settings want for the showing editor's kind of file; a pick changes it through a dip of the text's opacity.
        let dip = self.style.step(if self.prose { prose_style } else { code_style }, self.prose, now);
        let shown_level = level.unwrap_or(1.) * dip;
        // The caret layer shows through as much as the old editors have faded, and as much as the text has dipped; 1 once none is left.
        self.cover.set(self.leaving.iter().map(|leaving| 1. - leaving.alpha(now)).product::<f32>() * shown_level);
        // Frames come from this view alone, and only while an editor still fades; the layer
        // is woken with it, since it follows the same fade.
        if !self.leaving.is_empty() || self.entering.is_some() || self.style.moving() {
            let (pane, layer) = (cx.entity_id(), self.layer.entity_id());
            window.on_next_frame(move |_, cx| {
                cx.notify(pane);
                cx.notify(layer);
            });
        }
        if landed {
            // The pane has no editor left to draw: Kit's record of the last focused input goes with it, and so do the scrollbar's Find marks.
            if self.editor.is_none() {
                window.focused_input(cx);
                self.let_go_marks(cx);
            }
            // The workspace stops drawing this pane's editor, or the empty state under it.
            cx.emit(Landed);
        }
        let leaving: Vec<_> = self
            .leaving
            .iter()
            // The cover on top takes the clicks and the pointer: the editor's own hitboxes sit above its wrapper's occlude.
            .map(|leaving| div().absolute().inset_0().occlude().opacity(leaving.alpha(now)).child(styled(&leaving.editor, leaving.style)).child(div().absolute().inset_0().occlude()))
            .collect();
        // The editor's scrollbar is drawn by the caret layer in place of Kit's; its hand-scroll
        // clock lives as long as the pane keeps rendering that editor.
        let Some(editor) = self.editor.clone() else {
            unmark(id, cx.entity_id());
            // The last tab went: the old editor's last frame fades out over what the workspace draws under it, and the status line goes with it on the same level.
            let status = self.leaving.last().filter(|_| self.status_level > 0.).map(|leaving| {
                div().absolute().bottom_0().left_0().right_0().h(STATUS_HEIGHT).opacity(leaving.alpha(now) * self.status_level).child(AnyView::from(self.status.clone()).cached(StyleRefinement::default().size_full()))
            });
            return div().relative().size_full().pt(self.band).pb(STATUS_HEIGHT * self.status_level).child(div().relative().size_full().children(crate::control::fresh("find")).children(leaving)).children(status).into_any_element();
        };
        // The marks live on the pane, not in the keyed state below: GPUI drops that on a frame this
        // tab doesn't draw. A tab that stopped showing lets go of its match list and ticks (they
        // scan anew on the way back), so a hidden tab with Find open holds none.
        for (id, marks) in &self.marks {
            if *id != editor.entity_id() {
                marks.release();
            }
        }
        let marks = match self.marks.iter().find(|(id, _)| *id == editor.entity_id()) {
            Some((_, marks)) => marks.clone(),
            None => {
                let marks = crate::scrollbar::Marks::default();
                self.marks.push((editor.entity_id(), marks.clone()));
                marks
            }
        };
        self.layer_shows.set(Some(editor.entity_id()));
        let (pane, layer_shows) = (cx.entity_id(), self.layer_shows.clone());
        let bar = window.use_keyed_state(("editor-scroller", editor.entity_id()), cx, |_, cx| {
            let bar = crate::scrollbar::Bar {
                scroller: crate::scrollbar::Scroller::default(),
                feed: crate::scrollbar::Feed::new(editor.downgrade(), pane),
            };
            let (feed, scroller, mine) = (bar.feed.clone(), bar.scroller.clone(), editor.entity_id());
            // Weak: the editor holds this renderer, so a strong handle would keep it alive forever.
            let shown = editor.downgrade();
            editor.update(cx, |state, _| {
                // The shown editor's knob and Find's ticks are drawn by the caret layer's view, from what this records (`scrollbar::Feed`); the editor paints only the header shadow. An editor fading out is no longer the layer's: it paints its own knob, which fades with its text.
                state.set_scrollbar_renderer(Some(Rc::new(move |context, window, cx| {
                    let finding = shown.upgrade().is_some_and(|editor| editor.read(cx).search_session().is_active());
                    let theme = cx.theme();
                    // The placeholders are never painted while Find is closed (`paint_marks` returns at `!marks.on`).
                    let (find, current) = if finding {
                        let fills = gpui_kit::base::theme_tokens::find_fills(theme.foreground, theme.background.l < 0.5, crate::control::window_key(window));
                        (fills.matches, fills.current)
                    } else {
                        (Hsla::default(), Hsla::default())
                    };
                    let ink = crate::scrollbar::MarkInk {
                        find,
                        current,
                        caret: theme.caret.opacity(crate::tokens::MARK_CARET_ALPHA),
                    };
                    let sideways = context.axis() == gpui_kit::base::ScrollbarAxis::Both;
                    let handle: Rc<dyn gpui_kit::base::ScrollbarHandle> = Rc::new(context.clone());
                    let shadow = crate::scrollbar::header_shadow(handle.clone(), crate::theme::is_light(cx));
                    feed.record(context, ink);
                    let root = div().absolute().inset_0().child(shadow);
                    if layer_shows.get() == Some(mine) {
                        root.into_any_element()
                    } else {
                        let overlay = crate::scrollbar::Overlay { handle, scroller: scroller.clone(), ground: None };
                        root.child(crate::scrollbar::overlay_sideways(overlay, sideways)).into_any_element()
                    }
                })));
            });
            bar
        });
        let bar = bar.read(cx).clone();
        let bar_feed = bar.feed.clone();
        self.layer.update(cx, |layer, cx| layer.set_bar(&editor, bar, marks, cx));
        // Color swatches (`swatches.rs`): a view of its own over the editor, so a scroll repaints it and not the pane.
        let swatches = crate::swatches::chips(&editor, window, cx).map(|chips| AnyView::from(chips).cached(StyleRefinement::default().absolute().inset_0()));
        div()
            .relative()
            .size_full()
            .pt(self.band)
            // The status line takes the pane's foot; the editor, its scrollbar and the preview's follow all see the shorter place.
            .pb(STATUS_HEIGHT * self.status_level)
            .child(
                div()
                    .relative()
                    .size_full()
                    .children(crate::control::fresh("find"))
                    .child({
                        // Template tab stops (`tab_stops.rs`): Tab hops to the next, Escape or a click ends it; with none going, these pass everything on.
                        let (hopping, ending, clicked) = (editor.clone(), editor.clone(), editor.clone());
                        div()
                            .relative()
                            .size_full()
                            .when(shown_level < 1., |shown| shown.opacity(shown_level))
                            .capture_action(move |_: &kit::IndentInline, _, cx| {
                                if crate::tab_stops::next(&hopping, cx) {
                                    cx.stop_propagation();
                                }
                            })
                            .capture_action(move |_: &kit::Escape, _, cx| {
                                // Escape ends a reading aloud first (`speech.rs`), then a template's tab stops.
                                let reading = crate::speech::end(&ending, cx);
                                if crate::tab_stops::end(&ending, cx) || reading {
                                    cx.stop_propagation();
                                }
                            })
                            .capture_any_mouse_down(move |_, _, cx| {
                                crate::speech::end(&clicked, cx);
                                crate::tab_stops::end(&clicked, cx);
                            })
                            // A trackpad pinch over the text steps the size like Cmd-+ and Cmd-- (`app::pinch_text`).
                            .on_pinch(|event, _, cx| crate::app::pinch_text(event.delta, event.phase, cx))
                            .child(styled(&editor, self.style.shown))
                    })
                    .children(leaving)
                    .child(self.probe(editor.clone(), cx.entity_id(), lays, bar_feed))
                    .children(swatches)
                    .children(crate::control::track_scrolled("editor", move |cx| editor.read(cx).scroll_offset())),
            )
            .when(self.status_level > 0., |pane| {
                pane.child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .h(STATUS_HEIGHT)
                        .when(self.status_level < 1., |line| line.opacity(self.status_level))
                        .child(AnyView::from(self.status.clone()).cached(StyleRefinement::default().size_full())),
                )
            })
            .into_any_element()
    }
}

/// What the editor's right-click menu can do.
#[derive(Clone)]
enum EditorCommand {
    LookUp,
    Cut,
    Copy,
    Paste,
    SelectAll,
    ToggleComment,
    UseSelectionForFind,
    RevealInExplorer,
    Guess(String),
    LearnSpelling,
    IgnoreSpelling,
}

/// Opens the editor's right-click menu (a native one, in place of GPUI Kit's) once the frame has
/// drawn: nothing draws while a native menu is open. Two frames on, since a callback runs at the
/// start of the frame after the one it was queued in. Cut, Copy and Paste grey out as the edit
/// menu's do: no selection, or a file that can't change.
pub fn show_editor_menu(editor: &Entity<EditorState>, window: &mut Window, cx: &mut App) {
    let (selected, editable) = {
        let state = editor.read(cx);
        (!state.selected_range().is_empty(), state.presentation().is_editable())
    };
    let item = |title: &str, run: EditorCommand, symbol: &'static str, key: &'static str, modifiers: usize| {
        Entry::Item(Item::new(title, run).symbol(symbol).key(key, modifiers))
    };
    let disabled = |entry: Entry<EditorCommand>, off: bool| match entry {
        Entry::Item(item) => Entry::Item(item.disabled(off)),
        other => other,
    };
    let mut entries = Vec::with_capacity(16);
    // The underlined word at the caret: the Mac's guesses, then Learn and Ignore.
    let misspelled = crate::spell::misspelling_at_caret(editor, cx);
    if let Some(found) = &misspelled {
        if found.guesses.is_empty() {
            entries.push(Entry::Item(Item::new("No guesses found", EditorCommand::IgnoreSpelling).disabled(true)));
        }
        entries.extend(found.guesses.iter().map(|guess| Entry::Item(Item::new(guess.as_str(), EditorCommand::Guess(guess.clone())).disabled(!editable))));
        entries.push(Entry::Separator);
        entries.push(Entry::Item(Item::new("Learn spelling", EditorCommand::LearnSpelling)));
        entries.push(Entry::Item(Item::new("Ignore spelling", EditorCommand::IgnoreSpelling)));
        entries.push(Entry::Separator);
    }
    if let Some(title) = native_menu::lookup_title(editor.read(cx)) {
        entries.push(item(&title, EditorCommand::LookUp, "text.book.closed", "d", COMMAND | native_menu::CONTROL));
        entries.push(Entry::Separator);
    }
    entries.extend([
        disabled(item("Cut", EditorCommand::Cut, "scissors", "x", COMMAND), !(selected && editable)),
        disabled(item("Copy", EditorCommand::Copy, "doc.on.doc", "c", COMMAND), !selected),
        disabled(item("Paste", EditorCommand::Paste, "doc.on.clipboard", "v", COMMAND), !editable),
        Entry::Separator,
        item("Select all", EditorCommand::SelectAll, "selection.pin.in.out", "a", COMMAND),
        Entry::Separator,
        disabled(item("Toggle comment", EditorCommand::ToggleComment, "text.quote", "/", COMMAND), !editable),
        disabled(item("Use selection for find", EditorCommand::UseSelectionForFind, "magnifyingglass", "e", COMMAND), !selected),
        Entry::Separator,
        item("Reveal current file", EditorCommand::RevealInExplorer, "scope", "j", COMMAND | SHIFT),
    ]);
    let editor = editor.clone();
    window.on_next_frame(move |window, _| {
        window.on_next_frame(move |window, cx| {
            window
                .spawn(cx, async move |cx| {
                    let Some(command) = native_menu::pop_up(&entries, None) else { return };
                    cx.update(|window, cx| {
                        editor.update(cx, |state, cx| state.focus(window, cx));
                        match command {
                            EditorCommand::LookUp => window.dispatch_action(Box::new(crate::app::LookUp), cx),
                            EditorCommand::Cut => window.dispatch_action(Box::new(kit::Cut), cx),
                            EditorCommand::Copy => window.dispatch_action(Box::new(kit::Copy), cx),
                            EditorCommand::Paste => window.dispatch_action(Box::new(kit::Paste), cx),
                            EditorCommand::SelectAll => window.dispatch_action(Box::new(kit::SelectAll), cx),
                            EditorCommand::ToggleComment => window.dispatch_action(Box::new(kit::ToggleComments), cx),
                            EditorCommand::UseSelectionForFind => editor.update(cx, |state, cx| state.use_selection_for_find(cx)),
                            EditorCommand::RevealInExplorer => window.dispatch_action(Box::new(RevealCurrentFile), cx),
                            EditorCommand::Guess(guess) => {
                                if let Some(found) = &misspelled {
                                    crate::spell::replace(&editor, found, &guess, window, cx);
                                }
                            }
                            EditorCommand::LearnSpelling => {
                                if let Some(found) = &misspelled {
                                    crate::spell::learn(&found.word, cx);
                                }
                            }
                            EditorCommand::IgnoreSpelling => {
                                if let Some(found) = &misspelled {
                                    crate::spell::ignore(&found.word, cx);
                                }
                            }
                        }
                    })
                    .ok();
                })
                .detach();
        });
    });
}

/// A preview that was showing and is fading out over the one that came forward; the clock starts
/// like `Leaving`'s, on the frame after the new preview's first draw.
struct LeavingPreview {
    /// Weak: a preview fading out never keeps its document alive, so a closed tab's text and
    /// parse go with the tab even while the panel is hidden and nothing prunes this list.
    document: WeakEntity<Document>,
    /// How much of it showed when it began to fade.
    from: f32,
    /// When the fade began: `None` until the new preview has been drawn once.
    since: Option<Instant>,
    /// A render has drawn the new preview since the switch.
    drawn: bool,
}

impl LeavingPreview {
    fn alpha(&self, now: Instant) -> f32 {
        let Some(since) = self.since else { return self.from };
        let u = now.saturating_duration_since(since).as_secs_f32() / FADE_QUICK;
        if u >= 1. { 0. } else { self.from * (1. - ease_out(u)) }
    }

    fn start(&mut self, now: Instant) {
        if self.since.is_none() {
            if self.drawn {
                self.since = Some(now);
            } else {
                self.drawn = true;
            }
        }
    }
}

/// The markdown preview of the showing document, in the panel right of the editor.
pub struct PreviewPane {
    /// Weak: the preview doesn't keep a closed tab's document alive; it draws nothing once it's gone.
    document: WeakEntity<Document>,
    link: Rc<EditorLink>,
    /// The caret wash over the preview's block: a view of its own beside this pane in the
    /// window (`wash_layer`), so its frames never render the preview.
    layer: Entity<WashLayer>,
    /// The window this pane last drew in: where a switch reads the frame clock.
    window: Option<WindowId>,
    /// Previews fading out over the showing one, oldest first.
    leaving: Vec<LeavingPreview>,
    /// A switch left images to release once the fade lands, and the frame after it has drawn.
    release: bool,
    /// The documents whose previews left since the last release: their shaped lines go with the
    /// images, once the fade has landed.
    shed: Vec<WeakEntity<Document>>,
    settled: bool,
    /// The last document `set` was given has a preview, so the panel can show it.
    following: bool,
    /// The window's panel is on (its setting, or Shift-Cmd-V while minimal), as the workspace last said.
    visible: bool,
    /// The panel stopped showing and its slide is landing: when it has, the images go.
    hiding: Option<Task<()>>,
    /// The images were released since the panel stopped showing.
    released: bool,
    /// The window this pane draws in, once drawn: the owner its pictures are cached under.
    owner: Option<u64>,
    /// Wakes when the Mac's accent changes: ticked task boxes are drawn from it on every render.
    _accent: Option<Subscription>,
    /// The showing preview's first parse is still running: it draws blank, and this pane wakes on
    /// the parse landing (`watch`).
    awaiting: bool,
    watch: Option<Subscription>,
    /// The preview whose first parse just landed, fading in from nothing (`FADE_QUICK`).
    fade_in: Option<Entering>,
    /// The awaited parse is a cooled preview parsing again: no fade when it lands.
    rewarm: bool,
}

impl PreviewPane {
    pub fn new(document: Entity<Document>, link: Rc<EditorLink>, cx: &mut Context<Self>) -> Self {
        let this = cx.weak_entity();
        let layer = cx.new(|_| WashLayer::new(this));
        link.layer.set(Some(layer.entity_id()));
        Self {
            document: document.downgrade(),
            link,
            layer,
            window: None,
            leaving: Vec::new(),
            release: false,
            shed: Vec::new(),
            settled: false,
            following: true,
            visible: settings::get(cx).preview.visible,
            hiding: None,
            released: false,
            owner: None,
            _accent: None,
            awaiting: false,
            watch: None,
            fade_in: None,
            rewarm: false,
        }
    }

    /// The caret wash's view: the window draws it right after this pane, over the preview.
    pub fn wash_layer(&self) -> Entity<WashLayer> {
        self.layer.clone()
    }

    /// The editor link the pane follows now.
    pub fn link(&self) -> Rc<EditorLink> {
        self.link.clone()
    }

    /// Whether the panel is showing a preview: it's open and the showing document has one.
    fn shows(&self, cx: &App) -> bool {
        self.visible && self.following && self.document.upgrade().is_some_and(|document| document.read(cx).preview.is_some())
    }

    /// Frees the decoded pictures and shaped lines once the preview stops showing, after its slide
    /// has landed (it draws them until then), and clears the wait when it shows again. No frame is
    /// asked for: the panel isn't drawn while hidden, so `render` never gets to release them.
    fn check_hidden(&mut self, cx: &mut Context<Self>) {
        let shows = self.shows(cx);
        if shows {
            self.hiding = None;
            self.released = false;
            return;
        }
        if self.released || self.hiding.is_some() {
            return;
        }
        self.hiding = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs_f32(COLUMN_SLIDE) + Duration::from_millis(150)).await;
            this.update(cx, |pane, cx| {
                pane.hiding = None;
                // The cache is shared by every window: this release frees only what no other window shows.
                if !pane.shows(cx) {
                    if let Some(owner) = pane.owner {
                        release_embedded_images(owner, cx);
                    }
                    // The shaped lines of every block the preview painted go with them; the parsed
                    // document stays, so showing again shapes what is in view in its first frame.
                    if let Some(document) = pane.document.upgrade()
                        && let Some(preview) = document.read(cx).preview.as_ref()
                    {
                        preview.state.read(cx).release_layouts();
                    }
                    pane.released = true;
                }
            })
            .ok();
        }));
    }

    /// The window has no document to follow (its last tab closed): the panel shows nothing, so the
    /// pictures go after the slide like a hidden preview's.
    pub fn follow_none(&mut self, cx: &mut Context<Self>) {
        self.following = false;
        self.check_hidden(cx);
    }

    /// The workspace says whether the panel is on (a settings change, or Minimal mode's own toggle): a hidden
    /// panel frees its pictures once its slide lands.
    pub fn set_visible(&mut self, on: bool, cx: &mut Context<Self>) {
        self.visible = on;
        self.check_hidden(cx);
    }

    /// Shows `document`'s preview, following the editor `link` leaves (the active pane's). A
    /// document with no preview is ignored: the panel is closing or closed, and the preview it
    /// shows stays until it is gone. Another document crossfades: the old preview stays on top and
    /// fades out over `FADE_QUICK`, from the share it still shows if it was already fading.
    pub fn set(&mut self, document: Entity<Document>, link: Rc<EditorLink>, visible: bool, cx: &mut Context<Self>) {
        // Read before `following` and `visible` are overwritten: a panel that was shut (a code tab, or hidden) has nothing
        // on screen to fade. One still sliding shut counts: it draws the old preview until it lands.
        let was_shown = self.shows(cx) || self.hiding.is_some();
        self.visible = visible;
        if self._accent.is_none() {
            self._accent = Some(cx.observe_global::<crate::explorer::icons::AccentTick>(|_, cx| cx.notify()));
        }
        link.layer.set(Some(self.layer.entity_id()));
        let same = self.document.entity_id() == document.entity_id();
        if !same && document.read(cx).preview.is_none() {
            self.following = false;
            self.check_hidden(cx);
            return;
        }
        self.following = true;
        if !same || !Rc::ptr_eq(&self.link, &link) {
            if !same {
                if let Some(preview) = document.read(cx).preview.as_ref() {
                    preview.forget_reload();
                }
                // The new preview starts its own wait; the old one's level carries into its fade out.
                let level = self.fade_level(sampled(self.window));
                self.awaiting = false;
                self.watch = None;
                self.fade_in = None;
                let old = self.document.upgrade().filter(|old| old.read(cx).preview.is_some());
                if fades_out(was_shown, old.is_some()) && let Some(old) = old.as_ref() {
                    // The old preview still draws its images while it fades; they go when it lands.
                    self.release = true;
                    self.shed.push(old.downgrade());
                    self.settled = false;
                    let now = sampled(self.window);
                    self.leaving.retain(|leaving| leaving.alpha(now) > 0.01);
                    let from = self.leaving.last().map_or(1., |leaving| 1. - leaving.alpha(now));
                    let from = from * level;
                    self.leaving.push(LeavingPreview { document: old.downgrade(), from, since: None, drawn: false });
                    if self.leaving.len() > 3 {
                        self.leaving.remove(0);
                    }
                } else {
                    // Nothing shows, so nothing fades: the old preview's pictures and shaped lines go at once,
                    // with any that were still waiting on a fade.
                    let mut gone: Vec<_> = old.iter().map(|old| old.downgrade()).collect();
                    if !was_shown {
                        self.leaving.clear();
                        self.release = false;
                        gone.append(&mut self.shed);
                    }
                    for left in gone {
                        if let Some(left) = left.upgrade().filter(|left| left.entity_id() != document.entity_id())
                            && let Some(preview) = left.read(cx).preview.as_ref()
                        {
                            preview.state.read(cx).release_layouts();
                        }
                    }
                    if let Some(owner) = self.owner {
                        release_embedded_images(owner, cx);
                    }
                }
            }
            self.document = document.downgrade();
            self.link = link;
            cx.notify();
        }
        self.check_hidden(cx);
    }
}

/// Whether a switch to another preview draws the old one fading out: the panel was showing a
/// preview, and the old document has one.
fn fades_out(was_shown: bool, old_has_preview: bool) -> bool {
    was_shown && old_has_preview
}

impl PreviewPane {
    /// How much of the showing preview is drawn at `now`: nothing while its first parse runs,
    /// then the fade in, else all of it.
    fn fade_level(&self, now: Instant) -> f32 {
        if self.awaiting {
            return 0.;
        }
        self.fade_in.as_ref().map_or(1., |entering| entering.alpha(now))
    }
}

impl Render for PreviewPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let window_id = window.window_handle().window_id();
        self.window = Some(window_id);
        if self.owner.is_none() {
            // The cache keys what this window's previews draw by the window; its pictures go with it.
            let owner = window_id.as_u64();
            self.owner = Some(owner);
            cx.on_release(move |_, cx| forget_embedded_owner(owner, cx)).detach();
        }
        let now = crate::motion::drawn_now(window_id);
        for leaving in &mut self.leaving {
            leaving.start(now);
        }
        self.leaving.retain(|leaving| leaving.alpha(now) > 0.01 && leaving.document.upgrade().is_some());
        // Frames come from this view alone, and only while an old preview still fades.
        if !self.leaving.is_empty() {
            self.settled = false;
            let pane = cx.entity_id();
            window.on_next_frame(move |_, cx| cx.notify(pane));
        } else if self.release {
            // The fade landed: one more frame draws only the showing preview, then what it didn't
            // draw is released, so it keeps its images and the leaving one lost none mid-fade.
            if self.settled {
                if let Some(owner) = self.owner {
                    release_unseen_embedded_images(owner, cx);
                }
                // The previews that left keep no shaped lines either; one shown again shapes
                // what is in view in its first frame. The showing one is never released.
                let showing = self.document.entity_id();
                for left in std::mem::take(&mut self.shed) {
                    if let Some(left) = left.upgrade().filter(|left| left.entity_id() != showing)
                        && let Some(preview) = left.read(cx).preview.as_ref()
                    {
                        preview.state.read(cx).release_layouts();
                    }
                }
                self.release = false;
            } else {
                self.settled = true;
                let pane = cx.entity_id();
                window.on_next_frame(move |_, cx| cx.notify(pane));
            }
        }
        // The first parse of a big file lands into a blank preview: it fades in from nothing, the
        // clock starting on the frame after the first one that draws the parsed text.
        let state = self.document.upgrade().and_then(|document| document.read(cx).preview.as_ref().map(|preview| preview.state.clone()));
        if state.as_ref().is_some_and(|state| state.read(cx).awaiting_first_parse()) {
            if !self.awaiting {
                self.awaiting = true;
                // A state that parsed before and was emptied by cooling is parsing again: it
                // stays blank until it lands, then shows at once; only a first open fades.
                self.rewarm = state.as_ref().is_some_and(|state| state.read(cx).has_parsed());
                self.watch = state.map(|state| cx.observe(&state, |_, _, cx| cx.notify()));
            }
        } else if self.awaiting {
            self.awaiting = false;
            self.watch = None;
            if state.is_some() && !self.rewarm {
                self.fade_in = Some(Entering::default());
            }
        }
        let mut fade = self.awaiting.then_some(0.);
        if let Some(entering) = &mut self.fade_in {
            entering.start(now);
            let level = entering.alpha(now);
            if entering.since.is_some() && level >= 1. {
                self.fade_in = None;
            } else {
                fade = Some(level);
                let pane = cx.entity_id();
                window.on_next_frame(move |_, cx| cx.notify(pane));
            }
        }
        let ground = cx.theme().background;
        let leaving: Vec<_> = self
            .leaving
            .iter()
            .filter_map(|leaving| {
                let document = leaving.document.upgrade()?;
                let document = document.read(cx);
                let preview = document.preview.as_ref()?;
                Some(
                    div()
                        .absolute()
                        .inset_0()
                        .occlude()
                        .bg(ground)
                        .opacity(leaving.alpha(now))
                        .child(text_view(&preview.state, document.path.as_deref()).size_full()),
                )
            })
            .collect();
        let document = self.document.upgrade();
        let document = document.as_ref().map(|document| document.read(cx));
        // Nothing to show: no text in the editor and nothing parsed from it. A big file still
        // parsing, a cold preview and a stale one all have text, so none reads as empty.
        let empty = document.is_some_and(|document| {
            let blank = document.built_editor().is_some_and(|editor| editor.read(cx).text().chars().all(char::is_whitespace));
            blank && document.preview.as_ref().is_some_and(|preview| preview.state.read(cx).headings().is_some_and(|headings| headings.is_empty()))
        });
        // The follow element comes first: it prepaints before the text, so the preview moves in
        // the editor's frame.
        div()
            .relative()
            .size_full()
            .children(document.and_then(|document| {
                let preview = document.preview.as_ref()?;
                Some(
                    div()
                        .size_full()
                        .when_some(fade, |this, level| this.opacity(level))
                        .child(preview.follow(&self.link))
                        .child(text_view(&preview.state, document.path.as_deref()).size_full())
                        .children(crate::control::track_scrolled("preview", {
                            let list = preview.state.read(cx).list_state().clone();
                            move |_| list.scroll_px_offset_for_scrollbar()
                        }))
                        .when(empty, |this| {
                            this.child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .child(empty_state::fitted("preview-empty", "doc.text", "Nothing to preview", "Type Markdown to see it here.".into(), vec![], true, cx)),
                            )
                        })
                        .child(preview.lead(&self.link)),
                )
            }))
            .children(leaving)
    }
}

/// A pane's name in the split tree: stable while the window lives, whatever its place.
pub type PaneId = usize;

/// Which child of a split is going away.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Side {
    First,
    Second,
}

/// The split tree of the window's middle panel: panes at the leaves, two children at each split.
/// A split gives its first child `ratio` of its length (left or top) and the second the rest.
pub enum Node {
    Leaf(PaneId),
    Split(Box<Split>),
}

pub struct Split {
    pub id: usize,
    pub axis: SplitAxis,
    /// The first child's share once it has settled; a drag sets it and clears `glide`.
    pub ratio: f32,
    /// A pane opening or closing, or the split evening out or springing back from a stretch: the
    /// share drawn until it lands.
    pub glide: Option<Glide>,
    /// A divider dragged past `PANE_MIN` and still held: the share drawn, past the limit `ratio`
    /// keeps. Set by the drag, taken by its release.
    pub stretch: Option<f32>,
    /// A child (a pane, or a split holding the one pane left) is closing: the glide heads for its end, and the child goes when it lands.
    pub closing: Option<Side>,
    pub first: Node,
    pub second: Node,
    /// Where the split sat in the last frame, for a divider drag.
    pub bounds: Rc<Cell<Bounds<Pixels>>>,
}

impl Split {
    /// The first child's share drawn at `now`.
    pub fn share(&self, now: Instant) -> f32 {
        self.stretch.unwrap_or_else(|| self.glide.map_or(self.ratio, |glide| glide.level(now)))
    }

    /// How much of the split's length each child is laid out at, as a share, when that is more
    /// than it is drawn at: a child that shrinks (the pane a split opens beside, the one that
    /// closes, the one a stretch squeezes) keeps its widest layout until it lands and a child that
    /// grows is laid out at its widest, so the moving divider covers both and nothing is bare or
    /// rewrapped on the way. `length` is the split's size along its axis.
    pub fn held(&self, now: Instant, length: f32) -> (Option<f32>, Option<f32>) {
        if let Some(glide) = self.glide.filter(|glide| !glide.is_done(now)) {
            let (low, high) = glide.range();
            return (Some(high.clamp(0., 1.)), Some((1. - low).clamp(0., 1.)));
        }
        match self.stretch {
            Some(drawn) => {
                let least = (crate::tokens::PANE_MIN / length).min(0.5);
                if drawn < least {
                    (Some(least), None)
                } else if drawn > 1. - least {
                    (None, Some(least))
                } else {
                    (None, None)
                }
            }
            None => (None, None),
        }
    }
}

impl Node {
    fn collect(&self, live: bool, out: &mut Vec<PaneId>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split(split) => {
                if !(live && split.closing == Some(Side::First)) {
                    split.first.collect(live, out);
                }
                if !(live && split.closing == Some(Side::Second)) {
                    split.second.collect(live, out);
                }
            }
        }
    }

    /// Whether `id` is at the top, given `top` for this node; `None` when `id` isn't under it. A
    /// sibling of a closing pane at the top is at the top at once (its band eases there on the
    /// close's own glide); the closing pane keeps the place it has until it goes.
    fn at_top(&self, id: PaneId, top: bool) -> Option<bool> {
        match self {
            Node::Leaf(leaf) => (*leaf == id).then_some(top),
            Node::Split(split) => split
                .first
                .at_top(id, top)
                .or_else(|| split.second.at_top(id, top && (split.axis == SplitAxis::Right || split.closing == Some(Side::First)))),
        }
    }

    fn split(&mut self, target: PaneId, new: PaneId, axis: SplitAxis, before: bool, id: usize, now: Instant) -> bool {
        match self {
            Node::Leaf(leaf) if *leaf == target => {
                // The new pane opens from nothing to half.
                let (first, second, glide) = if before {
                    (Node::Leaf(new), Node::Leaf(target), Glide::new(0., 0.5, now))
                } else {
                    (Node::Leaf(target), Node::Leaf(new), Glide::new(1., 0.5, now))
                };
                *self = Node::Split(Box::new(Split { id, axis, ratio: 0.5, glide: Some(glide), stretch: None, closing: None, first, second, bounds: Rc::default() }));
                true
            }
            Node::Leaf(_) => false,
            Node::Split(split) => split.first.split(target, new, axis, before, id, now) || split.second.split(target, new, axis, before, id, now),
        }
    }

    fn close(&mut self, id: PaneId, now: Instant) -> bool {
        let Node::Split(split) = self else { return false };
        let direct = match (&split.first, &split.second) {
            (Node::Leaf(leaf), _) if *leaf == id => Some(Side::First),
            (_, Node::Leaf(leaf)) if *leaf == id => Some(Side::Second),
            _ => None,
        };
        let side = match direct {
            Some(side) => side,
            None => {
                if split.first.close(id, now) || split.second.close(id, now) {
                    return true;
                }
                // `id` is the one pane left in a split that is already closing its other pane (two panes closed in one go): the whole
                // split goes, `id` with it, as one child of this one.
                let lone = |node: &Node| {
                    let mut live = Vec::new();
                    node.collect(true, &mut live);
                    matches!(node, Node::Split(inner) if inner.closing.is_some()) && live[..] == [id]
                };
                if lone(&split.first) {
                    Side::First
                } else if lone(&split.second) {
                    Side::Second
                } else {
                    return false;
                }
            }
        };
        // The sibling is already going: this is the last live pane, and it stays.
        if split.closing.is_some() {
            return false;
        }
        let share = split.share(now);
        // A split still opening hands its speed to the close, so it turns around from where it is drawn.
        let speed = if split.stretch.is_some() { 0. } else { split.glide.map_or(0., |glide| glide.velocity(now)) };
        split.stretch = None;
        split.glide = Some(Glide::new(share, if side == Side::First { 0. } else { 1. }, now).with_speed(speed));
        split.closing = Some(side);
        true
    }

    fn reap(&mut self, now: Instant, removed: &mut Vec<PaneId>) {
        let Node::Split(split) = self else { return };
        split.first.reap(now, removed);
        split.second.reap(now, removed);
        if split.closing.is_none() || !split.glide.is_none_or(|glide| glide.is_done(now)) {
            return;
        }
        let Node::Split(split) = std::mem::replace(self, Node::Leaf(0)) else { return };
        let split = *split;
        let (gone, kept) = if split.closing == Some(Side::First) { (split.first, split.second) } else { (split.second, split.first) };
        gone.collect(false, removed);
        *self = kept;
    }

    fn sibling_of(&self, id: PaneId) -> Option<PaneId> {
        let Node::Split(split) = self else { return None };
        let other = match (&split.first, &split.second) {
            (Node::Leaf(leaf), other) if *leaf == id => Some(other),
            (other, Node::Leaf(leaf)) if *leaf == id => Some(other),
            _ => None,
        };
        match other {
            Some(other) => {
                let mut live = Vec::new();
                other.collect(true, &mut live);
                live.first().copied()
            }
            None => split.first.sibling_of(id).or_else(|| split.second.sibling_of(id)),
        }
    }

    fn moving(&self, now: Instant) -> bool {
        match self {
            Node::Leaf(_) => false,
            Node::Split(split) => split.glide.is_some_and(|glide| !glide.is_done(now)) || split.first.moving(now) || split.second.moving(now),
        }
    }

    fn split_mut(&mut self, id: usize) -> Option<&mut Split> {
        let Node::Split(split) = self else { return None };
        if split.id == id {
            return Some(split);
        }
        if let Some(found) = split.first.split_mut(id) {
            return Some(found);
        }
        split.second.split_mut(id)
    }

    /// The ids of the splits that stay (none closing), outermost first.
    fn split_ids(&self, out: &mut Vec<usize>) {
        let Node::Split(split) = self else { return };
        if split.closing.is_some() {
            return;
        }
        out.push(split.id);
        split.first.split_ids(out);
        split.second.split_ids(out);
    }

    fn saved(&self, leaf: &dyn Fn(PaneId) -> SavedPanes) -> SavedPanes {
        match self {
            Node::Leaf(id) => leaf(*id),
            Node::Split(split) => match split.closing {
                Some(Side::First) => split.second.saved(leaf),
                Some(Side::Second) => split.first.saved(leaf),
                None => SavedPanes::Split {
                    axis: split.axis,
                    ratio: split.ratio,
                    first: Box::new(split.first.saved(leaf)),
                    second: Box::new(split.second.saved(leaf)),
                },
            },
        }
    }

    fn build(saved: &SavedPanes, next: &mut usize, leaf: &mut dyn FnMut(&[PathBuf], usize, usize) -> Option<PaneId>) -> Option<Node> {
        match saved {
            SavedPanes::Pane { files, active, pinned } => leaf(files, *active, *pinned).map(Node::Leaf),
            SavedPanes::Split { axis, ratio, first, second } => {
                let first = Node::build(first, next, leaf);
                let second = Node::build(second, next, leaf);
                match (first, second) {
                    (Some(first), Some(second)) => {
                        let id = *next;
                        *next += 1;
                        Some(Node::Split(Box::new(Split {
                            id,
                            axis: *axis,
                            ratio: ratio.clamp(0.05, 0.95),
                            glide: None,
                            stretch: None,
                            closing: None,
                            first,
                            second,
                            bounds: Rc::default(),
                        })))
                    }
                    (first, second) => first.or(second),
                }
            }
        }
    }
}

/// The window's panes as a binary tree of splits (gpui-editor#kaf6).
pub struct Tree {
    pub root: Node,
    next_split: usize,
    /// The leaves and the live panes, listed once per change of the tree: a glide draws a frame
    /// per refresh, and none of them changes who is in the tree (finch#dgtv).
    lists: RefCell<Option<(Rc<[PaneId]>, Rc<[PaneId]>)>>,
}

impl Tree {
    pub fn new(first: PaneId) -> Self {
        Self { root: Node::Leaf(first), next_split: 0, lists: RefCell::default() }
    }

    /// The tree a saved layout describes: `leaf` makes each saved pane's view (or `None` when none
    /// of its files are left, and the pane is dropped). `None` when no pane is left.
    pub fn from_saved(saved: &SavedPanes, leaf: &mut dyn FnMut(&[PathBuf], usize, usize) -> Option<PaneId>) -> Option<Self> {
        let mut next_split = 0;
        let root = Node::build(saved, &mut next_split, leaf)?;
        Some(Self { root, next_split, lists: RefCell::default() })
    }

    /// What to write to the settings: `leaf` gives a pane's files and its showing tab's place.
    pub fn to_saved(&self, leaf: &dyn Fn(PaneId) -> SavedPanes) -> SavedPanes {
        self.root.saved(leaf)
    }

    /// Every pane in reading order (a split's first child, then its second), those closing included.
    pub fn leaves(&self) -> Rc<[PaneId]> {
        self.lists().0
    }

    /// The panes that stay, in reading order.
    pub fn live(&self) -> Rc<[PaneId]> {
        self.lists().1
    }

    /// Both lists, walked again only after `split`, `close` or `reap` changed the tree.
    fn lists(&self) -> (Rc<[PaneId]>, Rc<[PaneId]>) {
        self.lists
            .borrow_mut()
            .get_or_insert_with(|| {
                let (mut leaves, mut live) = (Vec::new(), Vec::new());
                self.root.collect(false, &mut leaves);
                self.root.collect(true, &mut live);
                (leaves.into(), live.into())
            })
            .clone()
    }

    /// The pane in the top left corner, which the traffic lights sit over.
    pub fn corner(&self) -> PaneId {
        let mut node = &self.root;
        loop {
            match node {
                Node::Leaf(id) => return *id,
                Node::Split(split) => node = &split.first,
            }
        }
    }

    /// Whether `id`'s top is the window's top: every split down above it has it first.
    pub fn at_top(&self, id: PaneId) -> bool {
        self.root.at_top(id, true).unwrap_or(true)
    }

    /// The pane in the top right corner of the editor area, whose bar runs under the window's preview button.
    pub fn top_right(&self) -> PaneId {
        let mut node = &self.root;
        loop {
            match node {
                Node::Leaf(id) => return *id,
                Node::Split(split) => node = if split.axis == SplitAxis::Right { &split.second } else { &split.first },
            }
        }
    }

    /// Splits `target`, with `new` as the second child (the first with `before`); it opens over `SPLIT`.
    pub fn split(&mut self, target: PaneId, new: PaneId, axis: SplitAxis, before: bool, now: Instant) -> bool {
        let done = self.root.split(target, new, axis, before, self.next_split, now);
        if done {
            self.next_split += 1;
            self.lists.take();
        }
        done
    }

    /// Starts closing `id`: its sibling grows over `SPLIT` and the pane goes when it lands. `false`
    /// for the only live pane.
    pub fn close(&mut self, id: PaneId, now: Instant) -> bool {
        let done = self.root.close(id, now);
        if done {
            self.lists.take();
        }
        done
    }

    /// Takes out the panes whose closing has landed and returns them (a closing split holds a leaf, so a reap that changes the tree always returns one).
    pub fn reap(&mut self, now: Instant) -> Vec<PaneId> {
        let mut removed = Vec::new();
        self.root.reap(now, &mut removed);
        if !removed.is_empty() {
            self.lists.take();
        }
        removed
    }

    /// The pane that takes `id`'s place when it closes: the first live pane of its sibling.
    pub fn sibling_of(&self, id: PaneId) -> Option<PaneId> {
        self.root.sibling_of(id)
    }

    /// A pane is opening or closing, so the window asks for frames.
    pub fn moving(&self, now: Instant) -> bool {
        self.root.moving(now)
    }

    pub fn split_mut(&mut self, id: usize) -> Option<&mut Split> {
        self.root.split_mut(id)
    }

    /// The ids of every split that stays, for what acts on all dividers at once.
    pub fn split_ids(&self) -> Vec<usize> {
        let mut ids = Vec::new();
        self.root.split_ids(&mut ids);
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::{SplitAxis, Tree, fades_out};
    use std::time::{Duration, Instant};

    /// A (preview), a code document, then B: the code document leaves the panel not following, so
    /// B's switch draws no leaving entry; B then A with the panel following still crossfades.
    #[test]
    fn a_code_tab_between_two_previews_leaves_no_fade() {
        // (following, old document has a preview) as `set` sees them.
        let old_has_preview = true;
        // set(code): no preview, so `following` goes false and the old document stays A.
        let mut following = false;
        assert!(!fades_out(following, old_has_preview));
        // set(B) then set(A): each finds the panel following, so the crossfade runs.
        following = true;
        assert!(fades_out(following, old_has_preview));
        // A document with no preview behind it has nothing to fade either.
        assert!(!fades_out(following, false));
    }

    /// Window > Merge panes shuts every pane but one in one go: with `0 | (1 / 2)`, closing 1 and then 2 at once takes the whole
    /// right split away, and the glide lands with only pane 0 left.
    #[test]
    fn two_panes_close_in_one_go() {
        let now = Instant::now();
        let mut tree = Tree::new(0);
        assert!(tree.split(0, 1, SplitAxis::Right, false, now));
        assert!(tree.split(1, 2, SplitAxis::Down, false, now));
        assert!(tree.close(1, now));
        assert_eq!(&*tree.live(), &[0, 2]);
        assert!(tree.close(2, now));
        assert_eq!(&*tree.live(), &[0]);
        let later = now + Duration::from_secs(5);
        let mut gone = tree.reap(later);
        gone.sort();
        assert_eq!(gone, vec![1, 2]);
        assert_eq!(&*tree.leaves(), &[0]);
        // The last live pane stays.
        assert!(!tree.close(0, later));
    }
}
