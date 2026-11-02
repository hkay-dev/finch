//! How the preview draws a markdown file's parsed state.
//!
//! A link to a local markdown file is sent up as `OpenFile`, which the
//! workspace handles by opening it in the editor.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use data_url::DataUrl;
use gpui_kit::base::text::{CodeBlock, MarkdownExtensions, TextView, TextViewState, TextViewStyle, forget_local_image, reserve_local_image, retain_shown_images};
use crate::hover::{FadeHover as _, Ink};
use crate::motion::Fade;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use crate::native_menu::{self, COMMAND, Entry, Item};
use crate::{theme, tooltip};
use crate::tokens::{
    ALPHA_BUTTON_HOVER, ALPHA_BUTTON_PRESSED, FADE_QUICK, ALPHA_CARD, ALPHA_PRIMARY, ALPHA_SECONDARY, ALPHA_QUOTE_BAR, ALPHA_SURFACE, ALPHA_TEXT_SELECTION,
    ALPHA_MARK_DARK, ALPHA_TEXT_SELECTION_LIGHT, CALLOUT_DARK, CALLOUT_LIGHT, LINK_BLUE, LINK_BLUE_LIGHT, MARK_AMBER_DARK, PREVIEW_BODY, PREVIEW_CODE_EM, PREVIEW_INLINE_CODE_WEIGHT, SELECTION_BLUE_LIGHT,
    PREVIEW_CODE_LINE, PREVIEW_HEADINGS, PREVIEW_LINE, PREVIEW_MEASURE, PREVIEW_READING_INSET, PREVIEW_READING_INSET_MIN, PREVIEW_READING_INSET_RIGHT_MIN, PREVIEW_READING_SHARE, QUOTE_BAR, RADIUS_CARD,
    ALPHA_CARD_LIGHT, RADIUS_BUTTON, RADIUS_TINT, ALPHA_TERTIARY, ICON_BUTTON, tabular, LINK_PILL_INSET, LINK_PILL_MAX_CHARS, RADIUS_TOOLTIP, TEXT_SUBHEADLINE,
    TEXT_SUBHEADLINE_LINE, TOOLTIP_DELAY, TOOLTIP_PAD_X, TOOLTIP_PAD_Y, COPY_SYMBOL, COPIED_BADGE_INSET_RIGHT, ALPHA_CALLOUT_BAR, ALPHA_CALLOUT_DARK, ALPHA_CALLOUT_LIGHT,
    ALPHA_LINK_UNDERLINE, CALLOUT_BAR, LINK_UNDERLINE, RADIUS_CALLOUT,
};

/// Opens a file in the editor of the window that sent it, then jumps its preview to the heading
/// `anchor` (a lowercased `#fragment`) when there is one, or puts the caret on `caret` and washes
/// its line (a compiler error's location).
#[derive(Action, Clone, PartialEq)]
#[action(namespace = gpui_editor, no_json)]
pub struct OpenFile {
    pub path: PathBuf,
    pub anchor: Option<String>,
    pub caret: Option<crate::start::Caret>,
}

/// Flips the task box of the list item that spans bytes `item_start..item_end` of the open file's
/// text when the preview `preview` last parsed it, as an edit of the file (`task::toggle_task`).
/// `preview` is the id of the preview's state, so a click in a preview that has since gone to
/// another file (or is fading out) is ignored.
#[derive(Action, Clone, PartialEq)]
#[action(namespace = gpui_editor, no_json)]
pub struct ToggleTask {
    pub item_start: usize,
    pub item_end: usize,
    /// The byte length of the whole text the preview parsed; a click is stale when the file's differs.
    pub parsed_len: usize,
    pub preview: u64,
}

/// What a clicked link means.
#[derive(Debug, PartialEq)]
pub(crate) enum LinkTarget {
    /// `http`, `https` or `mailto`: the system's browser or mail app.
    Web(String),
    /// An existing `.md` or `.markdown` file: the editor, then its heading `anchor` if any.
    Markdown(PathBuf, Option<String>),
    /// A `#heading` link: the preview scrolls to that heading of this file.
    Heading(String),
    /// Any other existing local file: the system's default app.
    Other(PathBuf),
    /// Nothing to do: a missing file, an unknown scheme, a lone `#`.
    Nothing,
}

/// How many decoded `data:` images the preview keeps before it looks for ones to drop.
const EMBEDDED_CAP: usize = 32;

/// How many bytes of decoded pixels (width x height x 4) the cache keeps before it looks for
/// ones to drop.
const EMBEDDED_BYTES_CAP: usize = 64 * 1024 * 1024;

/// What one `data:` URL is right now. The URL itself is not kept (it is the whole encoded image).
#[derive(Clone)]
enum Slot {
    /// Waiting for, or in, the one decode at a time; a transparent pixel stands in.
    Pending,
    /// Decoded pixels, already in the BGRA order GPUI uploads. The encoded bytes are gone.
    Pixels(Arc<RenderImage>),
    /// A GIF or SVG, which GPUI's loader decodes itself (frames, rasterizing). An animated WebP
    /// is decoded into frames here and arrives as `Pixels`.
    Encoded(Arc<Image>),
    /// Not an image anything here can read.
    Failed,
}

impl Slot {
    /// What this costs once decoded: width x height x 4, for every frame of a GIF or animated WebP.
    fn bytes(&self) -> usize {
        match self {
            Slot::Pixels(image) => {
                let size = image.size(0);
                (size.width.0.max(0) as usize).saturating_mul(size.height.0.max(0) as usize).saturating_mul(4).saturating_mul(image.frame_count().max(1))
            }
            Slot::Encoded(image) => decoded_bytes(image),
            Slot::Pending | Slot::Failed => 0,
        }
    }

    /// The id GPUI's picture fade-in remembers this image by; none until it is decoded.
    fn shown_id(&self) -> Option<u64> {
        match self {
            Slot::Pixels(image) => Some(image.id.0 as u64),
            Slot::Encoded(image) => Some(image.id()),
            Slot::Pending | Slot::Failed => None,
        }
    }
}

/// Where a decode job's encoded bytes are.
enum Source {
    Bytes { format: image::ImageFormat, bytes: Vec<u8> },
    /// A local file, read on the background thread; its format is read from its bytes.
    File(Arc<Path>),
}

/// One image waiting its turn to decode: its cache key and where its bytes are.
struct Job {
    key: (usize, u64),
    source: Source,
}

/// What a miss makes: the slot to cache and, when pixels are still to decode, the job.
struct Made {
    slot: Slot,
    job: Option<Job>,
}

/// Who drew an image or a local picture's header, by owner (a window) and the frame number of
/// that owner's last draw of it. A release by one window frees only what no other window draws.
#[derive(Default)]
struct Claims(Vec<(u64, u64)>);

impl Claims {
    fn one(owner: u64, frame: u64) -> Self {
        Self(vec![(owner, frame)])
    }

    /// `owner` drew this in its frame `frame`.
    fn touch(&mut self, owner: u64, frame: u64) {
        match self.0.iter_mut().find(|(other, _)| *other == owner) {
            Some(claim) => claim.1 = frame,
            None => self.0.push((owner, frame)),
        }
    }

    /// Some open owner drew this in its latest frame or the one before.
    fn held(&self, frames: &HashMap<u64, u64>) -> bool {
        self.0.iter().any(|&(owner, frame)| frames.get(&owner).is_some_and(|&now| frame + 1 >= now))
    }

    /// Drops `owner`'s claim: all of it, or only when it didn't draw this in its latest frame.
    fn release(&mut self, owner: u64, unseen_only: bool, frames: &HashMap<u64, u64>) {
        self.0.retain(|&(other, frame)| other != owner || (unseen_only && frames.get(&owner).is_some_and(|&now| frame >= now)));
    }
}

/// One cached image: who drew it and what it costs decoded. A local file keeps
/// its `path`, so a newer version of it retires this one.
struct Embedded {
    key: (usize, u64),
    slot: Slot,
    claims: Claims,
    bytes: usize,
    path: Option<Arc<Path>>,
}

impl Embedded {
    fn retire(self) -> Slot {
        self.slot
    }
}

const MUL: u64 = 0x9E37_79B9_7F4A_7C15;

/// How long a local picture's file stamp is trusted before the next frame asks again.
const RECHECK: Duration = Duration::from_secs(1);

/// How long a picture URL new beside one that just left (a link being typed) waits before its
/// first stat, so a path typed a character at a time stats once, when the typing pauses.
const TYPING_PAUSE: Duration = Duration::from_millis(300);

/// A local picture URL that left within this long of appearing was a partial one, being typed.
const PARTIAL_LIFE: Duration = Duration::from_secs(3);

/// What one local picture URL (in one folder) names: its file, the file's stamp as last seen
/// (`None`: missing or not a file), and when that was. Frames read this and never touch the disk.
struct LocalFile {
    path: Arc<Path>,
    /// The hash of the path, the base of every version's key.
    base: (usize, u64),
    /// `None` until the first background stat lands.
    stamp: Option<Option<u64>>,
    checked: Instant,
    born: Instant,
    /// A background stat is running.
    checking: bool,
    claims: Claims,
}

/// The stamp of the file at `path`: its mtime and length mixed; `None` unless it is a file.
fn file_stamp(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let nanos = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos() as u64);
    Some(nanos.wrapping_mul(MUL).wrapping_add(meta.len()))
}

/// A fast 64-bit hash of `url`, 8 bytes a step: it runs for every embedded image every frame, so
/// it has to cost about what comparing the strings did.
fn url_key(url: &str) -> (usize, u64) {
    const MUL: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut hash = 0xCBF2_9CE4_8422_2325u64;
    let mut chunks = url.as_bytes().chunks_exact(8);
    for chunk in &mut chunks {
        let word = u64::from_le_bytes(chunk.try_into().expect("chunk is 8 bytes"));
        hash = (hash ^ word).wrapping_mul(MUL);
        hash ^= hash >> 29;
    }
    for &byte in chunks.remainder() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(MUL);
    }
    (url.len(), hash ^ (hash >> 32))
}

/// Decoded `data:` images by a hash of their URL, least recently used first: `text_view` runs every frame, so
/// each URL has to decode once, not once per frame. Pixels decode one image at a time (`queue`,
/// `decoding`), so several large images never hold their decode buffers together. Past
/// `EMBEDDED_CAP` entries or `EMBEDDED_BYTES_CAP` bytes the unclaimed entry with the most bytes
/// for its distance from the view goes (`far_pick`); an entry
/// asked for in the current or previous frame is on screen and claimed, because dropping it would
/// decode it again next frame and flicker. When everything is claimed the cache runs over its
/// caps until images leave the screen. Evicted pixels are freed by the next render, which has the
/// `App` that needs.
#[derive(Default)]
struct EmbeddedImages {
    entries: VecDeque<Embedded>,
    evicted: Vec<Slot>,
    queue: VecDeque<Job>,
    /// A decode is running on the background executor.
    decoding: bool,
    /// Each owner's (a window's) frame number; `PreviewText::render` starts each frame.
    frames: HashMap<u64, u64>,
    bytes: usize,
    /// Local picture URLs by a hash of the URL and its folder.
    files: HashMap<(usize, u64), LocalFile>,
    /// When each window's latest counted frame was drawn (`first_in_frame`).
    drawn: HashMap<u64, Instant>,
    /// When a short-lived local picture URL last left (`prune_files`): a link is being typed.
    typing_at: Option<Instant>,
}

impl EmbeddedImages {
    /// Whether `owner`'s frame drawn at `at` hasn't been counted: a document switch draws two
    /// previews in one window frame, and the frame count (which the missed-frame checks read)
    /// goes up once.
    fn first_in_frame(&mut self, owner: u64, at: Instant) -> bool {
        self.drawn.insert(owner, at) != Some(at)
    }

    fn begin_frame(&mut self, owner: u64) {
        *self.frames.entry(owner).or_insert(0) += 1;
        self.prune_files();
    }

    /// Forgets the local picture URLs no window drew in the latest frame or the one before and
    /// that hold no decoded picture: the text no longer has them (a link edited or deleted). A
    /// file whose picture is still cached keeps its entry, so scrolling back never waits for a
    /// stat.
    fn prune_files(&mut self) {
        let frames = &self.frames;
        if self.files.values().all(|file| file.claims.held(frames)) {
            return;
        }
        let entries = &self.entries;
        let mut partial = false;
        self.files.retain(|_, file| {
            let keep = file.claims.held(frames) || entries.iter().any(|entry| entry.path.as_deref().map(Path::as_os_str) == Some(file.path.as_os_str()));
            if !keep {
                partial |= file.born.elapsed() < PARTIAL_LIFE;
                forget_local_image(&file.path);
            }
            keep
        });
        if partial {
            self.typing_at = Some(Instant::now());
        }
    }

    /// Remembers the local picture URL `fkey`, named `path`, seen first by `owner` now. `true`:
    /// stat it at once. `false`: a link is being typed (`typing_at`), so the stat waits for
    /// `settle_file`.
    fn add_file(&mut self, owner: u64, fkey: (usize, u64), path: Arc<Path>, base: (usize, u64)) -> bool {
        let frame = self.frames.get(&owner).copied().unwrap_or(0);
        let typing = self.typing_at.is_some_and(|at| at.elapsed() < TYPING_PAUSE);
        let now = Instant::now();
        self.files.insert(fkey, LocalFile { path, base, stamp: None, checked: now, born: now, checking: !typing, claims: Claims::one(owner, frame) });
        !typing
    }

    /// The path of the URL `fkey` to stat now that the typing paused, if the text still has it
    /// and nothing has stat it yet.
    fn settle_file(&mut self, fkey: (usize, u64)) -> Option<Arc<Path>> {
        let file = self.files.get_mut(&fkey)?;
        if file.checking || file.stamp.is_some() || !file.claims.held(&self.frames) {
            return None;
        }
        file.checking = true;
        Some(file.path.clone())
    }

    /// The slot for `url`, made by `make` only when it is not cached; a hit becomes the most
    /// recently used and copies nothing.
    fn get_or_insert_with(&mut self, owner: u64, url: &str, make: impl FnOnce((usize, u64)) -> Made) -> Slot {
        self.get_or_insert_key(owner, url_key(url), make)
    }

    /// `get_or_insert_with` for a caller that built the `key` itself (a local file's path and
    /// stamp). A new version of a file leaves the older ones in place, still shown, until its
    /// own decode lands (`finish`); versions still waiting in the queue are dropped.
    fn get_or_insert_key(&mut self, owner: u64, key: (usize, u64), make: impl FnOnce((usize, u64)) -> Made) -> Slot {
        let frame = self.frames.get(&owner).copied().unwrap_or(0);
        if let Some(at) = self.entries.iter().position(|entry| entry.key == key) {
            let mut entry = self.entries.remove(at).expect("position is in range");
            entry.claims.touch(owner, frame);
            self.entries.push_back(entry);
        } else {
            let Made { slot, job } = make(key);
            let path = match &job {
                Some(Job { source: Source::File(path), .. }) => Some(path.clone()),
                _ => None,
            };
            if let Some(path) = &path {
                let mut dropped = Vec::new();
                self.queue.retain(|job| match &job.source {
                    Source::File(other) if other == path => {
                        dropped.push(job.key);
                        false
                    }
                    _ => true,
                });
                self.entries.retain(|entry| !dropped.contains(&entry.key));
            }
            let bytes = slot.bytes();
            self.bytes += bytes;
            self.queue.extend(job);
            self.entries.push_back(Embedded { key, slot, claims: Claims::one(owner, frame), bytes, path });
            self.trim();
        }
        self.entries.back().map_or(Slot::Failed, |entry| entry.slot.clone())
    }

    /// The newest decoded version of the local file `path`, kept claimed by `owner`: what shows
    /// while a newer version decodes.
    fn shown_version(&mut self, owner: u64, path: &Path) -> Option<Slot> {
        let frame = self.frames.get(&owner).copied().unwrap_or(0);
        let entry = self
            .entries
            .iter_mut()
            .rev()
            .find(|entry| entry.path.as_deref() == Some(path) && matches!(entry.slot, Slot::Pixels(_) | Slot::Encoded(_)))?;
        entry.claims.touch(owner, frame);
        Some(entry.slot.clone())
    }

    /// Stores the decoded `slot` of the pending entry `key`. An entry that was evicted while it
    /// decoded has nowhere to go: its pixels are freed with the others. A local file's other
    /// decoded versions are retired: this one replaces them.
    fn finish(&mut self, key: (usize, u64), slot: Slot) {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.key == key && matches!(entry.slot, Slot::Pending)) else {
            drop(slot); // never handed to GPUI: no texture or asset to release
            return;
        };
        entry.bytes = slot.bytes();
        entry.slot = slot;
        self.bytes += entry.bytes;
        if let Some(path) = entry.path.clone() {
            let (stale, kept): (VecDeque<_>, VecDeque<_>) = self.entries.drain(..).partition(|other| {
                other.key != key && other.path.as_ref() == Some(&path) && !matches!(other.slot, Slot::Pending)
            });
            self.entries = kept;
            for old in stale {
                self.bytes -= old.bytes;
                self.queue.retain(|job| job.key != old.key);
                self.evicted.push(old.retire());
            }
        }
        self.trim();
    }

    /// `owner` stopped drawing images: all of its pictures (`unseen_only` false: the preview
    /// switched documents or hid) or the ones its latest frame didn't draw. What no other owner
    /// draws moves to `evicted` and the jobs not started for it are forgotten; a local file's
    /// remembered header goes with it. Pictures another window still shows stay.
    fn release(&mut self, owner: u64, unseen_only: bool) {
        let frames = &self.frames;
        for entry in &mut self.entries {
            entry.claims.release(owner, unseen_only, frames);
        }
        let (kept, gone): (VecDeque<_>, VecDeque<_>) = self.entries.drain(..).partition(|entry| entry.claims.held(frames));
        self.entries = kept;
        for entry in &gone {
            if let Some(path) = &entry.path {
                forget_local_image(path);
            }
        }
        let entries = &self.entries;
        self.queue.retain(|job| entries.iter().any(|entry| entry.key == job.key));
        self.bytes = self.entries.iter().map(|entry| entry.bytes).sum();
        self.evicted.extend(gone.into_iter().map(Embedded::retire));
        self.files.retain(|_, file| {
            file.claims.release(owner, unseen_only, frames);
            file.claims.held(frames)
        });
    }

    /// `owner` (a closed window) is gone: its pictures are released and its frame count dropped.
    fn forget_owner(&mut self, owner: u64) {
        self.release(owner, false);
        self.frames.remove(&owner);
        self.drawn.remove(&owner);
    }

    /// Drops unclaimed entries while the cache is over either cap, the one that costs the most
    /// for how far it is from the view first (`far_pick`).
    fn trim(&mut self) {
        while self.entries.len() > EMBEDDED_CAP || self.bytes > EMBEDDED_BYTES_CAP {
            let Some(at) = far_pick(&self.entries, &self.frames) else {
                return;
            };
            let entry = self.entries.remove(at).expect("position is in range");
            self.bytes -= entry.bytes;
            self.evicted.push(entry.retire());
        }
    }

    /// `owner`'s picture `key` was replaced by a newer one that has drawn: its claim from before
    /// this frame goes, and the pixels with it unless another window still draws them or this
    /// frame did. Without it every edit, theme switch and divider drag left one more decoded
    /// picture behind until a cap pushed it out.
    fn release_key(&mut self, owner: u64, key: (usize, u64)) {
        let frames = &self.frames;
        let Some(at) = self.entries.iter().position(|entry| entry.key == key) else { return };
        let entry = &mut self.entries[at];
        entry.claims.release(owner, true, frames);
        if entry.claims.held(frames) {
            return;
        }
        let entry = self.entries.remove(at).expect("position is in range");
        self.bytes -= entry.bytes;
        self.evicted.push(entry.retire());
    }

    /// The ids the picture fade-in remembers (`retain_shown_images`) for the images held now.
    fn live_ids(&self) -> HashSet<u64> {
        self.entries.iter().filter_map(|entry| entry.slot.shown_id()).collect()
    }
}

/// Which unclaimed entry to drop first: the one with the most bytes times its distance from the
/// view, which `entries` (least recently drawn first) gives as its steps from the newest end. A
/// big picture far off screen goes before a small formula just off it; of equal sizes the
/// farthest goes. A tiny entry counts as 1 KB so a flood of them still thins from the far end.
fn far_pick(entries: &VecDeque<Embedded>, frames: &HashMap<u64, u64>) -> Option<usize> {
    let len = entries.len();
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| !entry.claims.held(frames))
        .max_by_key(|&(at, entry)| (entry.bytes.max(1024).saturating_mul(len - at), len - at))
        .map(|(at, _)| at)
}

static EMBEDDED: LazyLock<Mutex<EmbeddedImages>> = LazyLock::new(Mutex::default);

thread_local! {
    /// The app handle `pump` decodes and repaints through; set by the first render.
    static APP: RefCell<Option<AsyncApp>> = const { RefCell::new(None) };
    /// What rasterizes an SVG picture on the decode thread; set with `APP`.
    static SVG: RefCell<Option<SvgRenderer>> = const { RefCell::new(None) };
}

/// The system alert sound: a link that leads nowhere says so the way a dead key does.
pub fn beep() {
    unsafe extern "C" {
        fn NSBeep();
    }
    // SAFETY: AppKit is linked by GPUI; the call takes no arguments and is made on the main thread.
    unsafe { NSBeep() };
}

/// Starts the next queued decode unless one is running. Pixels decode on the background executor,
/// one image at a time; when one lands the next starts and the windows repaint. Main thread only.
fn pump() {
    let Some(app) = APP.with_borrow(Clone::clone) else { return };
    let Job { key, source } = {
        let mut cache = EMBEDDED.lock().unwrap_or_else(|e| e.into_inner());
        if cache.decoding {
            return;
        }
        let Some(job) = cache.queue.pop_front() else { return };
        cache.decoding = true;
        job
    };
    let Some(renderer) = SVG.with_borrow(Clone::clone) else { return };
    // The encoded bytes move into the task and are freed when it ends.
    let task = app.background_executor().spawn(async move { load(source, renderer) });
    app.spawn(async move |cx| {
        let slot = task.await;
        let evicted = {
            let mut cache = EMBEDDED.lock().unwrap_or_else(|e| e.into_inner());
            cache.decoding = false;
            cache.finish(key, slot);
            std::mem::take(&mut cache.evicted)
        };
        // The next queued picture starts; with none queued this does nothing.
        pump();
        cx.update(|cx| {
            // An older version of a changed file, or an entry over the caps, goes now.
            free(evicted, cx);
            cx.refresh_windows();
        });
    })
    .detach();
}

/// Frees what `evicted` held: GPUI's asset for an encoded image, the atlas texture for pixels.
fn free(evicted: Vec<Slot>, cx: &mut App) {
    for slot in evicted {
        match slot {
            Slot::Encoded(image) => image.remove_asset(cx),
            Slot::Pixels(image) => cx.drop_image(image, None),
            Slot::Pending | Slot::Failed => {}
        }
    }
}

/// Frees the decoded images only window `owner` shows: its preview switched documents or hid,
/// so none of its pictures is on screen. A picture another window still draws stays, so that
/// window never blanks or decodes it again. Showing the images again decodes them again.
pub fn release_embedded_images(owner: u64, cx: &mut App) {
    release(owner, false, cx);
}

/// Frees the decoded images window `owner`'s last frame didn't draw and no other window shows: a
/// preview that faded out has landed, and the one showing keeps its own, so it never decodes
/// them again.
pub fn release_unseen_embedded_images(owner: u64, cx: &mut App) {
    release(owner, true, cx);
}

/// A closed window's pictures go with it, and its frame count.
pub fn forget_embedded_owner(owner: u64, cx: &mut App) {
    let (evicted, live) = {
        let mut cache = EMBEDDED.lock().unwrap_or_else(|e| e.into_inner());
        cache.forget_owner(owner);
        (std::mem::take(&mut cache.evicted), cache.live_ids())
    };
    retain_shown_images(&live);
    if !evicted.is_empty() {
        cx.defer(move |cx| free(evicted, cx));
    }
}

/// What a rendered diagram or formula needs of the image cache (`diagrams.rs`): its pixels live
/// here like an embedded image, claimed per frame by the window that draws them and freed with
/// the window's other pictures. The entry is `Pending` from the miss until `finish_diagram_slot`.
///
/// The pixels under `key` for `owner`'s current frame; on a miss a pending entry is made and
/// the second value is `true`: the caller owes the render.
pub(super) fn diagram_slot(owner: u64, key: (usize, u64)) -> (Option<Arc<RenderImage>>, bool) {
    let mut missed = false;
    let slot = EMBEDDED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_key(owner, key, |_| {
        missed = true;
        Made { slot: Slot::Pending, job: None }
    });
    match slot {
        Slot::Pixels(image) => (Some(image), missed),
        _ => (None, missed),
    }
}

/// The pixels under `key` if held, kept claimed by `owner` without making an entry: what shows
/// while a newer version of the same diagram renders.
pub(super) fn diagram_pixels(owner: u64, key: (usize, u64)) -> Option<Arc<RenderImage>> {
    let mut cache = EMBEDDED.lock().unwrap_or_else(|e| e.into_inner());
    let frame = cache.frames.get(&owner).copied().unwrap_or(0);
    let entry = cache.entries.iter_mut().find(|entry| entry.key == key)?;
    entry.claims.touch(owner, frame);
    match &entry.slot {
        Slot::Pixels(image) => Some(image.clone()),
        _ => None,
    }
}

/// `owner`'s picture `old` has been replaced by a newer one that drew this frame (the diagram
/// was edited, the theme changed, the column was dragged): frees its pixels now unless another
/// window or this frame still draws them.
pub(super) fn diagram_release(owner: u64, old: (usize, u64), cx: &mut App) {
    let (evicted, live) = {
        let mut cache = EMBEDDED.lock().unwrap_or_else(|e| e.into_inner());
        cache.release_key(owner, old);
        (std::mem::take(&mut cache.evicted), cache.live_ids())
    };
    if !evicted.is_empty() {
        retain_shown_images(&live);
        free(evicted, cx);
    }
}

/// Whether `key` is still a pending entry some window draws: a render for an entry that was
/// evicted or released is skipped, and a pending one nobody drew in the latest frame or the one
/// before (scrolled past, typed over) is dropped here so it doesn't render ahead of what is shown.
pub(super) fn diagram_pending(key: (usize, u64)) -> bool {
    let mut cache = EMBEDDED.lock().unwrap_or_else(|e| e.into_inner());
    let Some(at) = cache.entries.iter().position(|entry| entry.key == key && matches!(entry.slot, Slot::Pending)) else {
        return false;
    };
    if cache.entries[at].claims.held(&cache.frames) {
        return true;
    }
    let entry = cache.entries.remove(at).expect("position is in range");
    cache.evicted.push(entry.retire());
    false
}

/// `owner`'s frame number: it moves once per frame the preview draws.
pub(super) fn diagram_frame(owner: u64) -> u64 {
    EMBEDDED.lock().unwrap_or_else(|e| e.into_inner()).frames.get(&owner).copied().unwrap_or(0)
}

/// Stores the rendered `pixels` (`None`: it failed) of the pending entry `key`, and frees what
/// the cache dropped to stay under its caps.
pub(super) fn finish_diagram_slot(key: (usize, u64), pixels: Option<Arc<RenderImage>>, cx: &mut App) {
    let evicted = {
        let mut cache = EMBEDDED.lock().unwrap_or_else(|e| e.into_inner());
        cache.finish(key, pixels.map_or(Slot::Failed, Slot::Pixels));
        std::mem::take(&mut cache.evicted)
    };
    free(evicted, cx);
}

/// The card fill of tables and code blocks: the foreground at `ALPHA_CARD` in the dark, at
/// `ALPHA_CARD_LIGHT` in the light (a faint grey on the white page).
pub(super) fn card_fill(foreground: Hsla, light: bool) -> Hsla {
    foreground.opacity(if light { ALPHA_CARD_LIGHT } else { ALPHA_CARD })
}

fn release(owner: u64, unseen_only: bool, cx: &mut App) {
    let (evicted, live) = {
        let mut cache = EMBEDDED.lock().unwrap_or_else(|e| e.into_inner());
        cache.release(owner, unseen_only);
        (std::mem::take(&mut cache.evicted), cache.live_ids())
    };
    retain_shown_images(&live);
    if !evicted.is_empty() {
        cx.defer(move |cx| free(evicted, cx));
    }
}

/// The markdown extensions the preview's state and its view both carry: web links end in an
/// arrow, and Mermaid blocks, `$$` blocks and `$` formulas are claimed (`diagrams.rs`). One value for the life of the process, so each frame's clone keeps its revision,
/// and the state, started with it, does not parse the file again when the panel shows.
pub fn extensions() -> MarkdownExtensions {
    static EXTENSIONS: LazyLock<MarkdownExtensions> = LazyLock::new(|| {
        MarkdownExtensions::default()
            .external_link_arrow()
            .plugin(super::diagrams::MermaidBlock)
            .plugin(super::diagrams::MathBlock)
            .plugin(super::diagrams::InlineMath)
    });
    EXTENSIONS.clone()
}

/// The preview's prose, from the theme: body 15 pt in the foreground, headings 26, 22, 17 and
/// 15 semibold, code at 0.867 em on a faint card, hairline tables with tabular figures, quotes
/// in the muted ink behind a 2 pt bar, links in system blue.
fn prose(scale_factor: f32, cx: &App) -> TextViewStyle {
    thread_local! {
        /// The style for what it was built from, with the editor font family: the view compares styles every frame, and
        /// a rebuilt one would cost that comparison and an allocation per scroll frame.
        static PROSE: RefCell<Option<((Hsla, Hsla, Hsla, Hsla, bool, Hsla, Hsla, Hsla, u32), Option<String>, TextViewStyle)>> = const { RefCell::new(None) };
    }
    let weight = crate::settings::get(cx).text.weight; // code keeps its [text] weight; applies at next start
    let theme = cx.theme();
    let light = theme::is_light(cx);
    let drawn = crate::settings::get(cx).editor.drawn_family(cx);
    let family = drawn.as_deref();
    // One device pixel for the lines of tables, code blocks and rules.
    let hairline = 1. / scale_factor;
    // The body text is the editor's plain text (`theme.foreground`), so prose and code read in one ink.
    let key = (theme.foreground, theme.foreground, theme::ink(ALPHA_SECONDARY, cx), theme.border, light, crate::explorer::icons::accent(), theme.background, theme::ink(ALPHA_TERTIARY, cx), hairline.to_bits());
    PROSE.with_borrow_mut(|cached| match cached {
        Some((built_for, built_family, style)) if *built_for == key && built_family.as_deref() == family => style.clone(),
        _ => {
            let style = build_prose(key.0, key.1, key.2, key.3, key.4, weight, key.5, key.6, key.7, hairline, family);
            *cached = Some((key, family.map(str::to_owned), style.clone()));
            style
        }
    })
}

fn build_prose(foreground: Hsla, text: Hsla, muted: Hsla, border: Hsla, light: bool, weight: f32, accent: Hsla, ground: Hsla, tertiary: Hsla, hairline: f32, family: Option<&str>) -> TextViewStyle {
    let dark = !light;
    let blue = Hsla::from(rgb(if dark { LINK_BLUE } else { LINK_BLUE_LIGHT }));
    let callout_colors = (if dark { CALLOUT_DARK } else { CALLOUT_LIGHT }).map(|hex| Hsla::from(rgb(hex)));
    let selection = if dark { ALPHA_TEXT_SELECTION } else { ALPHA_TEXT_SELECTION_LIGHT };
    let code = px(PREVIEW_BODY * PREVIEW_CODE_EM); // design: a share of the body size

    let mut cell = StyleRefinement::default();
    cell.text.font_features = Some(tabular());
    let mut quote = StyleRefinement::default().border_color(foreground.opacity(ALPHA_QUOTE_BAR));
    quote.border_widths.left = Some(QUOTE_BAR.into());
    // The editor's code font, so the preview's code reads as the editor's does (the weight is set below).
    let code_font = |style: StyleRefinement| match family {
        Some(family) => style.font_family(family.to_owned()),
        None => style,
    };
    // One card fill for tables and code blocks (`card_fill`); a table's head is one `ALPHA_CARD`
    // of the foreground above it.
    let card = card_fill(foreground, light);

    let style = TextViewStyle::default()
        .with_foreground(text)
        .with_muted_foreground(muted)
        .with_link(blue)
        .with_callout_colors(callout_colors)
        .with_task_fill(accent)
        .with_selection(Hsla::from(rgb(if dark { LINK_BLUE } else { SELECTION_BLUE_LIGHT })).opacity(selection))
        .with_code_background(card)
        .with_code_label(tertiary)
        .with_code_plate(ground.blend(card)) // opaque: the card's look over the ground, so a plate hides what is under it without stacking
        .with_code_plate_radius(RADIUS_BUTTON) // the copy button's own radius, so the plate wraps the button and the label alike
        .with_hairline(px(hairline))
        .with_border(border)
        .with_dark(dark)
        .with_measure(PREVIEW_MEASURE)
        // The reading inset is a share of the width this layout has (the final one while the panel slides), held between its floor and 24 pt, so a narrow panel keeps a column to read.
        .with_gutter(PREVIEW_READING_SHARE, PREVIEW_READING_INSET_MIN, PREVIEW_READING_INSET_RIGHT_MIN, PREVIEW_READING_INSET)
        .with_heading(|level| {
            let last = PREVIEW_HEADINGS.len() - 1;
            StyleRefinement::default().text_size(px(PREVIEW_HEADINGS[usize::from(level.max(1) - 1).min(last)])) // design: sizes are tokens
        })
        .with_code_block(
            code_font(StyleRefinement::default())
                .text_size(code)
                .font_weight(FontWeight(weight))
                .line_height(relative(PREVIEW_CODE_LINE))
                .bg(card)
                .rounded(RADIUS_CARD),
        )
        .with_inline_code(HighlightStyle {
            background_color: Some(foreground.opacity(ALPHA_SURFACE)),
            font_weight: Some(FontWeight(PREVIEW_INLINE_CODE_WEIGHT)), // lighter than the block's: the renderer thickens the code font, not the prose's
            ..Default::default()
        })
        .with_inline_code_style(
            // No size here: a span follows the run it sits in (a heading, a table cell) at the code ratio, set below.
            code_font(StyleRefinement::default())
                .rounded(RADIUS_TINT)
                .bg(foreground.opacity(ALPHA_SURFACE)),
        )
        .with_inline_code_scale(PREVIEW_CODE_EM)
        .with_callout_fill(ALPHA_CALLOUT_DARK, ALPHA_CALLOUT_LIGHT)
        .with_callout_bar(CALLOUT_BAR, ALPHA_CALLOUT_BAR)
        .with_callout_radius(RADIUS_CALLOUT)
        .with_link_underline(LINK_UNDERLINE, ALPHA_LINK_UNDERLINE)
        .with_blockquote_style(quote)
        .with_symbol(|name, size| crate::symbols::icon(name, size).into_any_element())
        .with_table({
            let mut table = StyleRefinement::default().rounded(RADIUS_CARD).bg(card);
            table.overflow.x = Some(Overflow::Scroll); // a too-wide table scrolls, never breaks a word
            table
        })
        .with_table_head(StyleRefinement::default().bg(foreground.opacity(ALPHA_CARD)).font_weight(FontWeight::SEMIBOLD))
        .with_table_cell(cell);
    // The light theme keeps the text view's pale yellow; a dark one lays amber at a low alpha so light ink still reads on it.
    if dark { style.with_mark_fill(Hsla::from(rgb(MARK_AMBER_DARK)).opacity(ALPHA_MARK_DARK)) } else { style }
}

/// How long a code block's copy button shows its check after a click.
const COPIED_FOR: Duration = Duration::from_millis(1200);

/// A code block's copy button: copies the code and shows a check for `COPIED_FOR`. The text view
/// puts it in the block's corner beside the language name and fades it in while the pointer is
/// over the block.
fn code_actions(block: &CodeBlock, window: &mut Window, cx: &mut App) -> Div {
    let code = block.code();
    // The block's place in the file, so two blocks with the same text copy on their own; a block without a span falls back to its text.
    let key = block.span.map_or_else(
        || {
            let mut hasher = DefaultHasher::new();
            code.hash(&mut hasher);
            hasher.finish()
        },
        |span| span.start as u64,
    );
    let copied = window.use_keyed_state(ElementId::from(("code-copied", key as usize)), cx, |_, _| (None::<Instant>, Fade::with(0., FADE_QUICK)));
    // The check crossfades with the clipboard over FADE_QUICK, turning around from where it is drawn when a copy lands mid-fade.
    let now = crate::motion::drawn_now(window.window_handle().window_id());
    let (check, moving, copying) = copied.update(cx, |(at, fade), _| {
        let copying = at.is_some_and(|at| at.elapsed() < COPIED_FOR);
        fade.aim(if copying { 1. } else { 0. }, now);
        (fade.level(now), fade.is_moving(now), copying)
    });
    if moving {
        window.request_animation_frame();
    }
    let view = window.current_view();
    let (rest, lit) = (theme::ink(ALPHA_SECONDARY, cx), theme::ink(ALPHA_PRIMARY, cx));
    let ink = Ink { rest, lit, wash: theme::ink(1., cx) };
    let id = ElementId::from(("code-copy", key as usize));
    let button = div()
        .id(id.clone())
        .size(ICON_BUTTON)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .relative()
        .fade_hover(id, None, ink.wash.opacity(ALPHA_BUTTON_HOVER))
        .press_fill(ink.wash.opacity(ALPHA_BUTTON_PRESSED))
        .fade_ink(ink.rest, ink.lit)
        .child(div().opacity(1. - check).child(crate::symbols::icon("doc.on.doc", COPY_SYMBOL)))
        .child(div().absolute().size_full().flex().items_center().justify_center().opacity(check).child(crate::symbols::icon("checkmark", COPY_SYMBOL)))
        .rounded(RADIUS_BUTTON)
        .tooltip(tooltip::tip(if copying { "Copied" } else { "Copy" }))
        .tooltip_show_delay(tooltip::DELAY)
        // A press here is the button's, not the start of a text selection.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
            copied.update(cx, |(at, _), _| *at = Some(Instant::now()));
            cx.notify(view);
            cx.spawn(async move |cx| {
                cx.background_executor().timer(COPIED_FOR).await;
                let _ = cx.update(|cx| cx.notify(view));
            })
            .detach();
        });
    div().child(button)
}

/// Where the link under the pointer goes, in a small pill at the preview's bottom-left, as
/// Safari's status pill does: the address of a web link, the file name of a local one, `#heading`
/// of an in-page one. It stands on the float plane a tooltip does, shows after the pointer has
/// rested `TOOLTIP_DELAY` on the link (at once while it is already showing, so moving between
/// links doesn't blink it), fades in over `HOVER_IN` and out over `HOVER_OUT`, turns around from
/// the level drawn when the pointer comes back, and never takes a click. The text view reports
/// the link by notifying its state, so a preview at rest draws nothing for it.
struct LinkPill {
    state: WeakEntity<TextViewState>,
    /// The markdown file's folder, which a relative link's file name starts from.
    dir: Option<PathBuf>,
    /// The link the label is for; the label stays while the pill fades out.
    link: Option<SharedString>,
    label: SharedString,
    fade: Fade,
    /// The hover whose rest timer is running.
    waiting: Option<Instant>,
}

impl LinkPill {
    fn new(state: WeakEntity<TextViewState>, dir: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        if let Some(source) = state.upgrade() {
            cx.observe(&source, |_, _, cx| cx.notify()).detach();
        }
        Self { state, dir, link: None, label: SharedString::default(), fade: Fade::at(0.), waiting: None }
    }
}

/// What the pill says of the link `url`, in a markdown file inside `dir`: a web link as written, an
/// in-page link as `#heading`, a local file by its name (and `#heading` when it has one).
fn pill_label(url: &str, dir: Option<&Path>) -> String {
    let words = if url.starts_with('#') || matches!(scheme(url).as_deref(), Some("http" | "https" | "mailto")) {
        url.to_owned()
    } else {
        let name = local_path(url, dir).and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()));
        match (name, url.split_once('#')) {
            (Some(name), Some((_, fragment))) if !fragment.is_empty() => format!("{name}#{fragment}"),
            (Some(name), _) => name,
            (None, _) => url.to_owned(),
        }
    };
    crate::explorer::cut_middle(&percent_decode(&words), LINK_PILL_MAX_CHARS)
}

impl Render for LinkPill {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        let hover = self.state.read_with(cx, |state, _| state.hovered_link()).ok().flatten();
        match hover {
            Some((url, since)) => {
                if self.link.as_ref() != Some(&url) {
                    self.label = pill_label(&url, self.dir.as_deref()).into();
                    self.link = Some(url);
                }
                let waited = since.elapsed();
                if waited >= TOOLTIP_DELAY || self.fade.level(now) > 0. {
                    self.fade.aim(1., now);
                } else if self.waiting != Some(since) {
                    self.waiting = Some(since);
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(TOOLTIP_DELAY - waited).await;
                        this.update(cx, |_, cx| cx.notify()).ok();
                    })
                    .detach();
                }
            }
            None => {
                self.waiting = None;
                self.fade.aim(0., now);
            }
        }
        if self.fade.is_moving(now) {
            window.request_animation_frame();
        }
        let level = self.fade.level(now);
        if level <= 0. {
            return div().into_any_element();
        }
        let theme = cx.theme();
        let light = theme::is_light(cx);
        let (bg, border) = tooltip::float_fill(theme.background, theme.sidebar_foreground, light);
        let pill = tooltip::float(RADIUS_TOOLTIP, bg, border, light, false)
            .flex()
            .min_w_0()
            .px(px(TOOLTIP_PAD_X))
            .py(px(TOOLTIP_PAD_Y))
            .text_size(TEXT_SUBHEADLINE)
            .line_height(TEXT_SUBHEADLINE_LINE)
            .font_weight(FontWeight::NORMAL)
            .text_color(theme::ink(ALPHA_PRIMARY, cx))
            .opacity(level)
            .child(div().min_w_0().truncate().child(self.label.clone()))
            .children(crate::control::track("link-pill"));
        div().absolute().left(px(LINK_PILL_INSET)).right(px(LINK_PILL_INSET)).bottom(px(LINK_PILL_INSET)).flex().child(pill).into_any_element()
    }
}

/// The preview's element for `state`, the parsed text of `file`. Built when drawn, so it takes
/// its colors from the theme of the frame; style it like a `div`.
pub fn text_view(state: &Entity<TextViewState>, file: Option<&Path>) -> PreviewText {
    PreviewText {
        state: state.clone(),
        dir: file.and_then(Path::parent).map(Path::to_path_buf),
        style: StyleRefinement::default(),
    }
}

/// The preview's text: a `TextView` in the preview's prose. The view and its scrollbar fill the
/// panel; each block is held to the measure and centered.
#[derive(IntoElement)]
pub struct PreviewText {
    state: Entity<TextViewState>,
    /// The markdown file's folder, which relative links and images start from.
    dir: Option<PathBuf>,
    style: StyleRefinement,
}

impl Styled for PreviewText {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

/// What the preview's right-click menu can do.
#[derive(Clone, Debug, PartialEq)]
enum PreviewCommand {
    OpenLink(String),
    CopyLink(String),
    Copy,
    CopyMarkdown,
    CopyRichText,
    SelectAll,
}

/// The rows of the preview's right-click menu: on a link, Open link and Copy link address come
/// first; Copy and Copy as Markdown grey out with nothing selected (Copy as Rich Text takes the whole document then).
fn menu_entries(selected: bool, link: Option<&str>) -> Vec<Entry<PreviewCommand>> {
    let item = |title: &str, run: PreviewCommand, symbol: &'static str| Item::new(title, run).symbol(symbol);
    let mut entries = Vec::with_capacity(8);
    if let Some(url) = link {
        entries.push(Entry::Item(item("Open link", PreviewCommand::OpenLink(url.to_owned()), "arrow.up.forward.square")));
        entries.push(Entry::Item(item("Copy link address", PreviewCommand::CopyLink(url.to_owned()), "link")));
        entries.push(Entry::Separator);
    }
    entries.extend([
        Entry::Item(item("Copy", PreviewCommand::Copy, "doc.on.doc").key("c", COMMAND).disabled(!selected)),
        Entry::Item(item("Copy as Markdown", PreviewCommand::CopyMarkdown, "number").disabled(!selected)),
        Entry::Item(item("Copy as Rich Text", PreviewCommand::CopyRichText, "doc.richtext")),
        Entry::Separator,
        Entry::Item(item("Select all", PreviewCommand::SelectAll, "selection.pin.in.out").key("a", COMMAND)),
    ]);
    entries
}

/// The Copied badge's level at the frame drawn at `now`, and whether it still moves: aimed in while
/// the latest copy is under `COPIED_FOR` old by the wall clock `real` (so a trailing frame clock
/// can't hold it), out after; the fade runs on the drawn time `now`, always from the level drawn.
fn badge_step(at: Option<Instant>, fade: &mut Fade, now: Instant, real: Instant) -> (f32, bool) {
    fade.aim(if at.is_some_and(|at| real.saturating_duration_since(at) < COPIED_FOR) { 1. } else { 0. }, now);
    (fade.level(now), fade.is_moving(now))
}

/// Stamps the preview's Copied badge: it fades in, holds for `COPIED_FOR` from the latest copy and
/// fades out. A copy while it is leaving aims it back in from where it's drawn. The timer wakes the
/// view once the hold is over, so the fade-out starts without another event.
fn stamp_copied(copied: &Entity<(Option<Instant>, Fade)>, view: EntityId, cx: &mut App) {
    copied.update(cx, |(at, _), _| *at = Some(Instant::now()));
    cx.notify(view);
    cx.spawn(async move |cx| {
        cx.background_executor().timer(COPIED_FOR).await;
        cx.update(|cx| cx.notify(view));
    })
    .detach();
}

/// Opens the preview's right-click menu (a native one) once the frame has drawn: nothing draws
/// while a native menu is open. Two frames on, since a callback runs at the start of the frame
/// after the one it was queued in.
fn show_menu(state: &Entity<TextViewState>, dir: Option<PathBuf>, copied: Entity<(Option<Instant>, Fade)>, window: &mut Window, cx: &mut App) {
    let (selected, link) = state.update(cx, |state, _| (!state.selected_text().trim().is_empty(), state.take_context_link()));
    let entries = menu_entries(selected, link.as_deref());
    let state = state.clone();
    let view = window.current_view();
    window.on_next_frame(move |window, _| {
        window.on_next_frame(move |window, cx| {
            window
                .spawn(cx, async move |cx| {
                    let Some(command) = native_menu::pop_up(&entries, None) else { return };
                    if command == PreviewCommand::CopyRichText {
                        let Ok(task) = cx.update(|_, cx| copy_rich(&state, dir.clone(), cx)) else { return };
                        if task.await {
                            cx.update(|_, cx| stamp_copied(&copied, view, cx)).ok();
                        }
                        return;
                    }
                    cx.update(|window, cx| match command {
                        PreviewCommand::OpenLink(url) => open_link(&url, dir.as_deref(), &state.downgrade(), window, cx),
                        PreviewCommand::CopyLink(url) => {
                            cx.write_to_clipboard(ClipboardItem::new_string(url));
                            stamp_copied(&copied, view, cx);
                        }
                        PreviewCommand::Copy => {
                            let text = state.read(cx).selected_text().trim().to_string();
                            if !text.is_empty() {
                                cx.write_to_clipboard(ClipboardItem::new_string(text));
                                stamp_copied(&copied, view, cx);
                            }
                        }
                        PreviewCommand::CopyMarkdown => {
                            let markdown = state.read(cx).selected_markdown();
                            if let Some(text) = markdown.as_deref().map(trim_source).filter(|text| !text.is_empty()) {
                                cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
                                stamp_copied(&copied, view, cx);
                            }
                        }
                        PreviewCommand::SelectAll => state.update(cx, |state, cx| state.select_all(cx)),
                        PreviewCommand::CopyRichText => {}
                    })
                    .ok();
                })
                .detach();
        });
    });
}

/// Select all starts with the blanked front matter: cut leading blank lines, keep the first real line's indent.
fn trim_source(text: &str) -> &str {
    let start: usize = text.split_inclusive('\n').take_while(|line| line.trim().is_empty()).map(str::len).sum();
    text[start..].trim_end()
}

/// Copy as Rich Text: the selection's Markdown (the whole document when nothing is selected; the
/// selection is cleared again once read) as HTML, RTF and plain text on the pasteboard in one write.
/// The HTML is built off the main thread; AppKit's conversion to RTF and the write run on it.
/// Resolves to whether something was copied.
pub fn copy_rich(state: &Entity<TextViewState>, dir: Option<PathBuf>, cx: &mut App) -> Task<bool> {
    let nothing_selected = state.read(cx).selected_text().trim().is_empty();
    if nothing_selected {
        state.update(cx, |state, cx| state.select_all(cx));
    }
    let markdown = state.read(cx).selected_markdown().map(|text| trim_source(&text).to_owned()).filter(|text| !text.is_empty());
    if nothing_selected {
        state.update(cx, |state, cx| state.clear_selection(cx));
    }
    cx.spawn(async move |cx| {
        let Some(markdown) = markdown else { return false };
        let built = cx.background_executor().spawn(async move {
            let html = crate::rich_copy::html(&markdown, dir.as_deref());
            (markdown, html)
        });
        let (markdown, html) = built.await;
        cx.update(|_| crate::rich_copy::write(&markdown, &html))
    })
}

/// What opening the link `url` does, in a markdown file inside `dir`.
fn open_link(url: &str, dir: Option<&Path>, state: &WeakEntity<TextViewState>, window: &mut Window, cx: &mut App) {
    match resolve_link(url, dir) {
        LinkTarget::Web(url) => cx.open_url(&url),
        LinkTarget::Markdown(path, anchor) => window.dispatch_action(Box::new(OpenFile { path, anchor, caret: None }), cx),
        LinkTarget::Heading(anchor) => {
            state.update(cx, |state, _| state.set_pending_anchor(Some(anchor))).ok();
            window.refresh();
        }
        LinkTarget::Other(path) => cx.open_with_system(&path),
        LinkTarget::Nothing => beep(),
    }
}

impl RenderOnce for PreviewText {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let owner = window.window_handle().window_id().as_u64();
        let evicted = {
            let mut cache = EMBEDDED.lock().unwrap_or_else(|e| e.into_inner());
            if cache.first_in_frame(owner, crate::motion::drawn_now(window.window_handle().window_id())) {
                cache.begin_frame(owner);
            }
            std::mem::take(&mut cache.evicted)
        };
        if !evicted.is_empty() {
            cx.defer(move |cx| free(evicted, cx));
        }
        APP.with_borrow_mut(|app| {
            if app.is_none() {
                *app = Some(cx.to_async());
                SVG.set(Some(cx.svg_renderer()));
            }
        });
        let Self { state, dir, style } = self;
        // The scrollbar's hand-scroll clock lives as long as the preview keeps rendering.
        let scroller = window.use_keyed_state(("preview-scroller", state.entity_id()), cx, |_, _| crate::scrollbar::Scroller::default());
        let scroller = scroller.read(cx).clone();
        let handle = state.read(cx).scrollbar_handle();
        let ground = cx.theme().background;
        let image_dir = dir.clone();
        // Weak: Kit stores the link handler on the state, so a strong handle is a cycle that keeps a closed tab's preview alive.
        let click_state = state.downgrade();
        let preview_id = state.entity_id().as_u64();
        let parsed_len = state.read(cx).parsed_len();
        let menu_dir = dir.clone();
        let pill = window.use_keyed_state(("link-pill", state.entity_id()), cx, |_, cx| LinkPill::new(state.downgrade(), dir.clone(), cx));
        // The folder follows a renamed file without waking the pill.
        pill.update(cx, |pill, _| pill.dir.clone_from(&dir));

        let copied = window.use_keyed_state(("rich-copied", state.entity_id()), cx, |_, _| (None::<Instant>, Fade::with(0., FADE_QUICK)));
        let view = TextView::new(&state)
            .scrollable(true)
            .scrollbar(false)
            .selectable(true)
            .markdown_extensions(extensions())
            .style(prose(window.scale_factor(), cx))
            .code_block_actions(code_actions)
            .text_size(px(PREVIEW_BODY)) // design: a token's value
            .line_height(relative(PREVIEW_LINE))
            .on_link_click(move |url, click, window, cx| {
                if opens(click) {
                    open_link(url.as_ref(), dir.as_deref(), &click_state, window, cx);
                }
            })
            .on_task_click(move |item, window, cx| {
                window.dispatch_action(Box::new(ToggleTask { item_start: item.start, item_end: item.end, parsed_len, preview: preview_id }), cx)
            })
            .on_copied({
                let copied = copied.clone();
                let view = window.current_view();
                move |_, cx| stamp_copied(&copied, view, cx)
            })
            .image_source(move |uri| {
                let url: &str = uri.as_ref();
                if url.starts_with("data:") {
                    let slot = EMBEDDED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(owner, url, |key| prepare(key, url));
                    return match slot {
                        Slot::Pixels(image) => ImageSource::Render(image),
                        Slot::Encoded(image) => ImageSource::Image(image),
                        Slot::Pending => {
                            pump();
                            ImageSource::Custom(Arc::new(|_, _| None))
                        }
                        Slot::Failed => uri.clone().into(),
                    };
                }
                local_image(owner, url, image_dir.as_deref()).unwrap_or_else(|| uri.clone().into())
            });
        // The check shows after Copy as Rich Text, the menu's other copies and Cmd-C, as the code block's copy button does: in for COPIED_FOR, crossfading over FADE_QUICK.
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        let (check, moving) = copied.update(cx, |(at, fade), _| badge_step(*at, fade, now, Instant::now()));
        if moving {
            window.request_animation_frame();
        }
        let badge = (check > 0.).then(|| {
            let theme = cx.theme();
            let light = theme::is_light(cx);
            let (bg, border) = tooltip::float_fill(theme.background, theme.sidebar_foreground, light);
            let badge = tooltip::float(RADIUS_TOOLTIP, bg, border, light, false)
                .size(ICON_BUTTON)
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme::ink(ALPHA_PRIMARY, cx))
                .opacity(check)
                .child(crate::symbols::icon("checkmark", COPY_SYMBOL));
            div().absolute().top(px(LINK_PILL_INSET)).right(COPIED_BADGE_INSET_RIGHT).child(badge)
        });
        // The view fills the container (the caller's style), the overlay covers it.
        let menu_state = state.clone();
        div()
            .on_mouse_down(MouseButton::Right, move |_, window, cx| show_menu(&menu_state, menu_dir.clone(), copied.clone(), window, cx))
            .relative()
            .refine_style(&style)
            .child(crate::scrollbar::watch(&scroller))
            .child(view.size_full())
            .child(crate::scrollbar::overlay(crate::scrollbar::Overlay { handle, scroller: scroller.clone(), ground: Some(ground) }))
            .child(pill)
            .children(badge)
    }
}

/// Whether a click on a link opens it: the left button, a key press or a tap. A right or
/// middle click leaves the link alone (the right one pops up the preview's menu).
fn opens(click: &ClickEvent) -> bool {
    match click {
        ClickEvent::Mouse(click) => click.up.button == MouseButton::Left,
        ClickEvent::Keyboard(_) => true,
        ClickEvent::Touch(click) => !click.long_press,
    }
}

/// The URL's scheme, lowercased, when it has one of two or more letters
/// (so a Windows drive letter is not a scheme).
fn scheme(url: &str) -> Option<String> {
    let (scheme, _) = url.split_once(':')?;
    let valid = scheme.len() > 1
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then(|| scheme.to_ascii_lowercase())
}

/// The on-disk path a relative or `file://` URL names, with any `#fragment`
/// dropped and `%20`-style escapes decoded. `None` for any other scheme, an
/// empty path, or a relative path when the markdown file's folder is unknown.
fn local_path(url: &str, dir: Option<&Path>) -> Option<PathBuf> {
    let url = url.split('#').next().unwrap_or("");
    let raw = match scheme(url).as_deref() {
        Some("file") => {
            let rest = &url["file:".len()..];
            let rest = rest.strip_prefix("//").unwrap_or(rest);
            // `file://localhost/x` and `file:///x` both mean `/x`.
            let rest = rest.strip_prefix("localhost").unwrap_or(rest);
            rest.to_owned()
        }
        Some(_) => return None,
        None => url.to_owned(),
    };
    if raw.is_empty() {
        return None;
    }
    let path = PathBuf::from(percent_decode(&raw));
    if path.is_absolute() {
        Some(path)
    } else {
        dir.map(|dir| dir.join(path))
    }
}

/// Decides what a click on `url` does, in a markdown file inside `dir`. A `#fragment` is the
/// anchor of a heading, decoded and lowercased; one on its own is a heading of this file.
pub(crate) fn resolve_link(url: &str, dir: Option<&Path>) -> LinkTarget {
    if matches!(scheme(url).as_deref(), Some("http" | "https" | "mailto")) {
        return LinkTarget::Web(url.to_owned());
    }
    let anchor = url
        .split_once('#')
        .map(|(_, fragment)| percent_decode(fragment).to_lowercase())
        .filter(|fragment| !fragment.is_empty());
    if url.starts_with('#') {
        return anchor.map_or(LinkTarget::Nothing, LinkTarget::Heading);
    }
    let Some(path) = local_path(url, dir) else {
        return LinkTarget::Nothing;
    };
    if !path.exists() {
        return LinkTarget::Nothing;
    }
    // The editor's own name rule, so `.mdx`, `.mdwn`, `.mdc` and `.MD` open here too.
    let is_markdown = crate::language::for_path(Some(&path)) == "markdown";
    if is_markdown && path.is_file() {
        LinkTarget::Markdown(path, anchor)
    } else {
        LinkTarget::Other(path)
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What an image carried inline as a `data:` URL becomes: GIFs and SVGs stay encoded for GPUI's
/// loader, other formats become a decode job. Anything else is `Failed`, and the caller keeps
/// the URL URI-backed so GPUI's loader reports it.
fn prepare(key: (usize, u64), url: &str) -> Made {
    let failed = Made { slot: Slot::Failed, job: None };
    let Ok(data_url) = DataUrl::process(url) else { return failed };
    let mime = data_url.mime_type();
    let Some(format) = ImageFormat::from_mime_type(&format!("{}/{}", mime.type_, mime.subtype)) else { return failed };
    let Ok((bytes, _fragment)) = data_url.decode_to_vec() else { return failed };
    let decoded = match format {
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Webp => image::ImageFormat::WebP,
        ImageFormat::Bmp => image::ImageFormat::Bmp,
        ImageFormat::Tiff => image::ImageFormat::Tiff,
        ImageFormat::Ico => image::ImageFormat::Ico,
        ImageFormat::Pnm => image::ImageFormat::Pnm,
        ImageFormat::Gif | ImageFormat::Svg => {
            return Made { slot: Slot::Encoded(Arc::new(Image::from_bytes(format, bytes))), job: None };
        }
    };
    Made { slot: Slot::Pending, job: Some(Job { key, source: Source::Bytes { format: decoded, bytes } }) }
}

/// Reads and decodes one job's image, on the background thread: pixels in the order GPUI
/// uploads, or for a GIF or SVG the encoded image GPUI's loader decodes itself.
fn load(source: Source, renderer: SvgRenderer) -> Slot {
    match source {
        Source::Bytes { format, bytes } => {
            // An animated WebP goes through GPUI's frame decoder like a GIF; a still keeps `decode`.
            if format == image::ImageFormat::WebP && is_animated_webp(&bytes) {
                Image::from_bytes(ImageFormat::Webp, bytes).to_image_data(renderer).map_or(Slot::Failed, Slot::Pixels)
            } else {
                decode(format, &bytes).map_or(Slot::Failed, Slot::Pixels)
            }
        }
        Source::File(path) => {
            use std::io::Read as _;
            // The first bytes say whether this is a picture at all, before a big non-picture is read.
            let Ok(mut file) = std::fs::File::open(&path) else { return Slot::Failed };
            let mut bytes = Vec::new();
            if (&mut file).take(64).read_to_end(&mut bytes).is_err() {
                return Slot::Failed;
            }
            let svg = path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("svg"));
            let format = image::guess_format(&bytes).ok();
            if !svg && format.is_none() {
                return Slot::Failed;
            }
            if file.read_to_end(&mut bytes).is_err() {
                return Slot::Failed;
            }
            // GIF (frames) and SVG (rasterized) go through GPUI's own decoder, here and not on the
            // main thread, so a broken one is `Failed` and shows the plate.
            let encoded = if svg {
                Some(ImageFormat::Svg)
            } else if format == Some(image::ImageFormat::Gif) {
                Some(ImageFormat::Gif)
            } else if format == Some(image::ImageFormat::WebP) && is_animated_webp(&bytes) {
                Some(ImageFormat::Webp)
            } else {
                None
            };
            match (encoded, format) {
                (Some(encoded), _) => Image::from_bytes(encoded, bytes).to_image_data(renderer).map_or(Slot::Failed, Slot::Pixels),
                (None, Some(format)) => decode(format, &bytes).map_or(Slot::Failed, Slot::Pixels),
                (None, None) => Slot::Failed,
            }
        }
    }
}

/// What a picture's URL `url`, in the folder `dir`, shows now; `None` when it names no local
/// file. The first sight of a URL starts a background stat and shows nothing until it lands;
/// later frames use the remembered stamp (no disk, no allocation) and a background stat refreshes
/// it every `RECHECK`. The cache key is the path
/// with the stamp, so an edited file is a new image: it is read and decoded on `pump`'s queue,
/// one at a time, while the older version stays on screen, or the box is reserved from the file's
/// header. A file that is missing or can't be read shows the plate.
fn local_image(owner: u64, url: &str, dir: Option<&Path>) -> Option<ImageSource> {
    thread_local! {
        static UNREADABLE: ImageSource = ImageSource::Custom(Arc::new(|_, _| Some(Err(ImageCacheError::Io(Arc::new(std::io::Error::other("not a readable picture")))))));
    }
    let (len, hash) = url_key(url);
    let dir_hash = dir.map_or(0, |dir| url_key(&dir.to_string_lossy()).1);
    let fkey = (len, hash ^ dir_hash.rotate_left(21));
    let known = {
        let mut cache = EMBEDDED.lock().unwrap_or_else(|e| e.into_inner());
        let frame = cache.frames.get(&owner).copied().unwrap_or(0);
        cache.files.get_mut(&fkey).map(|file| {
            file.claims.touch(owner, frame);
            let due = !file.checking && file.checked.elapsed() > RECHECK;
            file.checking |= due;
            (file.path.clone(), file.base, file.stamp, due)
        })
    };
    let (path, base, stamp) = match known {
        Some((path, base, stamp, due)) => {
            if due {
                recheck(fkey, path.clone());
            }
            (path, base, stamp)
        }
        None => {
            let path: Arc<Path> = local_path(url, dir)?.into();
            let base = url_key(&path.to_string_lossy());
            let now = EMBEDDED.lock().unwrap_or_else(|e| e.into_inner()).add_file(owner, fkey, path.clone(), base);
            if now {
                recheck(fkey, path);
            } else {
                settle(fkey);
            }
            return Some(ImageSource::Custom(Arc::new(|_, _| None)));
        }
    };
    let stamp = match stamp {
        Some(Some(stamp)) => stamp,
        Some(None) => return Some(UNREADABLE.with(Clone::clone)),
        None => return Some(ImageSource::Custom(Arc::new(|_, _| None))),
    };
    let key = (base.0, base.1 ^ stamp.wrapping_mul(MUL));
    let (slot, shown) = {
        let mut cache = EMBEDDED.lock().unwrap_or_else(|e| e.into_inner());
        let slot = cache.get_or_insert_key(owner, key, |key| Made { slot: Slot::Pending, job: Some(Job { key, source: Source::File(path.clone()) }) });
        let shown = if matches!(slot, Slot::Pending) { cache.shown_version(owner, &path) } else { None };
        (slot, shown)
    };
    Some(match slot {
        Slot::Pixels(image) => ImageSource::Render(image),
        Slot::Encoded(image) => ImageSource::Image(image),
        Slot::Pending => {
            pump();
            match shown {
                Some(Slot::Pixels(image)) => ImageSource::Render(image),
                Some(Slot::Encoded(image)) => ImageSource::Image(image),
                _ => ImageSource::Custom(Arc::new(move |window, cx| {
                    reserve_local_image(&path, stamp, window, cx);
                    None
                })),
            }
        }
        Slot::Failed => UNREADABLE.with(Clone::clone),
    })
}

/// Stats the file of the local picture `fkey` on the background executor and takes its new
/// stamp; the windows repaint only when it changed.
fn recheck(fkey: (usize, u64), path: Arc<Path>) {
    let Some(app) = APP.with_borrow(Clone::clone) else { return };
    let task = app.background_executor().spawn(async move { file_stamp(&path) });
    app.spawn(async move |cx| {
        let stamp = task.await;
        let changed = {
            let mut cache = EMBEDDED.lock().unwrap_or_else(|e| e.into_inner());
            cache.files.get_mut(&fkey).is_some_and(|file| {
                file.checking = false;
                file.checked = Instant::now();
                file.stamp.replace(stamp) != Some(stamp)
            })
        };
        if changed {
            cx.update(|cx| cx.refresh_windows());
        }
    })
    .detach();
}

/// Waits out the typing pause, then stats the local picture `fkey` if the text still has it.
fn settle(fkey: (usize, u64)) {
    let Some(app) = APP.with_borrow(Clone::clone) else { return };
    app.spawn(async move |cx| {
        cx.background_executor().timer(TYPING_PAUSE).await;
        let path = EMBEDDED.lock().unwrap_or_else(|e| e.into_inner()).settle_file(fkey);
        if let Some(path) = path {
            recheck(fkey, path);
        }
    })
    .detach();
}

/// The longest side a still image may have and still be decoded.
const MAX_SIDE: u32 = 16384;

/// The most decoded bytes (width x height x 4) one still image may take: 256 MB.
const MAX_DECODED: u64 = 256 << 20;

/// Whether an image this size, as its header claims, is small enough to decode. A header can
/// claim 65535 x 65535 in a few bytes, so this is checked before any pixel buffer is made.
fn within_limits(width: u32, height: u32) -> bool {
    width <= MAX_SIDE && height <= MAX_SIDE && u64::from(width) * u64::from(height) * 4 <= MAX_DECODED
}

/// Decodes a still image to the BGRA pixels GPUI uploads, with one buffer: an 8-bit RGB or RGBA
/// image is read into the front of its final buffer and widened and swapped in place, where
/// `Image::to_image_data` builds the RGB image and then a second, RGBA one beside it.
fn decode(format: image::ImageFormat, bytes: &[u8]) -> Option<Arc<RenderImage>> {
    use image::{ColorType, ImageDecoder as _, metadata::Orientation};
    let mut decoder = image::ImageReader::with_format(std::io::Cursor::new(bytes), format).into_decoder().ok()?;
    let orientation = decoder.orientation().ok()?;
    let (width, height) = decoder.dimensions();
    // The decoder's own buffer (16-bit and float images are wider than 4 bytes a pixel), and a
    // turned image holds a second copy while it is turned.
    let allowance = if orientation == Orientation::NoTransforms { MAX_DECODED } else { MAX_DECODED / 2 };
    if !within_limits(width, height) || decoder.total_bytes() > allowance {
        return None;
    }
    let pixels = width as usize * height as usize;
    let channels = match decoder.color_type() {
        ColorType::Rgb8 => 3,
        ColorType::Rgba8 => 4,
        _ => 0,
    };
    let buffer = if channels == 0 || orientation != Orientation::NoTransforms {
        let mut image = image::DynamicImage::from_decoder(decoder).ok()?;
        image.apply_orientation(orientation);
        let mut buffer = image.into_rgba8();
        for pixel in buffer.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        buffer
    } else {
        let mut data = vec![0u8; pixels * 4];
        decoder.read_image(&mut data[..pixels * channels]).ok()?;
        if channels == 3 {
            for at in (0..pixels).rev() {
                let (r, g, b) = (data[at * 3], data[at * 3 + 1], data[at * 3 + 2]);
                data[at * 4..at * 4 + 4].copy_from_slice(&[b, g, r, 255]);
            }
        } else {
            for pixel in data.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
        }
        image::RgbaImage::from_raw(width, height, data)?
    };
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

/// Whether `bytes` is a WebP with more than one frame.
fn is_animated_webp(bytes: &[u8]) -> bool {
    image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(bytes)).is_ok_and(|decoder| decoder.has_animation())
}

/// What `image` costs once decoded: width x height x 4, read from its raster
/// header or SVG size (including GPUI's SVG rendering scale). Unreadable or
/// unsupported headers count as 8 times their encoded size.
fn decoded_bytes(image: &Image) -> usize {
    match gpui_kit::base::text::image_dimensions(&image.bytes) {
        Some((w, h)) => (w as usize).saturating_mul(h as usize).saturating_mul(4),
        None => image.bytes.len().saturating_mul(8),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use gpui_kit::{Image, ImageFormat};

    use gpui_kit::{ClickEvent, Modifiers, MouseButton, MouseClickEvent, MouseDownEvent, MouseUpEvent, point, px};

    use super::{COPIED_FOR, EMBEDDED_CAP, EmbeddedImages, Job, LinkTarget, Made, PreviewCommand, RenderImage, Slot, badge_step, decoded_bytes, menu_entries, opens, resolve_link};
    use crate::motion::Fade;
    use crate::native_menu::Entry;
    use crate::tokens::FADE_QUICK;
    use std::time::{Duration, Instant};

    #[test]
    fn copied_badge_fades_in_holds_turns_around_and_stops() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut fade = Fade::with(0., FADE_QUICK);
        let mut stamp = None;
        let mut series = Vec::new();
        // One copy at 0 ms, a second at 1250 ms while the first is fading out.
        for ms in (0..1700).step_by(25) {
            if ms == 0 {
                stamp = Some(at(0));
            }
            if ms == 1250 {
                stamp = Some(at(1250));
            }
            let (level, moving) = badge_step(stamp, &mut fade, at(ms), at(ms));
            series.push((ms, level, moving));
        }
        println!("badge level series (ms, level, moving): {series:?}");
        let level = |ms: u64| series.iter().find(|s| s.0 == ms).unwrap().1;
        let moving = |ms: u64| series.iter().find(|s| s.0 == ms).unwrap().2;
        assert!(level(0) < 0.001 && level(75) > 0. && level(75) < 1., "fades in");
        assert!(level(175) == 1. && !moving(175), "lands, frames stop");
        assert!(level(1000) == 1. && !moving(1000), "holds");
        let leaving = level(1250);
        assert!(leaving > 0. && leaving < 1., "first copy leaving at 1250: {leaving}");
        assert!(level(1275) > leaving, "second copy turns around from where it is drawn");
        assert!(level(1250 + 175) == 1., "back in");
        assert_eq!(COPIED_FOR, Duration::from_millis(1200));
        assert!(level(1600) == 1. && !moving(1600), "holds again, no frames");
    }

    fn click(button: MouseButton) -> ClickEvent {
        let position = point(px(1.), px(1.));
        ClickEvent::Mouse(MouseClickEvent {
            down: MouseDownEvent { button, position, modifiers: Modifiers::default(), click_count: 1, first_mouse: false },
            up: MouseUpEvent { button, position, modifiers: Modifiers::default(), click_count: 1 },
        })
    }

    /// Each row as (title, picked command, greyed out); a separator is `None`.
    fn rows(selected: bool, link: Option<&str>) -> Vec<Option<(String, PreviewCommand, bool)>> {
        menu_entries(selected, link)
            .into_iter()
            .map(|entry| match entry {
                Entry::Item(item) => Some((item.title, item.run, item.disabled)),
                _ => None,
            })
            .collect()
    }

    fn row(title: &str, run: PreviewCommand, disabled: bool) -> Option<(String, PreviewCommand, bool)> {
        Some((title.to_owned(), run, disabled))
    }

    #[test]
    fn menu_with_a_selection_enables_copy() {
        assert_eq!(
            rows(true, None),
            [
                row("Copy", PreviewCommand::Copy, false),
                row("Copy as Markdown", PreviewCommand::CopyMarkdown, false),
                row("Copy as Rich Text", PreviewCommand::CopyRichText, false),
                None,
                row("Select all", PreviewCommand::SelectAll, false),
            ]
        );
    }

    #[test]
    fn menu_without_a_selection_greys_copy_but_not_select_all() {
        assert_eq!(
            rows(false, None),
            [
                row("Copy", PreviewCommand::Copy, true),
                row("Copy as Markdown", PreviewCommand::CopyMarkdown, true),
                row("Copy as Rich Text", PreviewCommand::CopyRichText, false),
                None,
                row("Select all", PreviewCommand::SelectAll, false),
            ]
        );
    }

    #[test]
    fn menu_on_a_link_leads_with_its_rows() {
        let url = "https://example.com";
        assert_eq!(
            rows(false, Some(url))[..3],
            [
                row("Open link", PreviewCommand::OpenLink(url.into()), false),
                row("Copy link address", PreviewCommand::CopyLink(url.into()), false),
                None,
            ]
        );
        assert_eq!(rows(false, Some(url)).len(), 8);
    }

    #[test]
    fn only_a_left_click_opens_a_link() {
        assert!(opens(&click(MouseButton::Left)));
        assert!(!opens(&click(MouseButton::Right)));
        assert!(!opens(&click(MouseButton::Middle)));
    }

    fn image(_: (usize, u64)) -> Made {
        Made { slot: Slot::Encoded(Arc::new(Image::from_bytes(ImageFormat::Png, vec![1]))), job: None }
    }

    const A: u64 = 1;
    const B: u64 = 2;

    #[test]
    fn embedded_cache_hits_do_not_insert() {
        let mut cache = EmbeddedImages::default();
        cache.get_or_insert_with(A, "data:0", image);
        let mut made = false;
        cache.get_or_insert_with(A, "data:0", |key| {
            made = true;
            image(key)
        });
        assert!(!made, "a repeat lookup must not make or insert");
        assert_eq!(cache.entries.len(), 1);
    }

    #[test]
    fn pending_images_queue_one_job_each_and_a_late_result_of_an_evicted_one_is_freed() {
        let mut cache = EmbeddedImages::default();
        cache.begin_frame(A);
        let job = |key| Made { slot: Slot::Pending, job: Some(Job { key, source: super::Source::Bytes { format: image::ImageFormat::Png, bytes: vec![1] } }) };
        cache.get_or_insert_with(A, "data:a", job);
        cache.get_or_insert_with(A, "data:a", job);
        cache.get_or_insert_with(A, "data:b", job);
        assert_eq!(cache.queue.len(), 2, "a repeat lookup queues nothing");
        let a = super::url_key("data:a");
        cache.finish(a, Slot::Failed);
        assert!(matches!(cache.entries[0].slot, Slot::Failed));
        cache.release(A, false);
        cache.finish(super::url_key("data:b"), Slot::Failed);
        assert!(cache.queue.is_empty() && cache.entries.is_empty());
        assert_eq!(cache.evicted.len(), 2, "the two cleared entries; the late result is dropped");
    }

    #[test]
    fn a_changed_file_keeps_its_old_version_until_the_new_one_lands() {
        let mut cache = EmbeddedImages::default();
        cache.begin_frame(A);
        let file = |key| Made { slot: Slot::Pending, job: Some(Job { key, source: super::Source::File(std::path::Path::new("/tmp/a.png").into()) }) };
        let done = || Slot::Encoded(Arc::new(Image::from_bytes(ImageFormat::Png, vec![1])));
        cache.get_or_insert_key(A, (1, 1), file);
        cache.get_or_insert_key(A, (1, 1), file);
        assert_eq!((cache.entries.len(), cache.queue.len()), (1, 1), "the same stamp is a hit");
        cache.queue.clear();
        cache.finish((1, 1), done());
        cache.get_or_insert_key(A, (1, 2), file);
        assert!(cache.shown_version(A, std::path::Path::new("/tmp/a.png")).is_some(), "the old version shows meanwhile");
        assert_eq!(cache.entries.len(), 2);
        cache.get_or_insert_key(A, (1, 3), file);
        assert_eq!((cache.entries.len(), cache.queue.len()), (2, 1), "the version still queued is dropped");
        cache.finish((1, 3), done());
        assert_eq!(cache.entries.len(), 1, "the new version replaced the old");
        assert_eq!(cache.entries[0].key, (1, 3));
        assert_eq!(cache.evicted.len(), 1);
    }

    #[test]
    fn embedded_cache_keeps_claimed_images_over_the_cap_and_drops_old_ones() {
        let mut cache = EmbeddedImages::default();
        cache.begin_frame(A);
        for n in 0..EMBEDDED_CAP + 8 {
            cache.get_or_insert_with(A, &format!("data:{n}"), image);
        }
        assert_eq!(cache.entries.len(), EMBEDDED_CAP + 8, "all on screen, none evicted");
        assert!(cache.evicted.is_empty());

        // Scrolled away: two frames that ask for other images leave the first batch unclaimed.
        cache.begin_frame(A);
        cache.begin_frame(A);
        cache.get_or_insert_with(A, "data:new", image);
        assert_eq!(cache.entries.len(), EMBEDDED_CAP);
        assert_eq!(cache.evicted.len(), 9);
        assert!(cache.entries.iter().all(|entry| entry.key != super::url_key("data:0")));
        assert!(cache.entries.iter().any(|entry| entry.key == super::url_key("data:new")));
        cache.release(A, false);
        assert!(cache.entries.is_empty() && cache.bytes == 0);
        assert_eq!(cache.evicted.len(), 9 + EMBEDDED_CAP);
    }

    fn pixels(width: u32, height: u32) -> Slot {
        Slot::Pixels(Arc::new(RenderImage::new(vec![image::Frame::new(image::RgbaImage::new(width, height))])))
    }

    /// finch#my2c: what a Mermaid block holds after 20 edits, a picture per edit before and one now.
    #[test]
    fn an_edited_diagram_frees_its_old_picture_once_the_new_one_draws() {
        let held = |release: bool| {
            let mut cache = EmbeddedImages::default();
            let mut old: Option<(usize, u64)> = None;
            for n in 0..21u64 {
                let key = (0xD1A6, n);
                // The frame that misses: the old picture keeps drawing meanwhile.
                cache.begin_frame(A);
                cache.get_or_insert_key(A, key, |_| Made { slot: Slot::Pending, job: None });
                if let Some(old) = old {
                    cache.get_or_insert_key(A, old, |_| Made { slot: Slot::Pending, job: None });
                }
                cache.finish(key, pixels(1000, 600));
                // The frame the new one draws.
                cache.begin_frame(A);
                cache.get_or_insert_key(A, key, |_| Made { slot: Slot::Pending, job: None });
                if let (true, Some(old)) = (release, old) {
                    cache.release_key(A, old);
                }
                old = Some(key);
            }
            (cache.entries.len(), cache.bytes)
        };
        let (before, after) = (held(false), held(true));
        eprintln!("after 20 edits: before {} pictures {} bytes; after {} pictures {} bytes", before.0, before.1, after.0, after.1);
        assert_eq!(after, (1, 1000 * 600 * 4));
        assert!(before.0 > after.0);
    }

    #[test]
    fn a_replaced_picture_another_window_draws_stays() {
        let mut cache = EmbeddedImages::default();
        for owner in [A, B] {
            cache.begin_frame(owner);
        }
        let key = (0xD1A6, 1);
        for owner in [A, B] {
            cache.get_or_insert_key(owner, key, |_| Made { slot: pixels(10, 10), job: None });
        }
        cache.begin_frame(A);
        cache.begin_frame(B);
        cache.release_key(A, key);
        assert_eq!(cache.entries.len(), 1, "B drew it in its latest frame");
        cache.begin_frame(B);
        cache.begin_frame(B);
        cache.release_key(A, key);
        assert!(cache.entries.is_empty());
    }

    /// finch#rhky: scrolling a file of 40 formulas and 5 big pictures (every ninth item), ten items
    /// in view, five at a time.
    #[test]
    fn far_big_pictures_go_before_small_formulas_near_the_view() {
        let mut cache = EmbeddedImages::default();
        let big = |i: u64| i % 9 == 0;
        for start in (0..=35u64).step_by(5) {
            cache.begin_frame(A);
            for i in start..start + 10 {
                cache.get_or_insert_key(A, (7, i), |_| Made { slot: if big(i) { pixels(4000, 1000) } else { pixels(100, 50) }, job: None });
            }
        }
        let mut kept: Vec<u64> = cache.entries.iter().map(|entry| entry.key.1).collect();
        kept.sort_unstable();
        eprintln!("kept after scrolling: {} entries {} bytes: {kept:?}", kept.len(), cache.bytes);
        assert!(kept.len() <= EMBEDDED_CAP && cache.bytes <= super::EMBEDDED_BYTES_CAP);
        assert!(kept.iter().all(|&i| !big(i) || i >= 35), "the far big pictures went: {kept:?}");
        assert!((20..45).filter(|&i| !big(i)).all(|i| kept.contains(&i)), "the formulas near the view stayed: {kept:?}");
    }

    /// finch#swn7: typing a 30 character picture path a character at a time.
    #[test]
    fn typing_a_picture_path_stats_once_the_typing_pauses_and_forgets_the_partial_urls() {
        let full = "images/a-rather-long-name-1.png";
        let mut cache = EmbeddedImages::default();
        let mut stats = 0;
        for n in 1..=30 {
            cache.begin_frame(A);
            let path: Arc<std::path::Path> = std::path::Path::new("/tmp").join(&full[..n]).into();
            let base = super::url_key(&path.to_string_lossy());
            stats += usize::from(cache.add_file(A, base, path, base));
        }
        let last = super::url_key(&std::path::Path::new("/tmp").join(&full[..30]).to_string_lossy());
        assert!(cache.settle_file(last).is_some(), "the pause stats the path that is left");
        stats += 1;
        eprintln!("typing 30 characters: {stats} stats before 30; {} entries kept", cache.files.len());
        assert_eq!(stats, 3);
        assert!(cache.files.len() <= 3);

        // The link deleted: two frames that draw nothing of it.
        cache.begin_frame(A);
        cache.begin_frame(A);
        assert!(cache.files.is_empty(), "nothing is kept for text that is gone");

        // A picture still cached keeps its entry.
        let path: Arc<std::path::Path> = std::path::Path::new("/tmp/keep.png").into();
        let base = super::url_key("/tmp/keep.png");
        cache.add_file(A, base, path.clone(), base);
        cache.get_or_insert_key(A, (1, 1), |key| Made { slot: Slot::Pending, job: Some(Job { key, source: super::Source::File(path.clone()) }) });
        cache.begin_frame(A);
        cache.begin_frame(A);
        assert_eq!(cache.files.len(), 1);

    }

    #[test]
    fn a_release_frees_only_what_no_other_window_shows() {
        let mut cache = EmbeddedImages::default();
        for owner in [A, B] {
            cache.begin_frame(owner);
        }
        cache.get_or_insert_with(A, "data:shared", image);
        cache.get_or_insert_with(B, "data:shared", image);
        cache.get_or_insert_with(A, "data:mine", image);
        assert_eq!(cache.entries.len(), 2);

        cache.release(A, false);
        assert_eq!(cache.entries.len(), 1, "the one B shows stays");
        assert_eq!(cache.entries[0].key, super::url_key("data:shared"));
        assert_eq!(cache.evicted.len(), 1);

        // A's own frames don't age B's claim: the shared one stays while B drew it last.
        cache.begin_frame(A);
        cache.release(A, true);
        assert_eq!(cache.entries.len(), 1);

        // B stops drawing it for two frames: A's release of unseen now frees it.
        cache.begin_frame(B);
        cache.begin_frame(B);
        cache.release(A, true);
        assert!(cache.entries.is_empty());

        // B closes: its frame count goes.
        cache.forget_owner(B);
        assert!(cache.entries.is_empty() && cache.frames.get(&B).is_none());
        assert_eq!(cache.evicted.len(), 2);
    }

    #[test]
    fn huge_images_are_refused_before_decoding() {
        assert!(super::within_limits(16384, 4096));
        assert!(!super::within_limits(16385, 10));
        assert!(!super::within_limits(20000, 20000));
        assert!(!super::within_limits(65535, 65535));
        assert!(!super::within_limits(16384, 16384));
    }

    #[test]
    fn decoded_bytes_reads_png_and_gif_headers() {
        let mut png = vec![0x89, b'P', b'N', b'G', 13, 10, 26, 10, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        png.extend([0, 0, 0, 10, 0, 0, 0, 20]);
        assert_eq!(decoded_bytes(&Image::from_bytes(ImageFormat::Png, png)), 10 * 20 * 4);
        let gif = [b"GIF89a".as_slice(), &[3, 0, 4, 0]].concat();
        assert_eq!(decoded_bytes(&Image::from_bytes(ImageFormat::Gif, gif)), 3 * 4 * 4);
    }

    /// A folder with `README.md`, `docs/guide.md`, `docs/my notes.md`,
    /// `docs/data.csv` and `up.md` one level above `docs`.
    fn scratch() -> PathBuf {
        let root = std::env::temp_dir().join(format!("finch-render-{}", std::process::id()));
        std::fs::create_dir_all(root.join("docs/sub")).unwrap();
        for name in [
            "README.md",
            "docs/guide.md",
            "docs/my notes.md",
            "docs/data.csv",
            "up.md",
        ] {
            std::fs::write(root.join(name), "x").unwrap();
        }
        root
    }

    #[test]
    fn every_markdown_name_opens_in_editor() {
        let root = scratch();
        for name in ["a.mdx", "b.mdwn", "c.mdc", "B.MD", "d.Markdown"] {
            std::fs::write(root.join(name), "x").unwrap();
            assert_eq!(resolve_link(name, Some(&root)), LinkTarget::Markdown(root.join(name), None), "{name}");
        }
    }

    #[test]
    fn the_pill_names_where_a_link_goes() {
        let root = scratch();
        let dir = root.join("docs");
        assert_eq!(super::pill_label("https://example.com/a", Some(&dir)), "https://example.com/a");
        assert_eq!(super::pill_label("#what%27s-new", Some(&dir)), "#what's-new");
        assert_eq!(super::pill_label("./my%20notes.md", Some(&dir)), "my notes.md");
        assert_eq!(super::pill_label("../README.md#Install", Some(&dir)), "README.md#Install");
        let long = format!("https://example.com/{}/index.html", "a".repeat(80));
        let cut = super::pill_label(&long, Some(&dir));
        assert_eq!(cut.chars().count(), crate::tokens::LINK_PILL_MAX_CHARS);
        assert!(cut.starts_with("https://") && cut.contains('\u{2026}') && cut.ends_with(".html"));
    }

    #[test]
    fn relative_markdown_opens_in_editor() {
        let root = scratch();
        let dir = root.join("docs");
        assert_eq!(
            resolve_link("guide.md", Some(&dir)),
            LinkTarget::Markdown(dir.join("guide.md"), None)
        );
        assert_eq!(
            resolve_link("./my%20notes.md", Some(&dir)),
            LinkTarget::Markdown(dir.join("./my notes.md"), None)
        );
    }

    #[test]
    fn parent_folder_link_resolves() {
        let root = scratch();
        let dir = root.join("docs/sub");
        assert_eq!(
            resolve_link("../../up.md", Some(&dir)),
            LinkTarget::Markdown(dir.join("../../up.md"), None)
        );
    }

    #[test]
    fn fragment_on_a_markdown_file_travels_with_it() {
        let root = scratch();
        let target = root.join("README.md");
        let url = format!("file://{}#Install", target.display());
        assert_eq!(
            resolve_link(&url, None),
            LinkTarget::Markdown(target.clone(), Some("install".to_owned()))
        );
        assert_eq!(
            resolve_link("guide.md#usage", Some(&root.join("docs"))),
            LinkTarget::Markdown(root.join("docs/guide.md"), Some("usage".to_owned()))
        );
        assert_eq!(
            resolve_link("guide.md#caf%C3%A9", Some(&root.join("docs"))),
            LinkTarget::Markdown(root.join("docs/guide.md"), Some("café".to_owned()))
        );
    }

    #[test]
    fn empty_fragment_is_no_anchor() {
        let root = scratch();
        assert_eq!(
            resolve_link("guide.md#", Some(&root.join("docs"))),
            LinkTarget::Markdown(root.join("docs/guide.md"), None)
        );
        assert_eq!(resolve_link("#", Some(&root)), LinkTarget::Nothing);
    }

    #[test]
    fn heading_only_link_jumps_in_this_file() {
        let root = scratch();
        assert_eq!(resolve_link("#install", Some(&root)), LinkTarget::Heading("install".to_owned()));
        assert_eq!(resolve_link("#Getting-Started", None), LinkTarget::Heading("getting-started".to_owned()));
    }

    #[test]
    fn web_links_go_to_the_system() {
        let root = scratch();
        for url in [
            "https://example.com/a#b",
            "http://example.com",
            "mailto:a@b.c",
        ] {
            assert_eq!(
                resolve_link(url, Some(&root)),
                LinkTarget::Web(url.to_owned())
            );
        }
        assert_eq!(
            resolve_link("javascript:alert(1)", Some(&root)),
            LinkTarget::Nothing
        );
    }

    #[test]
    fn other_local_file_opens_with_system_app() {
        let root = scratch();
        let dir = root.join("docs");
        assert_eq!(
            resolve_link("data.csv", Some(&dir)),
            LinkTarget::Other(dir.join("data.csv"))
        );
    }

    #[test]
    fn missing_file_does_nothing() {
        let root = scratch();
        assert_eq!(resolve_link("nope.md", Some(&root)), LinkTarget::Nothing);
        assert_eq!(resolve_link("guide.md", None), LinkTarget::Nothing);
    }
}
