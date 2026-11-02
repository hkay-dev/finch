//! Real SF Symbols, drawn through AppKit (Harrier's `symbols.rs` is the model).
//!
//! `NSImage(systemSymbolName:)` with an `NSImage.SymbolConfiguration` of the
//! point size is drawn into a bitmap, wrapped as an SVG that embeds
//! the PNG, and handed to GPUI's `svg()` through GPUI Kit's `Icon`: GPUI keeps
//! only the alpha of an SVG and tints it with the text color, which is how
//! SwiftUI draws a monochrome symbol in the foreground style. The glyph is drawn
//! at the natural size AppKit gives it, centered in a box that is `size` tall or
//! as tall as the glyph, so nothing is scaled or clipped.
//!
//! Every symbol the app shows is drawn once at launch on a background thread (`prewarm`), so
//! the first frame only looks them up. An icon names its symbol by asset path
//! (`sf/xmark@9`), and GPUI loads that SVG from [`Assets`] once, into its sprite atlas.

use std::{
    borrow::Cow,
    collections::HashMap,
    ffi::{CString, c_void},
    ptr::null_mut,
    sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError},
};

use gpui_kit::component::Icon;
use gpui_kit::{AssetSource, SharedString, Styled as _, px};
use objc2::{
    class, msg_send,
    rc::{Allocated, Retained, autoreleasepool},
    runtime::AnyObject,
};

use crate::tokens::{
    COPY_SYMBOL, EMPTY_SYMBOL, ERROR_SYMBOL, FILTER_SYMBOL, FIND_SYMBOL, HEADER_BUTTON_SYMBOL, HEADER_CHEVRON, ICON_BUTTON_SYMBOL, STATUS_SYMBOL, TAB_CLOSE_SYMBOL, TAB_LIST_SYMBOL, TAB_WARNING_SYMBOL,
};
use crate::window_look::{Point, Rect, Size};

/// Pixels per point of the embedded bitmap, so a 2x display downsamples cleanly.
const PIXELS_PER_POINT: f64 = 4.;

/// `NSFontWeightRegular`: the weight [`icon`] draws in.
pub const REGULAR: f64 = 0.0;

/// `NSFontWeightSemibold`: the weight of the small chevrons, which read as the tree's stroke-3.2
/// chevron only when drawn heavier than a regular symbol.
pub const SEMIBOLD: f64 = 0.3;

/// Every symbol the app draws through [`icon`], at its size. One missing here still draws, on the
/// main thread the first time it's shown.
const LAUNCH: [(&str, f32); 54] = [
    ("sidebar.right", ICON_BUTTON_SYMBOL),
    ("sidebar.left", ICON_BUTTON_SYMBOL),
    ("xmark", TAB_CLOSE_SYMBOL),
    ("lock", TAB_CLOSE_SYMBOL),
    ("arrow.up", HEADER_BUTTON_SYMBOL),
    ("scope", HEADER_BUTTON_SYMBOL),
    ("eye", HEADER_BUTTON_SYMBOL),
    ("eye.slash", HEADER_BUTTON_SYMBOL),
    ("arrow.down.forward.and.arrow.up.backward", HEADER_BUTTON_SYMBOL),
    ("chevron.down", HEADER_CHEVRON),
    ("line.3.horizontal.decrease", FILTER_SYMBOL),
    ("xmark.circle.fill", FILTER_SYMBOL),
    ("line.3.horizontal.decrease.circle", EMPTY_SYMBOL),
    ("folder", EMPTY_SYMBOL),
    ("exclamationmark.triangle.fill", ERROR_SYMBOL),
    ("info.circle", 13.),
    ("lightbulb", 13.),
    ("exclamationmark.bubble", 13.),
    ("exclamationmark.triangle", 13.),
    ("exclamationmark.octagon", 13.),
    ("quote.bubble", 13.),
    ("arrow.uturn.left", 11.),
    ("photo", 13.),
    ("doc.on.doc", COPY_SYMBOL),
    ("checkmark", COPY_SYMBOL),
    ("magnifyingglass", FILTER_SYMBOL),
    ("arrow.right.to.line", FILTER_SYMBOL),
    ("pencil", FILTER_SYMBOL),
    ("arrow.triangle.2.circlepath", FILTER_SYMBOL),
    ("textformat", FIND_SYMBOL),
    ("textformat.abc.dottedunderline", FIND_SYMBOL),
    ("ellipsis.curlybraces", FIND_SYMBOL),
    ("text.viewfinder", FIND_SYMBOL),
    ("character.phonetic", FIND_SYMBOL),
    ("selection.pin.in.out", FIND_SYMBOL),
    ("chevron.up", FIND_SYMBOL),
    ("chevron.down", FIND_SYMBOL),
    ("arrow.left.arrow.right", FIND_SYMBOL),
    ("xmark", FIND_SYMBOL),
    ("pin.fill", TAB_CLOSE_SYMBOL),
    ("exclamationmark.circle", TAB_WARNING_SYMBOL),
    ("chevron.down", TAB_LIST_SYMBOL),
    ("doc", EMPTY_SYMBOL),
    ("doc.text", EMPTY_SYMBOL),
    ("list.bullet.indent", STATUS_SYMBOL),
    // One per kind of the Go to symbol list (`go_to_symbol::symbol_for`).
    ("function", FILTER_SYMBOL),
    ("s.square", FILTER_SYMBOL),
    ("e.square", FILTER_SYMBOL),
    ("t.square", FILTER_SYMBOL),
    ("i.square", FILTER_SYMBOL),
    ("c.square", FILTER_SYMBOL),
    ("number", FILTER_SYMBOL),
    ("tablecells", FILTER_SYMBOL),
    ("key", FILTER_SYMBOL),
];

/// The symbols drawn through [`icon_weighted`] at [`SEMIBOLD`], at their size.
const LAUNCH_SEMIBOLD: [(&str, f32); 2] = [("chevron.down", HEADER_CHEVRON), ("chevron.down", TAB_LIST_SYMBOL)];

/// A drawn symbol: the asset path its icon names, the SVG behind that path and its box in points.
struct Symbol {
    path: SharedString,
    svg: Vec<u8>,
    width: f32,
    height: f32,
}

/// Each symbol asked for, by name, quarter-point size and weight in thousandths, or `None` for a
/// name that is no symbol on this Mac.
type Cache = HashMap<(&'static str, i32, i32), Option<Arc<Symbol>>>;

static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Mutex::default);

fn cache() -> MutexGuard<'static, Cache> {
    CACHE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Draws every launch symbol on a background thread, so the first frame finds them drawn.
pub fn prewarm() {
    // A thread that won't start leaves each symbol to draw when it's first shown.
    let _ = std::thread::Builder::new().name("symbols".into()).spawn(|| {
        for (name, size) in LAUNCH {
            symbol(name, size, REGULAR);
        }
        for (name, size) in LAUNCH_SEMIBOLD {
            symbol(name, size, SEMIBOLD);
        }
    });
}

/// The symbols the Settings window draws (`settings_window`): the panes' filled glyphs and the controls' chevrons, drawn on a
/// background thread as the window opens, so its first frame finds them drawn.
pub fn prewarm_settings() {
    use crate::tokens::{SETTINGS_CHEVRON, SETTINGS_TILE_SYMBOL};
    let _ = std::thread::Builder::new().name("symbols-settings".into()).spawn(|| {
        for name in ["gearshape.fill", "doc.text.fill", "text.book.closed.fill", "paintpalette.fill", "textformat.size"] {
            symbol(name, SETTINGS_TILE_SYMBOL, SEMIBOLD);
        }
        for name in ["chevron.up.chevron.down", "chevron.up", "chevron.down"] {
            symbol(name, SETTINGS_CHEVRON, SEMIBOLD);
        }
        symbol("xmark.circle.fill", SETTINGS_TILE_SYMBOL, REGULAR);
    });
}

/// The symbol `name` at `size` points in the regular weight, as an icon laid out at its drawn box, tinted by
/// the text color. An unknown name draws nothing, at `size` square.
pub fn icon(name: &'static str, size: f32) -> Icon {
    icon_weighted(name, size, REGULAR)
}

/// [`icon`] at an `NSFont` weight (`SEMIBOLD`); each weight is its own drawing and asset path.
pub fn icon_weighted(name: &'static str, size: f32, weight: f64) -> Icon {
    match symbol(name, size, weight) {
        Some(symbol) => Icon::default().path(symbol.path.clone()).w(px(symbol.width)).h(px(symbol.height)), // design: the symbol's own drawn box
        None => Icon::default().size(px(size)), // design: a missing symbol keeps its slot
    }
}

/// The cached symbol, drawn on this thread first if no thread has drawn it yet. The lock is never
/// held while drawing; two threads that miss together both draw and the first to finish is kept.
fn symbol(name: &'static str, size: f32, weight: f64) -> Option<Arc<Symbol>> {
    let key = (name, (size * 4.).round() as i32, (weight * 1000.).round() as i32);
    if let Some(found) = cache().get(&key) {
        return found.clone();
    }
    let drawn = autoreleasepool(|_| unsafe { draw(name, f64::from(size), weight) }).map(|(png, width, height)| {
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">\
             <image width=\"{width}\" height=\"{height}\" href=\"data:image/png;base64,{}\"/></svg>", // design: SVG markup, not screen text
            base64(&png)
        );
        Arc::new(Symbol {
            path: format!("sf/{name}@{}w{}", f64::from(key.1) / 4., key.2).into(),
            svg: svg.into_bytes(),
            width: width as f32,
            height: height as f32,
        })
    });
    cache().entry(key).or_insert(drawn).clone()
}

/// The app's asset source: serves each drawn symbol's SVG by its `sf/` path, and nothing else.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        if !path.starts_with("sf/") {
            return Ok(None);
        }
        Ok(cache().values().flatten().find(|symbol| symbol.path.as_ref() == path).map(|symbol| Cow::Owned(symbol.svg.clone())))
    }

    fn list(&self, _path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

fn nsstring(text: &str) -> Option<Retained<AnyObject>> {
    let text = CString::new(text).ok()?;
    unsafe { msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()] }
}

/// Draws the symbol at its natural size, in a box `size` tall or the glyph's height when that is
/// taller, and returns the PNG with the box's width and height in points (each rounded up to a
/// whole bitmap pixel).
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn draw(symbol: &str, size: f64, weight: f64) -> Option<(Vec<u8>, f64, f64)> {
    let name = nsstring(symbol)?;
    let image: Option<Retained<AnyObject>> =
        msg_send![class!(NSImage), imageWithSystemSymbolName: &*name, accessibilityDescription: None::<&AnyObject>];
    // NSImageSymbolScaleMedium is 2: SwiftUI's default image scale.
    let config: Retained<AnyObject> = msg_send![
        class!(NSImageSymbolConfiguration),
        configurationWithPointSize: size,
        weight: weight,
        scale: 2isize,
    ];
    let image: Retained<AnyObject> = msg_send![&*image?, imageWithSymbolConfiguration: &*config];
    let natural: Size = msg_send![&*image, size];
    if natural.width <= 0. || natural.height <= 0. {
        return None;
    }
    let pixels_wide = (natural.width * PIXELS_PER_POINT).ceil() as isize;
    let pixels_high = (size.max(natural.height) * PIXELS_PER_POINT).ceil() as isize;
    let (box_width, box_height) = (pixels_wide as f64 / PIXELS_PER_POINT, pixels_high as f64 / PIXELS_PER_POINT);

    let alloc: Allocated<AnyObject> = msg_send![class!(NSBitmapImageRep), alloc];
    let rep: Option<Retained<AnyObject>> = msg_send![
        alloc,
        initWithBitmapDataPlanes: null_mut::<*mut u8>(),
        pixelsWide: pixels_wide,
        pixelsHigh: pixels_high,
        bitsPerSample: 8isize,
        samplesPerPixel: 4isize,
        hasAlpha: true,
        isPlanar: false,
        colorSpaceName: &*nsstring("NSCalibratedRGBColorSpace")?,
        bytesPerRow: 0isize,
        bitsPerPixel: 0isize,
    ];
    let rep = rep?;
    let _: () = msg_send![&*rep, setSize: Size { width: box_width, height: box_height }];
    let context: Option<Retained<AnyObject>> = msg_send![class!(NSGraphicsContext), graphicsContextWithBitmapImageRep: &*rep];
    let context = context?;
    let _: () = msg_send![class!(NSGraphicsContext), saveGraphicsState];
    let _: () = msg_send![class!(NSGraphicsContext), setCurrentContext: &*context];
    let target = Rect {
        origin: Point { x: (box_width - natural.width) / 2., y: (box_height - natural.height) / 2. },
        size: natural,
    };
    // A zero source rect draws the whole image; operation 2 is NSCompositingOperationSourceOver.
    let whole = Rect { origin: Point { x: 0., y: 0. }, size: Size { width: 0., height: 0. } };
    let _: () = msg_send![&*image, drawInRect: target, fromRect: whole, operation: 2usize, fraction: 1.0f64];
    let _: () = msg_send![class!(NSGraphicsContext), restoreGraphicsState];

    let properties: Retained<AnyObject> = msg_send![class!(NSDictionary), dictionary];
    // NSBitmapImageFileTypePNG is 4.
    let data: Option<Retained<AnyObject>> = msg_send![&*rep, representationUsingType: 4usize, properties: &*properties];
    let data = data?;
    let bytes: *const c_void = msg_send![&*data, bytes];
    let length: usize = msg_send![&*data, length];
    if bytes.is_null() {
        return None;
    }
    Some((std::slice::from_raw_parts(bytes as *const u8, length).to_vec(), box_width, box_height))
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}
