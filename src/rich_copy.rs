//! Copy as Rich Text: the preview's Markdown on the pasteboard as styled content. One write
//! carries the HTML (`public.html`, built by `export_html::pasteboard`), the RTF (`public.rtf`,
//! which AppKit makes from that HTML) and the Markdown source as plain text, so Mail, Pages and
//! Notes paste headings, emphasis, links, lists, tables and code with their formatting and a
//! text field pastes the source.
//!
//! Nothing here runs until the menu item is chosen. The HTML is built off the main thread; AppKit's
//! HTML import must run on the main thread, so the RTF conversion and the write do, once per click.
//!
//! A background launch (`GPUI_EDITOR_BACKGROUND`) writes to a pasteboard named for the process, never
//! the general one, so a check can read it back with `read_back`.

use std::ffi::{CString, c_void};

use objc2::{
    Encode, Encoding, class, msg_send,
    rc::{Allocated, Retained},
    runtime::{AnyObject, Bool},
};

const HTML: &str = "public.html";
const RTF: &str = "public.rtf";
const PLAIN: &str = "public.utf8-plain-text";
/// `NSUTF8StringEncoding`.
const UTF8: usize = 4;

/// An `NSRange`.
#[repr(C)]
struct Range {
    location: usize,
    length: usize,
}

// SAFETY: matches Foundation's `NSRange` layout.
unsafe impl Encode for Range {
    const ENCODING: Encoding = Encoding::Struct("_NSRange", &[usize::ENCODING, usize::ENCODING]);
}

fn string(text: &str) -> Retained<AnyObject> {
    let text = CString::new(text.replace('\0', "")).unwrap_or_default();
    unsafe { msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()] }
}

fn data(bytes: &[u8]) -> Retained<AnyObject> {
    unsafe { msg_send![class!(NSData), dataWithBytes: bytes.as_ptr().cast::<c_void>(), length: bytes.len()] }
}

fn bytes(data: &AnyObject) -> Vec<u8> {
    unsafe {
        let length: usize = msg_send![data, length];
        let pointer: *const u8 = msg_send![data, bytes];
        if pointer.is_null() { Vec::new() } else { std::slice::from_raw_parts(pointer, length).to_vec() }
    }
}

/// A one-entry `NSDictionary` of the attributed string's document type.
fn document_type(kind: &str) -> Retained<AnyObject> {
    unsafe { msg_send![class!(NSDictionary), dictionaryWithObject: &*string(kind), forKey: &*string("DocumentType")] }
}

/// The pasteboard Copy as Rich Text writes: the general one, or in a background launch one named
/// for the process.
fn pasteboard() -> Retained<AnyObject> {
    unsafe {
        if std::env::var_os("GPUI_EDITOR_BACKGROUND").is_some() {
            msg_send![class!(NSPasteboard), pasteboardWithName: &*string(&format!("finch-rich-copy-{}", std::process::id()))]
        } else {
            msg_send![class!(NSPasteboard), generalPasteboard]
        }
    }
}

/// The RTF AppKit makes from `html`, or `None` when the import fails. Main thread only.
fn rtf(html: &str) -> Option<Vec<u8>> {
    unsafe {
        let options: Retained<AnyObject> = msg_send![class!(NSMutableDictionary), dictionary];
        let encoding: Retained<AnyObject> = msg_send![class!(NSNumber), numberWithUnsignedInteger: UTF8];
        let _: () = msg_send![&*options, setObject: &*string("NSHTML"), forKey: &*string("DocumentType")];
        let _: () = msg_send![&*options, setObject: &*encoding, forKey: &*string("CharacterEncoding")];
        let alloc: Allocated<AnyObject> = msg_send![class!(NSAttributedString), alloc];
        let attributed: Option<Retained<AnyObject>> = msg_send![alloc,
            initWithData: &*data(html.as_bytes()), options: &*options,
            documentAttributes: std::ptr::null_mut::<AnyObject>(), error: std::ptr::null_mut::<*mut AnyObject>()];
        let attributed = attributed?;
        let length: usize = msg_send![&*attributed, length];
        let out: Option<Retained<AnyObject>> = msg_send![&*attributed,
            dataFromRange: Range { location: 0, length }, documentAttributes: &*document_type("NSRTF"),
            error: std::ptr::null_mut::<*mut AnyObject>()];
        out.map(|out| bytes(&out)).filter(|rtf| !rtf.is_empty())
    }
}

/// The HTML page for `markdown`; `base` is the folder its relative pictures start from. Pure work,
/// safe off the main thread.
pub fn html(markdown: &str, base: Option<&std::path::Path>) -> String {
    crate::export_html::pasteboard(markdown, base)
}

/// `html` without the `<img>` tags whose `src` is not a `data:` address. The RTF import loads what
/// those name, over the network on the main thread, and can hang there; the HTML flavor keeps them.
fn without_outside_images(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find("<img") {
        let (head, tail) = rest.split_at(at);
        out.push_str(head);
        let end = tail.find('>').map_or(tail.len(), |end| end + 1);
        if tail[..end].contains(" src=\"data:") {
            out.push_str(&tail[..end]);
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// Puts `html` (as `public.html`), its RTF and `markdown` (as plain text) on the pasteboard in one
/// write. Main thread only. False when the pasteboard refused the write.
pub fn write(markdown: &str, html: &str) -> bool {
    let rtf = rtf(&without_outside_images(html));
    unsafe {
        let pasteboard = pasteboard();
        let _: isize = msg_send![&*pasteboard, clearContents];
        let html_ok: Bool = msg_send![&*pasteboard, setData: &*data(html.as_bytes()), forType: &*string(HTML)];
        let mut written = html_ok.as_bool();
        if let Some(rtf) = rtf {
            let ok: Bool = msg_send![&*pasteboard, setData: &*data(&rtf), forType: &*string(RTF)];
            written &= ok.as_bool();
        }
        let ok: Bool = msg_send![&*pasteboard, setString: &*string(markdown), forType: &*string(PLAIN)];
        written & ok.as_bool()
    }
}

/// What the pasteboard holds, for the `rich-copy` control verb: each type with its size in bytes,
/// the RTF's first bytes and the plain text's first line.
pub fn read_back() -> String {
    unsafe {
        let pasteboard = pasteboard();
        let types: Option<Retained<AnyObject>> = msg_send![&*pasteboard, types];
        let count: usize = match types.as_deref() {
            Some(types) => msg_send![types, count],
            None => 0,
        };
        let mut report = Vec::new();
        for ix in 0..count {
            let kind: Retained<AnyObject> = msg_send![types.as_deref().unwrap(), objectAtIndex: ix];
            let name = {
                let chars: *const std::ffi::c_char = msg_send![&*kind, UTF8String];
                std::ffi::CStr::from_ptr(chars).to_string_lossy().into_owned()
            };
            if ![HTML, RTF, PLAIN].contains(&name.as_str()) {
                continue;
            }
            let held: Option<Retained<AnyObject>> = msg_send![&*pasteboard, dataForType: &*kind];
            let held = held.map(|held| bytes(&held)).unwrap_or_default();
            let head = String::from_utf8_lossy(&held[..held.len().min(if name == PLAIN { 40 } else { 14 })]).replace('\n', "\\n");
            report.push(format!("{name} {} bytes {head:?}", held.len()));
        }
        report.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::without_outside_images;

    #[test]
    fn only_data_images_reach_the_rtf_import() {
        let html = "<p>a <img src=\"https://x.test/a.png\" alt=\"x\"> b <img src=\"data:image/png;base64,AAAA\" alt=\"y\"> c <img src=\"/tmp/gone.png\"></p>";
        assert_eq!(without_outside_images(html), "<p>a  b <img src=\"data:image/png;base64,AAAA\" alt=\"y\"> c </p>");
    }
}
