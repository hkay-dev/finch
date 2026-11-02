//! Mermaid diagrams and math in the preview.
//!
//! Parsing only detects: a fenced ```mermaid block, a `$$` block and a `$...$` formula become
//! custom nodes holding their source and its hash, and nothing is rendered or loaded. The
//! renderers run every frame for what is visible, so they only look things up: a `Key` (source,
//! theme, size) names the pixels in the preview's image cache (`render::diagram_slot`), and a
//! miss queues one render. Renders run one at a time on the background executor (Pele on its
//! JavaScriptCore thread, `math` on its own) and rasterize there, so the main thread only
//! receives pixels; a landed render repaints the windows.
//!
//! The box is reserved before and after the pixels land: a diagram that has been rendered
//! remembers its size (`MEMO`), and until its first render the box is `DEFAULT_HEIGHT` tall.
//! A diagram is laid out for the column's width, which a canvas in its box reports each frame;
//! the render follows the width only once it has held still for `SETTLE`, so dragging a divider
//! never renders, and the last picture stays (scaled to fit) until the new one lands.
//! A picture landing in a box that held none fades in over `FADE_QUICK` from its first drawn
//! frame (`Arrival`); a swap of an old picture for a new one doesn't.
//! A diagram's box that learns its real height (first open, an edit, a new width) and a `$$`
//! block's box grow from the height they drew to it on the `SETTLE` spring (`Rise`), clipped
//! while they do, so the text below slides rather than jumps, and frames stop when it lands.
//!
//! A block with a mistake (or a formula the math engine rejects) is its plain code block with the
//! first line of the error in the muted ink under it; an inline formula that can't draw stays the
//! literal `$...$` text. Copy and Copy as Markdown give the source either way.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::base::text::{InlineElement, InlineRenderContext, MarkdownNode, MarkdownParseContext, MarkdownPlugin, markdown_ast::Node};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use super::render::{card_fill, diagram_frame, diagram_pending, diagram_pixels, diagram_release, diagram_slot, finish_diagram_slot};
use super::{math, pele};
use crate::motion::Glide;
use crate::theme;
use crate::tokens::{ALPHA_SECONDARY, ALPHA_SURFACE, CALLOUT_DARK, CALLOUT_LIGHT, CARD_INSET, FADE_QUICK, PREVIEW_BODY, PREVIEW_CODE_EM, PREVIEW_CODE_LINE, RADIUS_CARD};

/// How many results the memo keeps (sizes and errors, a few dozen bytes each); the oldest goes first.
const MEMO_CAP: usize = 256;

/// How many diagrams' widths and last pictures the placement memo keeps.
const PLACED_CAP: usize = 256;

/// The height of a diagram's box until it has rendered once.
const DEFAULT_HEIGHT: f32 = 180.;

/// The height of a `$$` block's box until it has rendered once.
const DEFAULT_MATH_HEIGHT: f32 = 40.;

/// A `$$` block's size as a share of the body text.
const DISPLAY_MATH_EM: f32 = 1.2;

/// How long the column's width must hold still before a diagram renders for it.
const SETTLE: Duration = Duration::from_millis(150);

/// Diagrams the engine has rendered (a counter for `debug diagram-stats`).
static DIAGRAM_RENDERS: AtomicUsize = AtomicUsize::new(0);
/// Formulas the math engine has rendered.
static MATH_RENDERS: AtomicUsize = AtomicUsize::new(0);

/// What `debug diagram-stats` reports.
pub struct Stats {
    pub diagrams: usize,
    pub formulas: usize,
    pub pele_loaded: bool,
}

pub fn stats() -> Stats {
    Stats { diagrams: DIAGRAM_RENDERS.load(Ordering::Relaxed), formulas: MATH_RENDERS.load(Ordering::Relaxed), pele_loaded: pele::loaded() }
}

/// The least-recently-used memo: past `cap` entries the oldest goes, and a read makes an entry
/// the newest.
struct Memo<K, V> {
    cap: usize,
    entries: VecDeque<(K, V)>,
}

impl<K: PartialEq, V> Memo<K, V> {
    fn new(cap: usize) -> Self {
        Self { cap, entries: VecDeque::new() }
    }

    fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        let at = self.entries.iter().position(|(other, _)| other == key)?;
        let entry = self.entries.remove(at).expect("position is in range");
        self.entries.push_back(entry);
        self.entries.back_mut().map(|(_, value)| value)
    }

    fn insert(&mut self, key: K, value: V) {
        if let Some(at) = self.entries.iter().position(|(other, _)| *other == key) {
            self.entries.remove(at);
        }
        self.entries.push_back((key, value));
        while self.entries.len() > self.cap {
            self.entries.pop_front();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Diagram,
    Display,
    Inline,
}

/// What a picture is: its source, the theme it is drawn in and its size. `size` is the column
/// width of a diagram, or the font size times 100 of a formula; `ink` the formula's color.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    kind: Kind,
    source: u64,
    theme: u64,
    size: u32,
    ink: u32,
}

impl Key {
    /// The name of the pixels in the image cache.
    fn name(&self) -> (usize, u64) {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        (0xD1A6, hasher.finish())
    }
}

/// What a finished render left: the logical size of the picture (and its baseline, for an inline
/// formula), or the text to show under the block that failed (`error_text`).
#[derive(Clone)]
enum Outcome {
    Ready { width: f32, height: f32, baseline: f32 },
    Failed(SharedString),
}

/// A diagram's column as the last frames saw it.
struct Placed {
    /// The width of the box as of the last frame that ended, and when it last changed.
    observed: f32,
    changed: Instant,
    /// The widest measure in the frame `frame`, the frame still going.
    pending: f32,
    frame: u64,
    /// The width the diagram is rendered for.
    settled: f32,
    /// A timer is waiting to see the width hold still.
    timer: bool,
    /// The picture drawn last: its key and logical size.
    shown: Option<(Key, f32, f32)>,
    /// Where the node starts in the file, which names it across edits of its source.
    start: Option<usize>,
    /// Whether the box held a picture last frame, and when the one now showing first drew.
    arrival: Arrival,
    /// The height the box draws, which grows to the picture's rather than jumping.
    rise: Rise,
}

/// A diagram box's height on its way to where it belongs: a critically damped spring
/// (`SETTLE`) from the height drawn when the target last changed, so a change mid-flight turns
/// around with the speed it had and never jumps. A box that wasn't drawn for a frame (scrolled
/// away) starts its spring again from the height it last drew, so scrolling back never jumps.
#[derive(Clone, Copy)]
struct Rise {
    /// The height aimed at: what the box drew before it knew its own (`DEFAULT_HEIGHT`).
    to: f32,
    glide: Option<Glide>,
    /// The height last drawn and the preview frame that drew it.
    drawn: f32,
    frame: u64,
}

impl Rise {
    /// A box that drew `height` in `frame` (its guess, before it knew its own).
    fn holding(height: f32, frame: u64) -> Self {
        Self { to: height, glide: None, drawn: height, frame }
    }

    /// The height drawn at `now` (in preview frame `frame`) for a box that belongs at `to`, and
    /// whether it is still moving.
    fn at(&mut self, to: f32, now: Instant, frame: u64) -> (f32, bool) {
        if self.glide.is_some() && frame > self.frame + 1 {
            self.glide = Some(Glide::over(self.drawn, self.to, crate::tokens::SETTLE, now));
        }
        if (self.to - to).abs() >= 0.5 {
            let (from, speed) = self.glide.map_or((self.drawn, 0.), |glide| (glide.level(now), glide.velocity(now)));
            self.to = to;
            self.glide = Some(Glide::over(from, to, crate::tokens::SETTLE, now).with_speed(speed));
        }
        let (height, moving) = match self.glide {
            Some(glide) if !glide.is_done(now) => (glide.level(now), true),
            _ => {
                self.glide = None;
                (self.to, false)
            }
        };
        self.drawn = height;
        self.frame = frame;
        (height, moving)
    }
}

impl Placed {
    /// Takes `pending` as the width; whether it changed.
    fn commit(&mut self) -> bool {
        if (self.pending - self.observed).abs() < 0.5 {
            return false;
        }
        self.observed = self.pending;
        self.changed = Instant::now();
        true
    }
}

enum Work {
    Diagram { source: SharedString, width: f32, colors: Colors, prefix: String },
    Math { tex: SharedString, display: bool, font_size: f32, color: u32 },
}

struct Job {
    key: Key,
    work: Work,
}

thread_local! {
    static MEMO: RefCell<Memo<Key, Outcome>> = RefCell::new(Memo::new(MEMO_CAP));
    static PLACED: RefCell<Memo<(u64, u64), Placed>> = RefCell::new(Memo::new(PLACED_CAP));
    /// The height each `$$` block's box draws, by `shown_group`.
    static RISES: RefCell<Memo<(u64, Kind, u64, u32), Rise>> = RefCell::new(Memo::new(PLACED_CAP));
    /// Where each `$$` block's picture is in fading in, by `shown_group`.
    static ARRIVALS: RefCell<Memo<(u64, Kind, u64, u32), Arrival>> = RefCell::new(Memo::new(PLACED_CAP));
    static SHOWN: RefCell<Memo<(u64, Kind, u64, u32), Shown>> = RefCell::new(Memo::new(PLACED_CAP));
    static QUEUE: RefCell<VecDeque<Job>> = const { RefCell::new(VecDeque::new()) };
    static RUNNING: Cell<bool> = const { Cell::new(false) };
}

/// The theme's colors as `0xRRGGBB` over the preview's ground: what Pele's custom properties
/// become.
#[derive(Clone, Copy, Hash)]
struct Colors {
    bg: u32,
    fg: u32,
    line: u32,
    muted: u32,
    surface: u32,
    surface_alt: u32,
    border: u32,
    accent: u32,
    series: [u32; 8],
}

fn hex(color: Hsla) -> u32 {
    let color = Rgba::from(color);
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u32;
    byte(color.r) << 16 | byte(color.g) << 8 | byte(color.b)
}

impl Colors {
    fn of(cx: &App) -> Self {
        let theme = cx.theme();
        let light = theme::is_light(cx);
        let (bg, fg) = (theme.background, theme.foreground);
        let over = |color: Hsla| hex(bg.blend(color));
        let accent = over(crate::explorer::icons::accent());
        let muted = over(theme::ink(ALPHA_SECONDARY, cx));
        let callouts = if light { CALLOUT_LIGHT } else { CALLOUT_DARK };
        let mut series = [accent; 8];
        series[..6].copy_from_slice(&callouts);
        series[7] = muted;
        Self {
            bg: hex(bg),
            fg: hex(fg),
            line: muted,
            muted,
            surface: over(card_fill(fg, light)),
            surface_alt: over(fg.opacity(ALPHA_SURFACE)),
            border: over(theme.border),
            accent,
            series,
        }
    }

    fn signature(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }

    fn palette(&self) -> pele::Palette {
        pele::Palette {
            bg: self.bg,
            fg: self.fg,
            line: self.line,
            muted: self.muted,
            surface: self.surface,
            surface_alt: self.surface_alt,
            border: self.border,
            accent: self.accent,
            font: "Helvetica, sans-serif".to_string(),
            series: self.series,
        }
    }
}

/// What the preview shows under a block the engine rejected, in one string made once: the first
/// line of the message, and for a parse error ("Parse error on line N:", which names no cause)
/// also the last line, which says what was expected.
fn error_text(message: &str) -> SharedString {
    let mut lines = message.lines().map(str::trim).filter(|line| !line.is_empty());
    let Some(first) = lines.next() else { return "could not render".into() };
    match lines.last() {
        Some(last) if first.starts_with("Parse error") && first.ends_with(':') => format!("{first}\n{last}").into(),
        _ => first.to_string().into(),
    }
}

/// What one render gives back: the pixels and their logical size.
type Rendered = Result<(Arc<RenderImage>, f32, f32, f32), String>;

/// Runs on the background executor: the engine's SVG, rasterized the way GPUI draws an SVG.
fn render(work: Work, renderer: SvgRenderer) -> Rendered {
    let (svg, width, height, baseline) = match work {
        Work::Diagram { source, width, colors, prefix } => {
            DIAGRAM_RENDERS.fetch_add(1, Ordering::Relaxed);
            let diagram = pele::render(&source, width, &colors.palette(), &prefix)?;
            (diagram.svg, diagram.width, diagram.height, diagram.height)
        }
        Work::Math { tex, display, font_size, color } => {
            MATH_RENDERS.fetch_add(1, Ordering::Relaxed);
            let image = math::render(&tex, display, font_size, color)?;
            (image.svg, image.width, image.height, image.baseline)
        }
    };
    let pixels = Image::from_bytes(ImageFormat::Svg, svg.into_bytes()).to_image_data(renderer).map_err(|err| err.to_string())?;
    Ok((pixels, width, height, baseline))
}

/// Starts the next queued render unless one is running; one at a time, so a file of many
/// diagrams never holds many rasters at once. A job whose picture left the cache while it
/// waited (scrolled away, closed) is dropped. Main thread only.
fn pump(cx: &mut App) {
    if RUNNING.get() {
        return;
    }
    let job = loop {
        match QUEUE.with_borrow_mut(VecDeque::pop_front) {
            None => return,
            Some(job) if diagram_pending(job.key.name()) => break job,
            Some(_) => {}
        }
    };
    RUNNING.set(true);
    let Job { key, work } = job;
    let renderer = cx.svg_renderer();
    let task = cx.background_executor().spawn(async move { render(work, renderer) });
    cx.spawn(async move |cx| {
        let rendered = task.await;
        cx.update(|cx| {
            RUNNING.set(false);
            match rendered {
                Ok((pixels, width, height, baseline)) => {
                    MEMO.with_borrow_mut(|memo| memo.insert(key, Outcome::Ready { width, height, baseline }));
                    finish_diagram_slot(key.name(), Some(pixels), cx);
                }
                Err(message) => {
                    MEMO.with_borrow_mut(|memo| memo.insert(key, Outcome::Failed(error_text(&message))));
                    finish_diagram_slot(key.name(), None, cx);
                }
            }
            pump(cx);
            cx.refresh_windows();
        });
    })
    .detach();
}

/// What a frame draws for one picture.
enum Look {
    /// Drawn: the pixels and their logical size.
    Ready { pixels: Arc<RenderImage>, width: f32, height: f32, baseline: f32 },
    /// Rendering; the size, when an earlier render of it is remembered.
    Waiting(Option<(f32, f32)>),
    /// The engine's first line of why not.
    Failed(SharedString),
}

/// A formula's picture in the last theme (or ink) it drew in: the same source at the same size.
type Shown = (Key, f32, f32, f32);

fn shown_group(owner: u64, key: Key) -> (u64, Kind, u64, u32) {
    (owner, key.kind, key.source, key.size)
}

/// Notes that `key`'s picture is what the formula shows now.
fn remember(owner: u64, key: Key, width: f32, height: f32, baseline: f32) {
    SHOWN.with_borrow_mut(|shown| shown.insert(shown_group(owner, key), (key, width, height, baseline)));
}

/// The picture the formula showed before `key` (the theme changed) if its pixels are still held,
/// so the line keeps drawing it until the new render lands.
fn stale(owner: u64, key: Key) -> Option<(Arc<RenderImage>, f32, f32, f32)> {
    let (old, width, height, baseline) = SHOWN.with_borrow_mut(|shown| shown.get_mut(&shown_group(owner, key)).copied())?;
    if old == key {
        return None;
    }
    Some((diagram_pixels(owner, old.name())?, width, height, baseline))
}

/// The picture for `key` in `owner`'s frame; a miss queues the render `work` makes.
fn lookup(owner: u64, key: Key, work: impl FnOnce() -> Work, cx: &mut App) -> Look {
    let known = MEMO.with_borrow_mut(|memo| memo.get_mut(&key).cloned());
    if let Some(Outcome::Failed(message)) = known {
        return Look::Failed(message);
    }
    let (pixels, missed) = diagram_slot(owner, key.name());
    if missed {
        let work = work();
        QUEUE.with_borrow_mut(|queue| {
            queue.retain(|job| job.key.name() != key.name());
            queue.push_back(Job { key, work });
        });
        pump(cx);
    }
    match (pixels, known) {
        (Some(pixels), Some(Outcome::Ready { width, height, baseline })) => Look::Ready { pixels, width, height, baseline },
        // Pixels whose size the memo lost: GPUI rasterizes an SVG at twice its size.
        (Some(pixels), _) => {
            let size = pixels.size(0);
            let (width, height) = (size.width.0 as f32 / SMOOTH_SVG_SCALE_FACTOR, size.height.0 as f32 / SMOOTH_SVG_SCALE_FACTOR);
            Look::Ready { pixels, width, height, baseline: height }
        }
        (None, Some(Outcome::Ready { width, height, .. })) => Look::Waiting(Some((width, height))),
        (None, _) => Look::Waiting(None),
    }
}

fn owner(window: &Window) -> u64 {
    window.window_handle().window_id().as_u64()
}

/// A hash of `text`: what names a picture's source.
fn source_hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// What a node carries: the source between the delimiters.
struct Source {
    text: SharedString,
    hash: u64,
}

impl Source {
    fn new(text: &str) -> Self {
        Self { text: text.into(), hash: source_hash(text) }
    }
}

/// A fenced ```mermaid block.
pub struct MermaidBlock;

impl MarkdownPlugin for MermaidBlock {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "mermaid"
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Code(code) = node else { return None };
        if !code.lang.as_deref().is_some_and(|lang| lang.eq_ignore_ascii_case("mermaid")) {
            return None;
        }
        let markdown = cx.node_source(node).map_or_else(|| format!("```mermaid\n{}\n```", code.value), str::to_string);
        Some(MarkdownNode::new("mermaid", Source::new(&code.value)).text(code.value.clone()).markdown(markdown))
    }

    fn render(&self, node: &MarkdownNode, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some(source) = node.data::<Source>() else { return div().into_any_element() };
        let (owner, colors) = (owner(window), Colors::of(cx));
        adopt(owner, source.hash, node.source_range().map(|range| range.start));
        let Some(width) = settled_width(owner, source.hash) else {
            return boxed(owner, source.hash, DEFAULT_HEIGHT, None, 1., window);
        };
        let key = Key { kind: Kind::Diagram, source: source.hash, theme: colors.signature(), size: width.round() as u32, ink: 0 };
        let text = source.text.clone();
        let work = move || Work::Diagram { prefix: format!("p{:x}", key.name().1), source: text, width, colors };
        match lookup(owner, key, work, cx) {
            Look::Failed(message) => error_block(&source.text, &message, window.scale_factor(), cx).into_any_element(),
            Look::Ready { pixels, width, height, .. } => {
                let old = PLACED.with_borrow_mut(|placed| {
                    let entry = placed.get_mut(&(owner, source.hash))?;
                    entry.shown.replace((key, width, height)).map(|(old, ..)| old)
                });
                // The one it replaced (an edit, a theme switch, a drag) has done its job: free its pixels now.
                if let Some(old) = old.filter(|old| *old != key) {
                    diagram_release(owner, old.name(), cx);
                }
                let fade = diagram_fade(owner, source.hash, true, window);
                boxed(owner, source.hash, height, Some((pixels, width, height)), fade, window)
            }
            Look::Waiting(size) => {
                // The last picture stays, as it was, until the one for the new width or source lands.
                let shown = PLACED.with_borrow_mut(|placed| placed.get_mut(&(owner, source.hash)).and_then(|entry| entry.shown));
                match shown {
                    Some((old, width, height)) => {
                        let pixels = (old != key).then(|| diagram_pixels(owner, old.name())).flatten();
                        let fade = diagram_fade(owner, source.hash, pixels.is_some(), window);
                        boxed(owner, source.hash, size.map_or(height, |size| size.1), pixels.map(|pixels| (pixels, width, height)), fade, window)
                    }
                    None => {
                        diagram_fade(owner, source.hash, false, window);
                        boxed(owner, source.hash, size.map_or(DEFAULT_HEIGHT, |size| size.1), None, 1., window)
                    }
                }
            }
        }
    }
}

/// A `$$ ... $$` block.
pub struct MathBlock;

impl MarkdownPlugin for MathBlock {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "math-block"
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Math(math) = node else { return None };
        let markdown = cx.node_source(node).map_or_else(|| format!("$$\n{}\n$$", math.value), str::to_string);
        Some(MarkdownNode::new("math-block", Source::new(&math.value)).text(math.value.clone()).markdown(markdown))
    }

    fn render(&self, node: &MarkdownNode, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some(source) = node.data::<Source>() else { return div().into_any_element() };
        let owner = owner(window);
        let start = node.source_range().map(|range| range.start);
        let font_size = PREVIEW_BODY * DISPLAY_MATH_EM;
        let color = hex(cx.theme().foreground);
        let key = Key { kind: Kind::Display, source: source.hash, theme: 0, size: (font_size * 100.).round() as u32, ink: color };
        let tex = source.text.clone();
        let work = move || Work::Math { tex, display: true, font_size, color };
        match lookup(owner, key, work, cx) {
            Look::Failed(message) => error_block(&source.text, &message, window.scale_factor(), cx).into_any_element(),
            Look::Ready { pixels, width, height, baseline } => {
                remember(owner, key, width, height, baseline);
                let fade = math_fade(owner, key, true, window);
                math_box(owner, key, height, fade, start, Some((width, height)), window).child(picture(pixels, width, height)).into_any_element()
            }
            Look::Waiting(size) => match stale(owner, key) {
                Some((pixels, width, height, _)) => {
                    let fade = math_fade(owner, key, true, window);
                    math_box(owner, key, height, fade, start, Some((width, height)), window).child(picture(pixels, width, height)).into_any_element()
                }
                None => {
                    math_fade(owner, key, false, window);
                    math_box(owner, key, size.map_or(DEFAULT_MATH_HEIGHT, |size| size.1), 1., start, None, window).into_any_element()
                }
            },
        }
    }
}

/// A `$...$` formula in a line of text. Prose that only has dollar signs in it
/// (`spent $5 and $10`) is left as it is: a formula doesn't start or end with a space, and its
/// closing `$` isn't followed by a digit.
pub struct InlineMath;

impl MarkdownPlugin for InlineMath {
    fn name(&self) -> &str {
        "math"
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::InlineMath(math) = node else { return None };
        let tex = math.value.as_str();
        // The source decides, not the value: markdown-rs strips one space of padding from `$ x $`.
        let inner = cx.node_source(node)?.trim_matches('$');
        if tex.is_empty() || inner.starts_with(char::is_whitespace) || inner.ends_with(char::is_whitespace) {
            return None;
        }
        let end = node.position()?.end.offset;
        if cx.source().get(end..).and_then(|rest| rest.chars().next()).is_some_and(|next| next.is_ascii_digit()) {
            return None;
        }
        Some(MarkdownNode::new("math", Source::new(tex)))
    }

    fn render_inline(&self, node: &MarkdownNode, context: &InlineRenderContext, window: &mut Window, cx: &mut App) -> Option<InlineElement> {
        let source = node.data::<Source>()?;
        let owner = owner(window);
        let font_size = f32::from(context.font_size());
        let color = hex(cx.theme().background.blend(context.text_style().color));
        let key = Key { kind: Kind::Inline, source: source.hash, theme: 0, size: (font_size * 100.).round() as u32, ink: color };
        let tex = source.text.clone();
        let work = move || Work::Math { tex, display: false, font_size, color };
        // Until it has drawn, and if the engine can't, the line shows the literal `$...$`.
        match lookup(owner, key, work, cx) {
            Look::Ready { pixels, width, height, baseline } => {
                remember(owner, key, width, height, baseline);
                Some(InlineElement::new(img(ImageSource::Render(pixels)).w(px(width)).h(px(height))).with_baseline(px(baseline)))
            }
            Look::Waiting(_) => {
                let (pixels, width, height, baseline) = stale(owner, key)?;
                Some(InlineElement::new(img(ImageSource::Render(pixels)).w(px(width)).h(px(height))).with_baseline(px(baseline)))
            }
            Look::Failed(_) => None,
        }
    }
}

/// A picture drawn at its logical size, shrunk to fit a narrower box.
fn picture(pixels: Arc<RenderImage>, width: f32, height: f32) -> impl IntoElement {
    img(ImageSource::Render(pixels)).w(px(width)).max_w_full().h(px(height))
}

thread_local! {
    /// Where each kind of box has been drawn in a window (`ordinal`): start in the file, source hash and the preview frame that last drew it. Only filled while rects are on.
    static ORDER: RefCell<HashMap<(u64, &'static str), Vec<(usize, u64, u64)>>> = RefCell::new(HashMap::new());
    /// The rect ids a kind of box recorded in the frame it last drew, to forget them when the next frame draws other boxes.
    static TRACKED: RefCell<HashMap<(u64, &'static str), (u64, Vec<String>)>> = RefCell::new(HashMap::new());
}

/// The box at file byte `start` as a count from 1, in document order, among the `prefix` boxes this window has drawn.
/// A box whose source was deleted, or which an edit moved, is dropped when a later frame draws blocks on both sides of
/// it (or a block with its source elsewhere), since the list draws a run of blocks and a scrolled-away one is never between two drawn ones.
fn ordinal(owner: u64, prefix: &'static str, start: usize, hash: u64, frame: u64) -> usize {
    ORDER.with_borrow_mut(|all| {
        let list = all.entry((owner, prefix)).or_default();
        list.retain(|&(other, ..)| other != start);
        list.push((start, hash, frame));
        let drawn = list.iter().filter(|entry| entry.2 == frame).map(|entry| entry.0);
        let (first, last) = (drawn.clone().min().unwrap_or(start), drawn.max().unwrap_or(start));
        list.retain(|&(other, same, seen)| seen == frame || !(same == hash || (first < other && other < last)));
        list.sort_unstable();
        list.iter().position(|entry| entry.0 == start).map_or(1, |at| at + 1)
    })
}

/// Records the box it sits in as `<prefix>:<n>` for `debug rects` (`n` from 1, `ordinal`), and the picture centered at the top of it as
/// `<prefix>-picture:<n>` when there is one (`pic` is its logical size, shrunk to the box's width as `picture` does). The ids of the
/// last frame's boxes are forgotten when a new frame's first box records. `None` unless rects are on.
fn track_box(owner: u64, prefix: &'static str, hash: u64, start: usize, frame: u64, pic: Option<(f32, f32)>) -> Option<impl IntoElement> {
    if !crate::control::rects_enabled() {
        return None;
    }
    let n = ordinal(owner, prefix, start, hash, frame);
    Some(
        canvas(
            move |bounds, window, _| {
                let window_id = window.window_handle().window_id();
                TRACKED.with_borrow_mut(|all| {
                    let entry = all.entry((owner, prefix)).or_default();
                    if entry.0 != frame {
                        for old in entry.1.drain(..) {
                            crate::control::unrecord(window_id, &old);
                        }
                        entry.0 = frame;
                    }
                    let id = format!("{prefix}:{n}");
                    crate::control::record(window, id.clone(), bounds);
                    entry.1.push(id);
                    if let Some((width, height)) = pic {
                        let width = width.min(f32::from(bounds.size.width));
                        let left = bounds.origin.x + (bounds.size.width - px(width)) / 2.;
                        let id = format!("{prefix}-picture:{n}");
                        crate::control::record(window, id.clone(), Bounds::new(point(left, bounds.origin.y), size(px(width), px(height))));
                        entry.1.push(id);
                    }
                });
            },
            |_, (), _, _| {},
        )
        .absolute()
        .size_full(),
    )
}

/// `div` at `fade` of its opacity; a picture at rest (1) isn't wrapped in a layer.
fn faded(div: Div, fade: f32) -> Div {
    if fade < 1. { div.opacity(fade) } else { div }
}

/// Where a picture is in fading in, from the first frame that drew it. `drawn` is whether the
/// box held a picture in its last render: an edit, a theme or width swap keeps the old picture
/// drawn, so only a box that was empty (the first render, or after the preview was closed and
/// its pixels freed) fades its picture in.
#[derive(Clone, Copy)]
struct Arrival {
    drawn: bool,
    at: Instant,
}

impl Arrival {
    fn new() -> Self {
        Self { drawn: false, at: Instant::now() }
    }

    /// The opacity of this frame's picture at `now` (the frame's drawn time), stamping the first
    /// frame that has one. 1 when there is none to fade or the fade has landed.
    fn show(&mut self, drawn: bool, now: Instant) -> f32 {
        if !drawn {
            self.drawn = false;
            return 1.;
        }
        if !std::mem::replace(&mut self.drawn, true) {
            self.at = now;
        }
        let u = now.saturating_duration_since(self.at).as_secs_f32() / FADE_QUICK;
        if u >= 1. { 1. } else { crate::motion::ease_out(u) }
    }
}

/// A diagram's fade this frame (see `Arrival::show`); asks for the next frame while it runs.
fn diagram_fade(owner: u64, hash: u64, drawn: bool, window: &mut Window) -> f32 {
    let now = crate::motion::drawn_now(window.window_handle().window_id());
    let fade = PLACED.with_borrow_mut(|placed| placed.get_mut(&(owner, hash)).map_or(1., |entry| entry.arrival.show(drawn, now)));
    if fade < 1. {
        window.request_animation_frame();
    }
    fade
}

/// A `$$` block's fade this frame, kept per formula and size (`shown_group`).
fn math_fade(owner: u64, key: Key, drawn: bool, window: &mut Window) -> f32 {
    let now = crate::motion::drawn_now(window.window_handle().window_id());
    let group = shown_group(owner, key);
    let fade = ARRIVALS.with_borrow_mut(|arrivals| {
        if arrivals.get_mut(&group).is_none() {
            arrivals.insert(group, Arrival::new());
        }
        arrivals.get_mut(&group).map_or(1., |arrival| arrival.show(drawn, now))
    });
    if fade < 1. {
        window.request_animation_frame();
    }
    fade
}

/// A diagram's box: as wide as the column and `height` tall once it has grown there (see `Rise`),
/// with a canvas that reports the column's width, and the picture centered when there is one, at
/// `fade` of its opacity. The picture is laid out at its final size; while the box is still
/// growing to it the box clips, so the picture is uncovered rather than squeezed.
fn boxed(owner: u64, hash: u64, height: f32, shown: Option<(Arc<RenderImage>, f32, f32)>, fade: f32, window: &mut Window) -> AnyElement {
    let (height, growing) = rise(owner, hash, height, window);
    let measure = canvas(move |bounds, _, cx| observe(owner, hash, f32::from(bounds.size.width), cx), |_, _, _, _| {}).absolute().size_full();
    let rect = crate::control::rects_enabled()
        .then(|| PLACED.with_borrow_mut(|placed| placed.get_mut(&(owner, hash)).and_then(|entry| entry.start)))
        .flatten()
        .and_then(|start| track_box(owner, "diagram", hash, start, diagram_frame(owner), shown.as_ref().map(|(_, width, height)| (*width, *height))));
    let body = shown.map(|(pixels, width, height)| faded(div().absolute().size_full().flex().justify_center(), fade).child(picture(pixels, width, height)));
    let frame = div().relative().w_full().h(px(height)).child(measure).children(rect).children(body);
    if growing { frame.overflow_hidden() } else { frame }.into_any_element()
}

/// A `$$` block's box at `fade` of its opacity: it grows to `height` on the same spring as a
/// diagram's (kept per formula and size, `shown_group`), clipped while it does. Its first
/// height is taken as it is.
fn math_box(owner: u64, key: Key, height: f32, fade: f32, start: Option<usize>, pic: Option<(f32, f32)>, window: &mut Window) -> Div {
    let now = crate::motion::drawn_now(window.window_handle().window_id());
    let (frame, group) = (diagram_frame(owner), shown_group(owner, key));
    let (drawn, moving) = RISES.with_borrow_mut(|rises| {
        if rises.get_mut(&group).is_none() {
            rises.insert(group, Rise::holding(height, frame));
        }
        rises.get_mut(&group).map_or((height, false), |rise| rise.at(height, now, frame))
    });
    if moving {
        window.request_animation_frame();
    }
    let rect = start.and_then(|start| track_box(owner, "formula", key.source, start, frame, pic));
    let frame = faded(div().w_full().h(px(drawn)).flex().justify_center(), fade).children(rect);
    if moving { frame.overflow_hidden() } else { frame }
}

/// The height a diagram's box draws this frame, heading for `to` on the `SETTLE` spring from
/// where it is drawn (a change mid-flight turns around with the speed it had), and whether it is
/// still moving; asks for the next frame while it is. A box with no placement yet (its first
/// frame, which draws the guess) is drawn at `to`.
fn rise(owner: u64, hash: u64, to: f32, window: &mut Window) -> (f32, bool) {
    let now = crate::motion::drawn_now(window.window_handle().window_id());
    let frame = diagram_frame(owner);
    let (height, moving) = PLACED.with_borrow_mut(|placed| placed.get_mut(&(owner, hash)).map_or((to, false), |entry| entry.rise.at(to, now, frame)));
    if moving {
        window.request_animation_frame();
    }
    (height, moving)
}

/// The width a diagram renders for: the column's, once it has held still for `SETTLE`;
/// `None` before the first frame has measured it.
fn settled_width(owner: u64, hash: u64) -> Option<f32> {
    PLACED.with_borrow_mut(|placed| {
        placed.get_mut(&(owner, hash)).map(|entry| {
            if (entry.observed - entry.settled).abs() >= 0.5 && entry.changed.elapsed() >= SETTLE {
                entry.settled = entry.observed;
            }
            entry.settled
        })
    })
}

/// A diagram with a new hash (its source was edited) starts from the placement of the one that
/// was at the same place in this window, so its box and picture stay as they were until the new
/// picture lands; `start` is the node's place in the file.
fn adopt(owner: u64, hash: u64, start: Option<usize>) {
    PLACED.with_borrow_mut(|placed| {
        if let Some(entry) = placed.get_mut(&(owner, hash)) {
            entry.start = start;
            return;
        }
        let Some(start) = start else { return };
        let found = placed.entries.iter().rev().find(|((other, _), entry)| *other == owner && entry.start == Some(start)).map(|(_, entry)| (entry.observed, entry.settled, entry.shown, entry.arrival, entry.rise));
        if let Some((observed, settled, shown, arrival, rise)) = found {
            // The box carries its picture (and whether it was drawn) across the edit, so the new picture doesn't fade in.
            placed.insert((owner, hash), Placed { observed, pending: observed, frame: 0, changed: Instant::now(), settled, timer: false, shown, start: Some(start), arrival, rise });
        }
    });
}

/// A frame's measure of a diagram's column. The first one lets the diagram render (the next
/// frame). A source drawn twice in one frame (at the top level and in a list) is measured twice:
/// the frame's width is the larger, and it is compared with the last frame's only when the next
/// frame starts, so the two never flip each other. A change starts a timer that repaints once the
/// width has held still.
fn observe(owner: u64, hash: u64, width: f32, cx: &mut App) {
    if width < 1. {
        return;
    }
    let frame = diagram_frame(owner);
    let arm = PLACED.with_borrow_mut(|placed| match placed.get_mut(&(owner, hash)) {
        None => {
            placed.insert((owner, hash), Placed { observed: width, pending: width, frame, changed: Instant::now(), settled: width, timer: false, shown: None, start: None, arrival: Arrival::new(), rise: Rise::holding(DEFAULT_HEIGHT, frame) });
            cx.refresh_windows();
            false
        }
        Some(entry) if entry.frame == frame => {
            entry.pending = entry.pending.max(width);
            false
        }
        Some(entry) => {
            let changed = entry.commit();
            entry.frame = frame;
            entry.pending = width;
            changed && !std::mem::replace(&mut entry.timer, true)
        }
    });
    if arm {
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(SETTLE).await;
                let held = cx.update(|cx| {
                    let held = PLACED.with_borrow_mut(|placed| match placed.get_mut(&(owner, hash)) {
                        Some(entry) => {
                            // The last frame's width counts even if no frame follows it.
                            entry.commit();
                            if entry.changed.elapsed() < SETTLE {
                                false
                            } else {
                                entry.timer = false;
                                true
                            }
                        }
                        None => true,
                    });
                    if held {
                        cx.refresh_windows();
                    }
                    held
                });
                if held {
                    break;
                }
            }
        })
        .detach();
    }
}

/// The plain code block of a source the engine rejected, with the engine's message (`error_text`)
/// under it in the muted ink: the code block's card, its inset and hairline edge (`scale_factor` is the window's, for the hairline) and its font.
fn error_block(source: &SharedString, message: &SharedString, scale_factor: f32, cx: &App) -> Div {
    let theme = cx.theme();
    let light = theme::is_light(cx);
    let settings = crate::settings::get(cx);
    let code = px(PREVIEW_BODY * PREVIEW_CODE_EM);
    let mut block = div().w_full().p(CARD_INSET).border(px(1. / scale_factor)).border_color(theme.border).rounded(RADIUS_CARD).bg(card_fill(theme.foreground, light)).flex().flex_col().gap(px(6.));
    let family = settings.editor.drawn_family(cx).unwrap_or_else(|| theme.mono_font_family.clone());
    let text = div().text_size(code).line_height(relative(PREVIEW_CODE_LINE)).font_weight(FontWeight(settings.text.weight)).font_family(family).text_color(theme.foreground);
    block = block.child(text.child(source.clone()));
    block.child(div().text_size(code).text_color(theme::ink(ALPHA_SECONDARY, cx)).child(message.clone()))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{Arrival, Memo, error_text};

    #[test]
    fn a_picture_fades_in_only_into_a_box_that_held_none() {
        let t0 = Instant::now();
        let mut arrival = Arrival::new();
        assert_eq!(arrival.show(false, t0), 1.);
        assert!(arrival.show(true, t0) < 0.001);
        let mid = arrival.show(true, t0 + Duration::from_millis(75));
        assert!(mid > 0. && mid < 1.);
        assert_eq!(arrival.show(true, t0 + Duration::from_millis(200)), 1.);
        // A swap keeps a picture drawn: no new fade.
        assert_eq!(arrival.show(true, t0 + Duration::from_millis(300)), 1.);
        // A box that drew nothing (its pixels freed) fades the next picture in again.
        assert_eq!(arrival.show(false, t0 + Duration::from_millis(400)), 1.);
        assert!(arrival.show(true, t0 + Duration::from_millis(500)) < 0.001);
    }

    #[test]
    fn memo_keeps_to_its_cap_and_keeps_the_newest() {
        let mut memo = Memo::new(3);
        for n in 0..10 {
            memo.insert(n, n * 10);
            assert!(memo.entries.len() <= 3);
        }
        assert_eq!(memo.entries.iter().map(|(key, _)| *key).collect::<Vec<_>>(), [7, 8, 9]);
        // A read makes an entry the newest, so it outlives older ones.
        assert_eq!(memo.get_mut(&7).copied(), Some(70));
        memo.insert(10, 100);
        assert!(memo.get_mut(&7).is_some());
        assert!(memo.get_mut(&8).is_none());
        // Inserting a key again replaces it without growing the memo.
        memo.insert(10, 101);
        assert_eq!(memo.entries.len(), 3);
        assert_eq!(memo.get_mut(&10).copied(), Some(101));
    }

    #[test]
    fn error_message_shows_its_first_real_line() {
        assert_eq!(&*error_text("\n  Unknown diagram\n  ^^^\n"), "Unknown diagram");
        assert_eq!(&*error_text(""), "could not render");
    }

    #[test]
    fn parse_errors_add_the_line_that_says_what_was_expected() {
        let message = "Parse error on line 2:\n  A --> B[unclosed\n------------------^\nExpecting 'SQE', got '$end'";
        assert_eq!(&*error_text(message), "Parse error on line 2:\nExpecting 'SQE', got '$end'");
        assert_eq!(&*error_text("Parse error on line 2:"), "Parse error on line 2:");
    }
}
