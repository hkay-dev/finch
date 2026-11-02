//! The explorer's icons: Finder's own file and folder icons, SF Symbols for the kinds Finder draws
//! as one plain page (Markdown, JSON, config, lock and text files), and the disclosure chevron.
//!
//! `NSWorkspace` hands out the icon Finder shows for a file type; it is drawn into a bitmap on the
//! background executor and handed to the renderer as is, one per file extension and one for folders
//! (Finder has no open variant, so a folder is the same icon open or closed). A symbol is drawn the
//! same way, gray, under a type key that starts with `sf:`. Nothing draws on the
//! render path: a type not yet drawn shows an empty slot, is asked for once, and its icon fades in
//! when it lands. `init` pre-warms the folder and the common types at launch.
//! The chevron is a small stroked path, a semibold chevron the way a source list draws it.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    ffi::CString,
    path::Path,
    ptr::null_mut,
    sync::Arc,
    time::{Duration, Instant},
};

use std::hash::{Hash as _, Hasher as _};

use gpui_kit::component::Icon;
use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, AsyncApp, ElementId, Hsla, ImageSource, IntoElement as _,
    ParentElement as _, RenderImage, Rgba, Styled as _, div, img, px,
};
use image::{Frame, ImageBuffer};
use objc2::{
    ClassType, class, define_class, msg_send,
    rc::{Allocated, Retained},
    runtime::{AnyObject, NSObject},
    sel,
};

use crate::tokens::{ICON, ICON_GIT_TINT};
use crate::window_look::{Point, Rect, Size};

/// Pixels per point of an icon's bitmap, so a 2x display shows it one to one.
const PIXELS_PER_POINT: f64 = 2.;

/// How long a freshly drawn icon takes to fade in.
const FADE: Duration = Duration::from_millis(150);

/// The type identifier of a folder; no extension can equal it, since extensions hold no dot.
const FOLDER: &str = "public.folder";

/// Kinds Finder draws as one plain page, which wear an SF Symbol in the tree instead, so they read
/// apart (finch#r2y0). A type key is `SYMBOL_PREFIX` and the symbol's name.
const SYMBOL_PREFIX: &str = "sf:";
const MARKDOWN: &str = "sf:doc.richtext";
const JSON: &str = "sf:curlybraces";
const CONFIG: &str = "sf:gearshape";
const LOCK: &str = "sf:shippingbox";
const TEXT: &str = "sf:doc.plaintext";

/// How much of the icon square a symbol fills, its point size, and its weight (`NSFontWeightRegular`).
const SYMBOL_FILL: f64 = 0.9;
const SYMBOL_WEIGHT: f64 = 0.0;
/// Joins a symbol's type key to the colour it is drawn in (`sf:doc.richtext@e8e8e8`): the bitmap
/// carries the theme's ink at full strength and the row fades it to the ink's own alpha, so a
/// symbol reads in the text colour of either theme (finch#td09), not one grey for both.
const INK_SEP: char = '@';
/// Joins a type key to the git colour its icon wears (`rs~ff9f0a`): the bitmap is Finder's icon moved
/// toward that colour, drawn once per type and colour.
const TINT_SEP: char = '~';

/// Types drawn at launch: a folder's usual contents. The symbols aren't here: their colour is the
/// theme's, unknown until the first row draws.
const COMMON: &[&str] = &[
    "rs", "yml", "yaml", "swift", "js", "ts", "tsx", "py", "sh", "html", "css",
    "png", "jpg", "pdf", "log",
];

/// How many file types beyond `COMMON` keep a bitmap; past it the least recently drawn one is
/// dropped, and loads again if its type shows up once more.
const UNCOMMON_MAX: usize = 256;

/// One file type's icon: asked for and not back yet, or back (`None` when Finder gave nothing).
#[derive(Clone)]
enum Slot {
    Pending,
    Ready(Option<Arc<RenderImage>>, Instant),
}

#[derive(Default)]
struct Cache {
    /// Set by `init`; without it nothing is ever drawn.
    app: Option<AsyncApp>,
    /// Each file type asked for (lowercase extension, empty for none, or `FOLDER`).
    slots: HashMap<String, Slot>,
    /// Types a render found pending, so a landing one repaints the windows.
    wanted: HashSet<String>,
    /// A repaint is already scheduled for this tick.
    repaint_set: bool,
    /// Each uncommon type in `slots` (not `FOLDER`, not in `COMMON`) with the tick it was last drawn.
    used: HashMap<String, u64>,
    /// Counts draws of an icon, for `used`.
    tick: u64,
}

impl Cache {
    /// Marks an uncommon type as just drawn; common types and the symbols (a few, in the plain ink and
    /// four git tints per theme) are never evicted, so cost nothing. A type in a git tint counts as uncommon.
    fn touch(&mut self, file_type: &str) {
        self.tick += 1;
        if let Some(at) = self.used.get_mut(file_type) {
            *at = self.tick;
        } else if file_type != FOLDER && !COMMON.contains(&file_type) && !file_type.starts_with(SYMBOL_PREFIX) {
            self.used.insert(file_type.to_string(), self.tick);
        }
    }

    /// Stores a landed icon. Returns whether to repaint the windows, and the images of the least
    /// recently drawn uncommon types dropped to stay within `UNCOMMON_MAX`.
    fn land(&mut self, file_type: &str, image: Option<Arc<RenderImage>>) -> (bool, Vec<Arc<RenderImage>>) {
        // A tinted icon lands dated past `FADE`: it takes the place of the untinted one `picture` showed meanwhile.
        let at = if split_tint(file_type).1.is_some() { Instant::now().checked_sub(FADE * 2).unwrap_or_else(Instant::now) } else { Instant::now() };
        self.slots.insert(file_type.to_string(), Slot::Ready(image, at));
        self.touch(file_type);
        let mut evicted = Vec::new();
        while self.used.len() > UNCOMMON_MAX {
            let oldest = self
                .used
                .iter()
                .filter(|(ty, _)| matches!(self.slots.get(*ty), Some(Slot::Ready(..))))
                .min_by_key(|(_, at)| **at)
                .map(|(ty, _)| ty.clone());
            let Some(oldest) = oldest else { break };
            self.used.remove(&oldest);
            if let Some(Slot::Ready(Some(image), _)) = self.slots.remove(&oldest) {
                evicted.push(image);
            }
        }
        // Repaint only for a type a render saw pending, and once for all that land in the same tick.
        let repaint = self.wanted.remove(file_type) && !std::mem::replace(&mut self.repaint_set, true);
        (repaint, evicted)
    }
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
}

/// Starts drawing the folder icon and the common file types in the background, and watches the
/// system accent.
pub fn init(cx: &mut App) {
    cx.set_global(AccentTick(0));
    CACHE.with_borrow_mut(|cache| {
        cache.app = Some(cx.to_async());
        request(cache, FOLDER, f32::from(ICON));
        for ext in COMMON {
            request(cache, ext, f32::from(ICON));
        }
    });
    watch_accent();
}

/// Counts the Mac's accent changes. The drop wash on a folder row is drawn from the accent on every
/// render, so each explorer observes this and renders again when it ticks.
pub struct AccentTick(pub u64);

impl gpui_kit::Global for AccentTick {}

thread_local! {
    /// An accent change is already queued to tick, so the two notifications one change posts tick once.
    static ACCENT_QUEUED: RefCell<bool> = const { RefCell::new(false) };
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements and the class has no ivars.
    #[unsafe(super(NSObject))]
    #[name = "GPUIEditorAccentWatcher"]
    struct AccentWatcher;

    impl AccentWatcher {
        #[unsafe(method(changed:))]
        fn changed(&self, _notification: &AnyObject) {
            accent_changed();
        }
    }
);

/// Called on the main thread from the run loop: ticks `AccentTick` from a foreground task, never
/// inside whatever update the post came from.
fn accent_changed() {
    if ACCENT_QUEUED.replace(true) {
        return;
    }
    let Some(app) = CACHE.with_borrow(|cache| cache.app.clone()) else { return };
    app.spawn(async move |cx| {
        ACCENT_QUEUED.replace(false);
        cx.update(|cx| cx.global_mut::<AccentTick>().0 += 1);
    })
    .detach();
}

/// Listens for the system accent changing: AppKit's in-process notification and the system-wide
/// one the Appearance settings post. Both go to one watcher that lives as long as the app.
fn watch_accent() {
    // SAFETY: plain Foundation calls on the main thread; the watcher is leaked, as the centres
    // hold their observers unretained.
    unsafe {
        let watcher: Retained<AnyObject> = msg_send![AccentWatcher::class(), new];
        let local: Retained<AnyObject> = msg_send![class!(NSNotificationCenter), defaultCenter];
        let distributed: Retained<AnyObject> = msg_send![class!(NSDistributedNotificationCenter), defaultCenter];
        let none: *mut AnyObject = null_mut();
        if let Some(name) = nsstring("NSSystemColorsDidChangeNotification") {
            let _: () = msg_send![&*local, addObserver: &*watcher, selector: sel!(changed:), name: &*name, object: none];
        }
        if let Some(name) = nsstring("AppleColorPreferencesChangedNotification") {
            let _: () = msg_send![&*distributed, addObserver: &*watcher, selector: sel!(changed:), name: &*name, object: none];
        }
        std::mem::forget(watcher);
    }
}

/// Draws `file_type` on the background executor, then stores it and repaints the windows.
fn request(cache: &mut Cache, file_type: &str, size: f32) {
    let Some(app) = cache.app.clone() else { return };
    if cache.slots.contains_key(file_type) {
        return;
    }
    cache.slots.insert(file_type.to_string(), Slot::Pending);
    cache.touch(file_type);
    let file_type = file_type.to_string();
    let task = app.background_executor().spawn({
        let file_type = file_type.clone();
        async move { draw(&file_type, size) }
    });
    app.spawn(async move |cx| {
        let image = task.await;
        let (repaint, evicted) = CACHE.with_borrow_mut(|cache| cache.land(&file_type, image));
        if !evicted.is_empty() {
            // Release the evicted bitmaps' textures too, or the atlas keeps them.
            cx.update(|cx| {
                for image in evicted {
                    cx.drop_image(image, None);
                }
            });
        }
        if repaint {
            cx.background_executor().timer(Duration::from_millis(16)).await;
            CACHE.with_borrow_mut(|cache| cache.repaint_set = false);
            cx.update(|cx| cx.refresh_windows());
        }
    })
    .detach();
}

/// Finder's icon for a folder, `size` points square.
pub fn folder(size: f32) -> AnyElement {
    picture(FOLDER, size)
}

/// `folder` wearing git's `tint`: the same icon moved `ICON_GIT_TINT` of the way to the colour.
pub fn folder_tinted(size: f32, tint: Hsla) -> AnyElement {
    picture(&tint_key(FOLDER, tint), size)
}

/// `file` wearing git's `tint`. A symbol is drawn in the tint itself, at its alpha; Finder's icon is moved
/// `ICON_GIT_TINT` of the way to the colour, so its shape and shading stay.
pub fn file_tinted(name: &str, size: f32, tint: Hsla) -> AnyElement {
    let kind = file_type(name);
    if kind.starts_with(SYMBOL_PREFIX) {
        return file(name, size, tint);
    }
    picture(&tint_key(&kind, tint), size)
}

/// Whether the icon for `name` is an SF Symbol (see-through at its ink's alpha) and not a Finder bitmap.
pub fn is_symbol(name: &str) -> bool {
    file_type(name).starts_with(SYMBOL_PREFIX)
}

/// The type key of `kind` wearing `tint`: `kind`, `~`, and the tint as six hex digits of sRGB.
fn tint_key(kind: &str, tint: Hsla) -> String {
    let Rgba { r, g, b, .. } = Rgba::from(tint);
    let byte = |level: f32| (level * 255.).round() as u8;
    format!("{kind}{TINT_SEP}{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b))
}

/// The icon for a file, picked by its name, `size` points square: Finder's for its extension, or an
/// SF Symbol for the kinds `file_type` names one for, drawn in `ink` (the theme's text ink at the
/// strength it should show; its alpha is applied here, its colour is baked into the bitmap).
pub fn file(name: &str, size: f32, ink: Hsla) -> AnyElement {
    let kind = file_type(name);
    if !kind.starts_with(SYMBOL_PREFIX) {
        return picture(&kind, size);
    }
    let Rgba { r, g, b, .. } = Rgba::from(ink);
    let byte = |level: f32| (level * 255.).round() as u8;
    let key = format!("{kind}{INK_SEP}{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b));
    div().flex_none().opacity(ink.a).child(picture(&key, size)).into_any_element()
}

/// The type whose icon a file wears: a symbol for Markdown, JSON, config, lock and plain text files
/// (Finder draws them all as one page), else its extension. Dotfiles and names with no extension,
/// which Finder draws as a blank or "?" page, wear the plain text symbol.
fn file_type(name: &str) -> String {
    // Path gives a dotfile like `.gitignore` no extension, so it falls to the plain icon too.
    let ext = Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let name = name.to_ascii_lowercase();
    let symbol = match ext.as_str() {
        _ if name == "package-lock.json" => LOCK,
        "lock" => LOCK,
        "md" | "markdown" | "mdx" => MARKDOWN,
        "json" | "jsonc" | "json5" => JSON,
        "toml" | "ini" | "cfg" | "conf" => CONFIG,
        "txt" | "" => TEXT,
        _ if name.starts_with(".env") => TEXT,
        _ => return ext,
    };
    symbol.to_string()
}

/// The icon as an element `size` square; one still drawing, or that Finder gave nothing for,
/// keeps its empty slot. A symbol (`INK_SEP` in its key) is drawn right here on its first lookup,
/// in either theme's ink: it is a 20 pt glyph, not Finder's icon lookup, and drawing it now means
/// the first frame already has it, with no fade-in and no empty slot on a theme change (finch#td09).
fn picture(file_type: &str, size: f32) -> AnyElement {
    let slot = CACHE.with_borrow_mut(|cache| {
        if !cache.slots.contains_key(file_type) {
            if file_type.contains(INK_SEP) {
                // Dated past `FADE`, so it shows at full strength at once.
                let drawn = Instant::now().checked_sub(FADE * 2).unwrap_or_else(Instant::now);
                cache.slots.insert(file_type.to_string(), Slot::Ready(draw(file_type, size), drawn));
            } else {
                request(cache, file_type, size);
            }
        }
        cache.touch(file_type);
        let slot = cache.slots.get(file_type).cloned();
        if matches!(slot, Some(Slot::Pending)) {
            cache.wanted.insert(file_type.to_string());
        }
        slot
    });
    match slot {
        Some(Slot::Ready(Some(image), at)) => {
            let icon = img(ImageSource::Render(image)).size(px(size)).flex_none();
            if at.elapsed() < FADE {
                // One animation id per drawn icon, so every row showing it fades on the same clock.
                let mut hasher = std::hash::DefaultHasher::new();
                at.hash(&mut hasher);
                icon.with_animation(
                    ElementId::NamedInteger("finder-icon-in".into(), hasher.finish()),
                    Animation::new(FADE),
                    |icon, t| icon.opacity(crate::motion::ease_out(t)),
                )
                .into_any_element()
            } else {
                icon.into_any_element()
            }
        }
        // A tinted type not drawn yet shows the untinted icon, so its landing is a recolour, not a fade from nothing.
        _ => match split_tint(file_type) {
            (kind, Some(_)) => picture(kind, size),
            _ => div().size(px(size)).flex_none().into_any_element(),
        },
    }
}

/// The disclosure chevron of a folder row: 9 pt, semibold, tinted by the text color. It is drawn
/// pointing right and turned `turn` of the quarter turn to pointing down, so it turns with its fold.
pub fn chevron(turn: f32) -> Icon {
    let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\" viewBox=\"0 0 24 24\" \
         fill=\"none\" stroke=\"black\" stroke-width=\"3.2\" stroke-linecap=\"round\" \
         stroke-linejoin=\"round\"><path d=\"m9 6 6 6-6 6\"/></svg>"; // design: SVG markup, not screen text
    Icon::default().data(svg.as_bytes()).rotate(gpui_kit::radians(std::f32::consts::FRAC_PI_2 * turn))
}

/// The system accent color, for the drop wash on a folder row.
pub fn accent() -> Hsla {
    unsafe {
        let color: Option<Retained<AnyObject>> = msg_send![class!(NSColor), controlAccentColor];
        let converted: Option<Retained<AnyObject>> = color.and_then(|color| {
            let space: Retained<AnyObject> = msg_send![class!(NSColorSpace), sRGBColorSpace];
            msg_send![&*color, colorUsingColorSpace: &*space]
        });
        let Some(color) = converted else {
            return Hsla { h: 0.6, s: 0.8, l: 0.55, a: 1. };
        };
        let (mut r, mut g, mut b, mut a) = (0f64, 0f64, 0f64, 0f64);
        let _: () = msg_send![&*color, getRed: &mut r, green: &mut g, blue: &mut b, alpha: &mut a];
        Rgba { r: r as f32, g: g as f32, b: b as f32, a: a as f32 }.into()
    }
}

fn nsstring(text: &str) -> Option<Retained<AnyObject>> {
    let text = CString::new(text).ok()?;
    unsafe { msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()] }
}

/// The SF Symbol `name` as an image of colour `rgb` (sRGB, 0 to 1) sized for a `size` point square,
/// or `None` when the system has no such symbol.
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn symbol_image(name: &str, rgb: [f64; 3], size: f64) -> Option<Retained<AnyObject>> {
    let name = nsstring(name)?;
    let none: *mut AnyObject = null_mut();
    let image: Option<Retained<AnyObject>> =
        msg_send![class!(NSImage), imageWithSystemSymbolName: &*name, accessibilityDescription: none];
    let image = image?;
    let shape: Retained<AnyObject> = msg_send![
        class!(NSImageSymbolConfiguration),
        configurationWithPointSize: size * SYMBOL_FILL,
        weight: SYMBOL_WEIGHT
    ];
    let gray: Retained<AnyObject> = msg_send![class!(NSColor), colorWithSRGBRed: rgb[0], green: rgb[1], blue: rgb[2], alpha: 1.0f64];
    let tint: Retained<AnyObject> = msg_send![class!(NSImageSymbolConfiguration), configurationWithHierarchicalColor: &*gray];
    let both: Retained<AnyObject> = msg_send![&*shape, configurationByApplyingConfiguration: &*tint];
    msg_send![&*image, imageWithSymbolConfiguration: &*both]
}

/// Draws Finder's icon for `key` (an extension or a type identifier, then `~` and a tint's six hex digits
/// when it wears git's colour) `size` points square
/// as an image of `PIXELS_PER_POINT` pixels per point. Runs on a background thread, never on the
/// render path.
fn draw(key: &str, size: f32) -> Option<Arc<RenderImage>> {
    let (kind, tint) = split_tint(key);
    #[allow(unsafe_op_in_unsafe_fn)]
    unsafe fn run(file_type: &str, size: f64) -> Option<(u32, Vec<u8>)> {
        let symbol = file_type.starts_with(SYMBOL_PREFIX);
        let image: Option<Retained<AnyObject>> = if let Some(name) = file_type.strip_prefix(SYMBOL_PREFIX) {
            let (name, hex) = name.split_once(INK_SEP)?;
            let level = |at: usize| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok().map(|byte| f64::from(byte) / 255.);
            symbol_image(name, [level(0)?, level(2)?, level(4)?], size)
        } else {
            let kind = nsstring(file_type)?;
            let workspace: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
            msg_send![&*workspace, iconForFileType: &*kind]
        };
        let image = image?;
        let pixels = (size * PIXELS_PER_POINT).ceil() as isize;

        let alloc: Allocated<AnyObject> = msg_send![class!(NSBitmapImageRep), alloc];
        let rep: Option<Retained<AnyObject>> = msg_send![
            alloc,
            initWithBitmapDataPlanes: null_mut::<*mut u8>(),
            pixelsWide: pixels,
            pixelsHigh: pixels,
            bitsPerSample: 8isize,
            samplesPerPixel: 4isize,
            hasAlpha: true,
            isPlanar: false,
            colorSpaceName: &*nsstring("NSCalibratedRGBColorSpace")?,
            bytesPerRow: 0isize,
            bitsPerPixel: 0isize,
        ];
        let rep = rep?;
        let _: () = msg_send![&*rep, setSize: Size { width: size, height: size }];
        let context: Option<Retained<AnyObject>> =
            msg_send![class!(NSGraphicsContext), graphicsContextWithBitmapImageRep: &*rep];
        let context = context?;
        let _: () = msg_send![class!(NSGraphicsContext), saveGraphicsState];
        let _: () = msg_send![class!(NSGraphicsContext), setCurrentContext: &*context];
        // A symbol keeps its proportions, centred in 90% of the square; Finder's icons fill it.
        let mut target = Rect {
            origin: Point { x: 0., y: 0. },
            size: Size { width: size, height: size },
        };
        if symbol {
            let natural: Size = msg_send![&*image, size];
            let fit = (size * SYMBOL_FILL / natural.width.max(1.)).min(size * SYMBOL_FILL / natural.height.max(1.));
            let (width, height) = (natural.width * fit, natural.height * fit);
            target = Rect {
                origin: Point { x: (size - width) / 2., y: (size - height) / 2. },
                size: Size { width, height },
            };
        }
        // A zero source rect draws the whole image; operation 2 is NSCompositingOperationSourceOver.
        let whole = Rect { origin: Point { x: 0., y: 0. }, size: Size { width: 0., height: 0. } };
        let _: () = msg_send![&*image, drawInRect: target, fromRect: whole, operation: 2usize, fraction: 1.0f64];
        let _: () = msg_send![class!(NSGraphicsContext), restoreGraphicsState];

        let data: *const u8 = msg_send![&*rep, bitmapData];
        let stride: isize = msg_send![&*rep, bytesPerRow];
        if data.is_null() || stride < pixels * 4 {
            return None;
        }
        // The rep holds premultiplied RGBA rows, maybe padded; the renderer wants tight premultiplied BGRA.
        let side = pixels as usize;
        let mut bgra = Vec::with_capacity(side * side * 4);
        for row in 0..side {
            let row = std::slice::from_raw_parts(data.add(row * stride as usize), side * 4);
            for px in row.chunks_exact(4) {
                bgra.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
        }
        Some((side as u32, bgra))
    }
    let (side, mut bgra) = unsafe { run(kind, f64::from(size)) }?;
    if let Some(rgb) = tint {
        tint_pixels(&mut bgra, rgb, ICON_GIT_TINT);
    }
    let buffer = ImageBuffer::from_raw(side, side, bgra)?;
    Some(Arc::new(RenderImage::new(vec![Frame::new(buffer)])))
}

/// Splits a tinted type key (`rs~ff9f0a`) into the type and the tint's sRGB bytes. A key whose tail after
/// the last `~` isn't six hex digits (an extension that ends in `~`) is no tint.
fn split_tint(key: &str) -> (&str, Option<[u8; 3]>) {
    let Some((kind, hex)) = key.rsplit_once(TINT_SEP) else { return (key, None) };
    let byte = |at: usize| hex.get(at..at + 2).and_then(|pair| u8::from_str_radix(pair, 16).ok());
    match (hex.len(), byte(0), byte(2), byte(4)) {
        (6, Some(r), Some(g), Some(b)) => (kind, Some([r, g, b])),
        _ => (key, None),
    }
}

/// Moves each premultiplied BGRA pixel `strength` of the way to `rgb` at its own alpha, so the icon keeps
/// its shape and shading and wears the colour; transparent pixels stay transparent.
fn tint_pixels(bgra: &mut [u8], rgb: [u8; 3], strength: f32) {
    for pixel in bgra.chunks_exact_mut(4) {
        let alpha = f32::from(pixel[3]) / 255.;
        for (channel, target) in [(0, rgb[2]), (1, rgb[1]), (2, rgb[0])] {
            let mixed = f32::from(pixel[channel]) * (1. - strength) + f32::from(target) * alpha * strength;
            pixel[channel] = mixed.round().min(255.) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_kinds_wear_their_own_symbols() {
        assert_eq!(file_type("README.md"), MARKDOWN);
        assert_eq!(file_type("data.JSON"), JSON);
        assert_eq!(file_type("Cargo.toml"), CONFIG);
        assert_eq!(file_type("Cargo.lock"), LOCK);
        assert_eq!(file_type("package-lock.json"), LOCK);
        assert_eq!(file_type("notes.txt"), TEXT);
        assert_eq!(file_type(".gitignore"), TEXT);
        assert_eq!(file_type("LICENSE"), TEXT);
        assert_eq!(file_type(".env.local"), TEXT);
        assert_eq!(file_type("main.rs"), "rs");
    }

    #[test]
    fn a_tinted_key_splits_into_its_type_and_colour() {
        assert_eq!(split_tint("rs~ff9f0a"), ("rs", Some([0xff, 0x9f, 0x0a])));
        assert_eq!(split_tint("public.folder~00ff80"), ("public.folder", Some([0x00, 0xff, 0x80])));
        // Plain keys and an extension that ends in `~` are no tint.
        assert_eq!(split_tint("rs"), ("rs", None));
        assert_eq!(split_tint("bak~"), ("bak~", None));
        assert_eq!(split_tint("a~b~12345g"), ("a~b~12345g", None));
        assert_eq!(tint_key("rs", Hsla::from(Rgba { r: 1., g: 0.5, b: 0., a: 0.85 })), "rs~ff8000");
    }

    #[test]
    fn a_tint_moves_opaque_pixels_toward_the_colour_and_leaves_clear_ones() {
        // Premultiplied BGRA: an opaque white pixel, a half clear one, a clear one.
        let mut bgra = vec![255, 255, 255, 255, 100, 100, 100, 128, 0, 0, 0, 0];
        tint_pixels(&mut bgra, [200, 100, 0], 0.5);
        // Opaque: halfway between white and the colour (BGR order).
        assert_eq!(&bgra[0..4], &[128, 178, 228, 255]);
        // Half clear: the colour is taken at the pixel's alpha, so it stays under its own alpha.
        assert_eq!(&bgra[4..8], &[50, 75, 100, 128]);
        assert_eq!(&bgra[8..12], &[0, 0, 0, 0]);
    }

    #[test]
    fn uncommon_types_stay_within_the_bound_and_common_ones_stay_ready() {
        let mut cache = Cache::default();
        for ty in COMMON.iter().chain([&FOLDER]) {
            cache.slots.insert(ty.to_string(), Slot::Pending);
            cache.touch(ty);
            cache.land(ty, None);
        }
        for n in 0..300 {
            let ty = format!("ext{n}");
            cache.slots.insert(ty.clone(), Slot::Pending);
            cache.touch(&ty);
            cache.land(&ty, None);
            // Keep drawing the common types, as rows do; they must never leave.
            cache.touch(MARKDOWN);
        }
        assert_eq!(cache.used.len(), UNCOMMON_MAX);
        assert_eq!(cache.slots.len(), UNCOMMON_MAX + COMMON.len() + 1);
        for ty in COMMON.iter().chain([&FOLDER]) {
            assert!(matches!(cache.slots.get(*ty), Some(Slot::Ready(..))), "{ty} left");
        }
        // The oldest went first, the newest stayed.
        assert!(!cache.slots.contains_key("ext0"));
        assert!(cache.slots.contains_key("ext299"));
    }
}
