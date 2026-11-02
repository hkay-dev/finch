//! Insert text from an iPhone photo or scan (Edit > Import from iPhone or iPad).
//!
//! AppKit's Continuity Camera items (Take Photo, Scan Documents) work through the responder chain:
//! the window's view answers `validRequestorForSendType:returnType:` for image return types and
//! its `readSelectionFromPasteboard:` posts `GPUIPasteboardImageNotification` when the pasteboard
//! holds an image and no text (`vendor/gpui-pre-macos/PATCHED.md`). This module observes that
//! notification, copies the image (or the scan's PDF) off the pasteboard, recognizes its text with
//! Vision on the background executor, and pastes the result at the caret of the key editor through
//! `EditorState::paste_text`, so it is one edit (one Undo) with the paste's wash and the clipboard
//! is untouched. No text found: nothing is inserted and the app beeps.
//!
//! Vision is `dlopen`ed on the first recognition; nothing of it is linked or loaded at launch.
//! Written against the Objective-C runtime without typed bindings, as `native_menu.rs` is.

use std::{
    cell::RefCell,
    ffi::{CStr, CString, c_char, c_void},
    path::Path,
    sync::{Once, mpsc},
};

use gpui_kit::component::input::EditorState;
use gpui_kit::{AnyWindowHandle, App, AppContext as _, AsyncApp, WeakEntity};

use crate::document::Document;
use objc2::{
    ClassType, Encode, Encoding, class, define_class, msg_send,
    rc::{Retained, autoreleasepool},
    runtime::{AnyClass, AnyObject, Bool, NSObject},
    sel,
};

/// What `readSelectionFromPasteboard:` posts, as the vendored window names it.
const NOTIFICATION: &str = "GPUIPasteboardImageNotification";

/// The pasteboard types the window hands over, scan first, with the older names; the first two
/// are PDF.
const TYPES: [&str; 6] = [
    "com.adobe.pdf",
    "Apple PDF pasteboard type",
    "public.jpeg",
    "public.png",
    "public.tiff",
    "NeXT TIFF v4.0 pasteboard type",
];

/// Pixels on the long side of a scanned page when it is drawn for recognition.
const PAGE_LONG_SIDE: f64 = 2800.;

/// Pages of a scan that are read; the rest are left.
const MAX_PAGES: usize = 50;

/// An image's bytes as they came, and whether they are a PDF.
struct Source {
    bytes: Vec<u8>,
    pdf: bool,
}

thread_local! {
    /// The app, for the notification handler, which AppKit calls with no `App` borrowed.
    static APP: RefCell<Option<AsyncApp>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements and the class has no ivars.
    #[unsafe(super(NSObject))]
    #[name = "FinchCameraTextObserver"]
    struct Observer;

    impl Observer {
        #[unsafe(method(imageArrived:))]
        fn image_arrived(&self, note: &AnyObject) {
            let Some(source) = source_of(note) else { return };
            let Some(app) = APP.with_borrow(Clone::clone) else { return };
            match app.update(|cx| current_target(cx)) {
                Some(target) => submit(source, Some(target), |_| {}, &app),
                None => crate::preview::render::beep(),
            }
        }
    }
);

/// Starts listening for an image the window receives. Call once, on the main thread, at launch;
/// it adds one notification observer and loads nothing.
pub fn install(cx: &mut App) {
    APP.with_borrow_mut(|app| *app = Some(cx.to_async()));
    // SAFETY: plain Foundation calls on the main thread; the observer is kept alive by leaking it,
    // as the center holds it unretained and it lives as long as the app.
    unsafe {
        let observer: Retained<AnyObject> = msg_send![Observer::class(), new];
        let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        let name = string(NOTIFICATION);
        let _: () = msg_send![center, addObserver: &*observer, selector: sel!(imageArrived:),
            name: &*name, object: std::ptr::null::<AnyObject>()];
        std::mem::forget(observer);
    }
}

/// The control verb `camera-text <image or pdf file>`: the same path an image from the iPhone
/// takes, from a file. Replies with what went in, or the reason nothing did.
pub fn from_file(path: &str, reply: mpsc::Sender<String>, cx: &mut App) {
    let path = Path::new(path);
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            reply.send(format!("err {}: {err}", path.display())).ok();
            return;
        }
    };
    let pdf = path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"));
    submit(
        Source { bytes, pdf },
        None,
        move |result| {
            reply.send(result).ok();
        },
        &cx.to_async(),
    );
}

/// Recognizes the text of `source` off the main thread, pastes it, and calls `done` with a line
/// saying what happened.
fn submit(source: Source, target: Option<Target>, done: impl FnOnce(String) + 'static, app: &AsyncApp) {
    let background = app.background_executor().clone();
    app.spawn(async move |cx| {
        let recognized = background.spawn(async move { recognize(&source) }).await;
        let line = cx.update(|cx| match recognized {
            Ok(text) if !text.trim().is_empty() => match paste(&text, target, cx) {
                true => format!("inserted {} characters: {}", text.chars().count(), text.replace('\n', "\\n")),
                false => {
                    crate::preview::render::beep();
                    "err the editor that asked for the text is gone or can't change".to_string()
                }
            },
            Ok(_) => {
                crate::preview::render::beep();
                "err no text found".to_string()
            }
            Err(err) => {
                crate::preview::render::beep();
                format!("err {err}")
            }
        });
        done(line);
    })
    .detach();
}

/// Pastes `text` as a paste does: at the caret of the editor `target` named when the picture
/// arrived, if its tab still shows that editor; with no target (the control verb), at the key
/// window's editor.
fn paste(text: &str, target: Option<Target>, cx: &mut App) -> bool {
    let (handle, editor) = match target {
        Some(target) => {
            let (Some(document), Some(editor)) = (target.document.upgrade(), target.editor.upgrade()) else {
                return false;
            };
            if document.read(cx).built_editor() != Some(&editor) {
                return false;
            }
            (target.handle, editor)
        }
        None => {
            let Some(editor) = crate::app::key_editor(cx) else { return false };
            let Some(handle) = cx.active_window().or_else(|| crate::control::target(cx).ok().map(|(handle, _)| handle)) else {
                return false;
            };
            (handle, editor)
        }
    };
    if !editor.read(cx).is_editable() {
        return false;
    }
    cx.update_window(handle, |_, window, cx| editor.update(cx, |state, cx| state.paste_text(text, window, cx))).is_ok()
}

/// Where a picture goes: the key window's showing tab and its editor, as the picture arrived.
struct Target {
    handle: AnyWindowHandle,
    document: WeakEntity<Document>,
    editor: WeakEntity<EditorState>,
}

fn current_target(cx: &App) -> Option<Target> {
    let (handle, document) = crate::app::key_document(cx)?;
    let editor = document.read(cx).built_editor()?.clone();
    Some(Target { handle, document: document.downgrade(), editor: editor.downgrade() })
}

// ---- The pasteboard ----

fn string(text: &str) -> Retained<AnyObject> {
    let text = CString::new(text).unwrap_or_default();
    unsafe { msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()] }
}

fn text_of(string: &AnyObject) -> String {
    let chars: *const c_char = unsafe { msg_send![string, UTF8String] };
    if chars.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(chars) }.to_string_lossy().into_owned()
}

/// The image data the notification's pasteboard holds, copied out.
fn source_of(note: &AnyObject) -> Option<Source> {
    unsafe {
        let info: *mut AnyObject = msg_send![note, userInfo];
        if info.is_null() {
            return None;
        }
        let pasteboard: *mut AnyObject = msg_send![info, objectForKey: &*string("pasteboard")];
        if pasteboard.is_null() {
            return None;
        }
        TYPES.iter().enumerate().find_map(|(index, kind)| {
            let data: *mut AnyObject = msg_send![pasteboard, dataForType: &*string(kind)];
            if data.is_null() {
                return None;
            }
            let length: usize = msg_send![data, length];
            let bytes: *const u8 = msg_send![data, bytes];
            (!bytes.is_null() && length > 0).then(|| Source {
                bytes: std::slice::from_raw_parts(bytes, length).to_vec(),
                pdf: index < 2,
            })
        })
    }
}

// ---- Vision ----

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
}

/// `CGImageRef` for `msg_send`'s argument check.
#[repr(transparent)]
#[derive(Clone, Copy)]
struct CGImageRef(*const c_void);

// SAFETY: a `CGImageRef` is a pointer to the opaque `CGImage` struct.
unsafe impl Encode for CGImageRef {
    const ENCODING: Encoding = Encoding::Pointer(&Encoding::Struct("CGImage", &[]));
}

/// Loads Vision once; false when this Mac has none.
fn load_vision() -> bool {
    static LOAD: Once = Once::new();
    LOAD.call_once(|| {
        // SAFETY: a plain `dlopen` of a system framework by path; the handle is never closed.
        unsafe { dlopen(c"/System/Library/Frameworks/Vision.framework/Vision".as_ptr(), 1) };
    });
    AnyClass::get(c"VNRecognizeTextRequest").is_some()
}

/// The text of the image or scan, pages joined by a blank line. Runs off the main thread.
fn recognize(source: &Source) -> Result<String, String> {
    if !load_vision() {
        return Err("Vision isn't available on this Mac".to_string());
    }
    autoreleasepool(|_| {
        let pages = if source.pdf {
            let pages = read_pdf(&source.bytes)?;
            if pages.is_empty() {
                return Err("the scan has no pages".to_string());
            }
            pages
        } else {
            vec![read_data(&source.bytes)?]
        };
        let pages: Vec<String> = pages.into_iter().map(|page| page.trim().to_string()).filter(|page| !page.is_empty()).collect();
        Ok(pages.join("\n\n"))
    })
}

/// A Vision text request: accurate level, language correction on.
fn request() -> Retained<AnyObject> {
    unsafe {
        let class = AnyClass::get(c"VNRecognizeTextRequest").expect("Vision is loaded");
        let request: Retained<AnyObject> = msg_send![msg_send![class, alloc], init];
        // VNRequestTextRecognitionLevelAccurate.
        let _: () = msg_send![&*request, setRecognitionLevel: 0isize];
        let _: () = msg_send![&*request, setUsesLanguageCorrection: Bool::YES];
        request
    }
}

fn read_data(bytes: &[u8]) -> Result<String, String> {
    unsafe {
        let data: Retained<AnyObject> = msg_send![class!(NSData), dataWithBytes: bytes.as_ptr() as *const c_void, length: bytes.len()];
        let options: Retained<AnyObject> = msg_send![class!(NSDictionary), dictionary];
        let class = AnyClass::get(c"VNImageRequestHandler").ok_or("Vision isn't available on this Mac")?;
        let handler: Retained<AnyObject> = msg_send![msg_send![class, alloc], initWithData: &*data, options: &*options];
        perform(&handler)
    }
}

fn read_cg_image(image: CGImageRef) -> Result<String, String> {
    unsafe {
        let options: Retained<AnyObject> = msg_send![class!(NSDictionary), dictionary];
        let class = AnyClass::get(c"VNImageRequestHandler").ok_or("Vision isn't available on this Mac")?;
        let handler: Retained<AnyObject> = msg_send![msg_send![class, alloc], initWithCGImage: image, options: &*options];
        perform(&handler)
    }
}

/// Runs a text request on `handler` and returns the lines it found, one a line.
unsafe fn perform(handler: &AnyObject) -> Result<String, String> {
    unsafe {
        let request = request();
        let requests: Retained<AnyObject> = msg_send![class!(NSArray), arrayWithObject: &*request];
        let mut error: *mut AnyObject = std::ptr::null_mut();
        let ok: Bool = msg_send![handler, performRequests: &*requests, error: &mut error];
        if !ok.as_bool() {
            let reason = if error.is_null() {
                "Vision could not read the image".to_string()
            } else {
                let description: *mut AnyObject = msg_send![error, localizedDescription];
                text_of(&*description)
            };
            return Err(reason);
        }
        let results: *mut AnyObject = msg_send![&*request, results];
        if results.is_null() {
            return Ok(String::new());
        }
        let count: usize = msg_send![results, count];
        let mut lines = Vec::with_capacity(count);
        for index in 0..count {
            let observation: *mut AnyObject = msg_send![results, objectAtIndex: index];
            let candidates: *mut AnyObject = msg_send![observation, topCandidates: 1usize];
            if candidates.is_null() {
                continue;
            }
            let first: *mut AnyObject = msg_send![candidates, firstObject];
            if first.is_null() {
                continue;
            }
            let line: *mut AnyObject = msg_send![first, string];
            if !line.is_null() {
                lines.push(text_of(&*line));
            }
        }
        Ok(lines.join("\n"))
    }
}

// ---- A scan's PDF, drawn page by page with Core Graphics (already loaded by GPUI) ----

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGAffineTransform {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    tx: f64,
    ty: f64,
}

unsafe extern "C" {
    fn CGDataProviderCreateWithData(
        info: *mut c_void,
        data: *const c_void,
        size: usize,
        release: *const c_void,
    ) -> *mut c_void;
    fn CGDataProviderRelease(provider: *mut c_void);
    fn CGPDFDocumentCreateWithProvider(provider: *mut c_void) -> *mut c_void;
    fn CGPDFDocumentRelease(document: *mut c_void);
    fn CGPDFDocumentGetNumberOfPages(document: *mut c_void) -> usize;
    fn CGPDFDocumentGetPage(document: *mut c_void, number: usize) -> *mut c_void;
    fn CGPDFPageGetBoxRect(page: *mut c_void, kind: i32) -> CGRect;
    fn CGPDFPageGetRotationAngle(page: *mut c_void) -> i32;
    fn CGPDFPageGetDrawingTransform(page: *mut c_void, kind: i32, rect: CGRect, rotate: i32, preserve_aspect: bool) -> CGAffineTransform;
    fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
    fn CGColorSpaceRelease(space: *mut c_void);
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: *mut c_void,
        info: u32,
    ) -> *mut c_void;
    fn CGBitmapContextCreateImage(context: *mut c_void) -> *const c_void;
    fn CGContextRelease(context: *mut c_void);
    fn CGContextSetRGBFillColor(context: *mut c_void, r: f64, g: f64, b: f64, a: f64);
    fn CGContextFillRect(context: *mut c_void, rect: CGRect);
    fn CGContextConcatCTM(context: *mut c_void, transform: CGAffineTransform);
    fn CGContextDrawPDFPage(context: *mut c_void, page: *mut c_void);
    fn CGImageRelease(image: *const c_void);
    fn CGContextScaleCTM(context: *mut c_void, sx: f64, sy: f64);
}

/// The text of each page of the PDF (at most `MAX_PAGES`), read one page at a time: the page is
/// drawn on white, `PAGE_LONG_SIDE` pixels on its long side, recognized and released before the
/// next is drawn, each in its own autorelease pool.
fn read_pdf(bytes: &[u8]) -> Result<Vec<String>, String> {
    // SAFETY: `bytes` outlives the provider and the document, both released before returning;
    // each Core Graphics object is released once.
    unsafe {
        let provider = CGDataProviderCreateWithData(std::ptr::null_mut(), bytes.as_ptr().cast(), bytes.len(), std::ptr::null());
        if provider.is_null() {
            return Err("the scan could not be read".to_string());
        }
        let document = CGPDFDocumentCreateWithProvider(provider);
        CGDataProviderRelease(provider);
        if document.is_null() {
            return Err("the scan could not be read".to_string());
        }
        let space = CGColorSpaceCreateDeviceRGB();
        let mut pages = Vec::new();
        let mut failure = None;
        for number in 1..=CGPDFDocumentGetNumberOfPages(document).min(MAX_PAGES) {
            let read = autoreleasepool(|_| {
                let image = render_page(document, number, space)?;
                let text = read_cg_image(image);
                CGImageRelease(image.0);
                Some(text)
            });
            match read {
                Some(Ok(text)) => pages.push(text),
                Some(Err(err)) => {
                    failure = Some(err);
                    break;
                }
                None => {}
            }
        }
        CGColorSpaceRelease(space);
        CGPDFDocumentRelease(document);
        match failure {
            Some(err) => Err(err),
            None => Ok(pages),
        }
    }
}

/// Page `number` (from 1) of `document` as an image on white, or `None` for a page that is empty
/// or can't be drawn. The caller releases the image.
unsafe fn render_page(document: *mut c_void, number: usize, space: *mut c_void) -> Option<CGImageRef> {
    // kCGPDFMediaBox, kCGImageAlphaPremultipliedLast.
    const MEDIA_BOX: i32 = 0;
    const ALPHA_LAST: u32 = 1;
    unsafe {
        let page = CGPDFDocumentGetPage(document, number);
        if page.is_null() {
            return None;
        }
        let rect = CGPDFPageGetBoxRect(page, MEDIA_BOX);
        let turned = matches!(CGPDFPageGetRotationAngle(page).rem_euclid(360), 90 | 270);
        let (width, height) = if turned { (rect.size.height, rect.size.width) } else { (rect.size.width, rect.size.height) };
        if width < 1. || height < 1. {
            return None;
        }
        let scale = PAGE_LONG_SIDE / width.max(height);
        let (pixels_wide, pixels_high) = ((width * scale).round() as usize, (height * scale).round() as usize);
        let context = CGBitmapContextCreate(std::ptr::null_mut(), pixels_wide, pixels_high, 8, 0, space, ALPHA_LAST);
        if context.is_null() {
            return None;
        }
        let area = CGRect {
            origin: CGPoint { x: 0., y: 0. },
            size: CGSize { width: pixels_wide as f64, height: pixels_high as f64 },
        };
        // The drawing transform fits the page to a rect in points and only scales down, so it is
        // built for the page's own size and the context is scaled up to the pixels.
        let points = CGRect { origin: CGPoint { x: 0., y: 0. }, size: CGSize { width, height } };
        CGContextSetRGBFillColor(context, 1., 1., 1., 1.);
        CGContextFillRect(context, area);
        CGContextScaleCTM(context, scale, scale);
        CGContextConcatCTM(context, CGPDFPageGetDrawingTransform(page, MEDIA_BOX, points, 0, true));
        CGContextDrawPDFPage(context, page);
        let image = CGBitmapContextCreateImage(context);
        CGContextRelease(context);
        (!image.is_null()).then_some(CGImageRef(image))
    }
}
