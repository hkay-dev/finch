//! Pele (Obsidian's Mermaid renderer, `vendor/pele/`) run in the Mac's own JavaScriptCore.
//!
//! JavaScriptCore is loaded with `dlopen` on the first `render` and its C API reached through
//! `dlsym`, so nothing links it and a file with no diagram never loads it. One context lives on
//! one background thread (a context is not safe to use from two threads at once); the thread
//! releases it after `IDLE` with no request and exits, and the next request starts a new one.
//!
//! Pele colors everything through CSS custom properties (`fill="var(--_fg)"`), which resvg does
//! not resolve, so `render` replaces every `var(...)` with the palette's values before the SVG
//! leaves this module.

use std::ffi::{CString, c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::cell::Cell;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Pele 1.13.0's single-file bundle (`vendor/pele/README.md`).
const BUNDLE: &str = include_str!("../../vendor/pele/pele.global.js");

/// How long the JavaScriptCore thread waits for a request before releasing the context.
const IDLE: Duration = Duration::from_secs(60);

/// How long one evaluation may run in the context (loading the bundle or drawing one diagram)
/// before JavaScriptCore stops it. A diagram that runs away then fails, its context is dropped,
/// and the next request builds a fresh one, so the thread is never stuck.
const RUN_LIMIT_SECONDS: f64 = 10.0;

/// Colors (`0xRRGGBB`) and font that replace Pele's `--pele-*` custom properties.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    /// `--pele-bg`: the preview ground.
    pub bg: u32,
    /// `--pele-fg`: text ink.
    pub fg: u32,
    /// `--pele-line`: edge and line ink.
    pub line: u32,
    /// `--pele-muted`: secondary ink.
    pub muted: u32,
    /// `--pele-surface`: card fill.
    pub surface: u32,
    /// `--pele-surface-alt`: alternate card fill.
    pub surface_alt: u32,
    /// `--pele-border`: hairline.
    pub border: u32,
    /// `--pele-accent`.
    pub accent: u32,
    /// `--pele-font`: CSS font-family list, such as `Inter, sans-serif`.
    pub font: String,
    /// `--pele-series-1` to `-8`.
    pub series: [u32; 8],
}

/// One rendered diagram: self-contained SVG (no `var()`), sized in logical pixels.
#[derive(Clone, Debug)]
pub struct Diagram {
    pub svg: String,
    pub width: f32,
    pub height: f32,
}

/// Renders Mermaid `source` laid out for `max_width`. `id_prefix` must differ between diagrams.
/// Blocking: sends the job to the JavaScriptCore thread and waits. Call from the background
/// executor, never the main thread. A Pele parse error comes back as its message.
pub fn render(source: &str, max_width: f32, palette: &Palette, id_prefix: &str) -> Result<Diagram, String> {
    let (reply, answer) = mpsc::channel();
    let job = Job {
        source: source.to_owned(),
        max_width,
        font: palette.font.clone(),
        id_prefix: id_prefix.to_owned(),
        reply,
    };
    send(job)?;
    let raw = answer.recv().map_err(|_| {
        // The thread died mid-job (a panic); the next call starts a fresh one.
        *lock_engine() = None;
        "the diagram engine stopped".to_owned()
    })??;
    Ok(Diagram { svg: resolve_vars(&raw.svg, palette), width: raw.width, height: raw.height })
}

/// True once JavaScriptCore has been loaded in this process.
pub fn loaded() -> bool {
    LOADED.load(Ordering::Relaxed)
}

static LOADED: AtomicBool = AtomicBool::new(false);

struct Job {
    source: String,
    max_width: f32,
    font: String,
    id_prefix: String,
    reply: Sender<Result<Raw, String>>,
}

struct Raw {
    svg: String,
    width: f32,
    height: f32,
}

/// The queue into the running JavaScriptCore thread; `None` while no thread runs.
static ENGINE: Mutex<Option<Sender<Job>>> = Mutex::new(None);

fn lock_engine() -> std::sync::MutexGuard<'static, Option<Sender<Job>>> {
    ENGINE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Queues `job`, starting the thread if none runs. Sending under the lock means the thread's
/// idle exit (which also takes the lock) can never miss a job.
fn send(job: Job) -> Result<(), String> {
    let mut engine = lock_engine();
    let mut job = Some(job);
    for _ in 0..2 {
        if engine.is_none() {
            let (tx, rx) = mpsc::channel();
            std::thread::Builder::new()
                .name("pele".into())
                .stack_size(8 << 20)
                .spawn(move || run_thread(rx))
                .map_err(|e| format!("could not start the diagram engine: {e}"))?;
            *engine = Some(tx);
        }
        match engine.as_ref().expect("set above").send(job.take().expect("job kept until sent")) {
            Ok(()) => return Ok(()),
            Err(mpsc::SendError(back)) => {
                job = Some(back);
                *engine = None;
            }
        }
    }
    Err("the diagram engine would not start".to_owned())
}

fn run_thread(jobs: Receiver<Job>) {
    let mut js: Option<Js> = None;
    loop {
        let job = match jobs.recv_timeout(IDLE) {
            Ok(job) => job,
            Err(RecvTimeoutError::Timeout) => {
                let mut engine = lock_engine();
                match jobs.try_recv() {
                    Ok(job) => job,
                    Err(_) => {
                        *engine = None;
                        return; // `js` drops here and releases the context
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if js.is_none() {
            match Js::start() {
                Ok(started) => js = Some(started),
                Err(message) => {
                    job.reply.send(Err(message)).ok();
                    continue;
                }
            }
        }
        let engine = js.as_ref().expect("started above");
        let result = engine.render(&job);
        if engine.broken.get() {
            js = None;
        }
        job.reply.send(result).ok();
    }
}

// ---- JavaScriptCore C API, through dlsym ----

type Ctx = *mut c_void;
type Value = *const c_void;
type Object = *mut c_void;
type JsString = *mut c_void;

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}

/// The JavaScriptCore functions this module calls.
struct Api {
    group_create: unsafe extern "C" fn() -> *mut c_void,
    group_release: unsafe extern "C" fn(*mut c_void),
    group_set_time_limit: unsafe extern "C" fn(*mut c_void, f64, *const c_void, *mut c_void),
    context_create: unsafe extern "C" fn(*mut c_void, *mut c_void) -> Ctx,
    context_release: unsafe extern "C" fn(Ctx),
    global_object: unsafe extern "C" fn(Ctx) -> Object,
    evaluate: unsafe extern "C" fn(Ctx, JsString, Object, JsString, c_int, *mut Value) -> Value,
    string_create: unsafe extern "C" fn(*const c_char) -> JsString,
    string_release: unsafe extern "C" fn(JsString),
    string_max_size: unsafe extern "C" fn(JsString) -> usize,
    string_utf8: unsafe extern "C" fn(JsString, *mut c_char, usize) -> usize,
    value_to_string: unsafe extern "C" fn(Ctx, Value, *mut Value) -> JsString,
    value_make_string: unsafe extern "C" fn(Ctx, JsString) -> Value,
    value_make_number: unsafe extern "C" fn(Ctx, f64) -> Value,
    value_protect: unsafe extern "C" fn(Ctx, Value),
    value_unprotect: unsafe extern "C" fn(Ctx, Value),
    set_property: unsafe extern "C" fn(Ctx, Object, JsString, Value, c_int, *mut Value),
}

/// Loads JavaScriptCore once; the handle is never closed.
fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| {
        // SAFETY: a plain `dlopen` of a system framework by path.
        let handle = unsafe {
            dlopen(c"/System/Library/Frameworks/JavaScriptCore.framework/JavaScriptCore".as_ptr(), 1)
        };
        if handle.is_null() {
            return Err("JavaScriptCore could not be loaded".to_owned());
        }
        LOADED.store(true, Ordering::Relaxed);
        macro_rules! sym {
            ($name:literal) => {{
                // SAFETY: the symbol is part of JavaScriptCore's public C API and the field it
                // fills has that function's signature.
                let ptr = unsafe { dlsym(handle, $name.as_ptr()) };
                if ptr.is_null() {
                    return Err(format!("JavaScriptCore has no {}", $name.to_string_lossy()));
                }
                unsafe { std::mem::transmute(ptr) }
            }};
        }
        Ok(Api {
            group_create: sym!(c"JSContextGroupCreate"),
            group_release: sym!(c"JSContextGroupRelease"),
            group_set_time_limit: sym!(c"JSContextGroupSetExecutionTimeLimit"),
            context_create: sym!(c"JSGlobalContextCreateInGroup"),
            context_release: sym!(c"JSGlobalContextRelease"),
            global_object: sym!(c"JSContextGetGlobalObject"),
            evaluate: sym!(c"JSEvaluateScript"),
            string_create: sym!(c"JSStringCreateWithUTF8CString"),
            string_release: sym!(c"JSStringRelease"),
            string_max_size: sym!(c"JSStringGetMaximumUTF8CStringSize"),
            string_utf8: sym!(c"JSStringGetUTF8CString"),
            value_to_string: sym!(c"JSValueToStringCopy"),
            value_make_string: sym!(c"JSValueMakeString"),
            value_make_number: sym!(c"JSValueMakeNumber"),
            value_protect: sym!(c"JSValueProtect"),
            value_unprotect: sym!(c"JSValueUnprotect"),
            set_property: sym!(c"JSObjectSetProperty"),
        })
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// A JavaScriptCore context with Pele loaded. Only ever used on the `pele` thread.
struct Js {
    api: &'static Api,
    /// The context group that carries the execution time limit.
    group: *mut c_void,
    ctx: Ctx,
    /// Set when an evaluation threw out of the engine (the time limit stopped it): the context
    /// can't be trusted again, so the thread drops it.
    broken: Cell<bool>,
    /// The `pele` object the bundle evaluates to, protected from collection.
    pele: Value,
}

/// Runs in the context: calls `pele.render` on the `__src` and `__opts` globals and answers
/// `ok\n<width>\n<height>\n<svg>` or `err\n<message>`.
const CALL: &str = "(function(){try{var r=__pele.render(__src,{maxWidth:__maxWidth,fontFamily:__font,\
idPrefix:__prefix,links:false,images:false});return 'ok\\n'+r.width+'\\n'+r.height+'\\n'+r.svg;}\
catch(e){return 'err\\n'+String(e&&e.message!==undefined?e.message:e);}})()";

impl Js {
    fn start() -> Result<Js, String> {
        let api = api()?;
        // SAFETY: a fresh group; a null callback makes JavaScriptCore terminate the script at the
        // limit; a null class gives a default global object.
        let (group, ctx) = unsafe {
            let group = (api.group_create)();
            (api.group_set_time_limit)(group, RUN_LIMIT_SECONDS, std::ptr::null(), std::ptr::null_mut());
            (group, (api.context_create)(group, std::ptr::null_mut()))
        };
        if ctx.is_null() {
            // SAFETY: the group was created above and nothing else holds it.
            unsafe { (api.group_release)(group) };
            return Err("JavaScriptCore could not make a context".to_owned());
        }
        let mut js = Js { api, group, ctx, broken: Cell::new(false), pele: std::ptr::null() };
        // The bundle is strict (`var pele` is not global), so end the script with `pele` and
        // keep the value it returns.
        let script = format!("{BUNDLE}\n;pele");
        let pele = js.eval(&script)?;
        // SAFETY: `pele` belongs to `ctx`.
        unsafe { (api.value_protect)(ctx, pele) };
        js.pele = pele;
        js.set_global("__pele", pele)?;
        Ok(js)
    }

    fn render(&self, job: &Job) -> Result<Raw, String> {
        let src = self.string_value(&job.source)?;
        self.set_global("__src", src)?;
        let font = self.string_value(&job.font)?;
        self.set_global("__font", font)?;
        let prefix = self.string_value(&job.id_prefix)?;
        self.set_global("__prefix", prefix)?;
        // SAFETY: `ctx` is live.
        let width = unsafe { (self.api.value_make_number)(self.ctx, f64::from(job.max_width)) };
        self.set_global("__maxWidth", width)?;
        let out = self.eval(CALL)?;
        let text = self.to_rust_string(out)?;
        let (kind, rest) = text.split_once('\n').ok_or("the diagram engine answered nothing")?;
        if kind == "err" {
            return Err(rest.to_owned());
        }
        let mut parts = rest.splitn(3, '\n');
        let (Some(w), Some(h), Some(svg)) = (parts.next(), parts.next(), parts.next()) else {
            return Err("the diagram engine answered badly".to_owned());
        };
        let (Ok(width), Ok(height)) = (w.parse::<f32>(), h.parse::<f32>()) else {
            return Err("the diagram engine gave no size".to_owned());
        };
        Ok(Raw { svg: svg.to_owned(), width, height })
    }

    fn js_string(&self, text: &str) -> Result<JsString, String> {
        let c = CString::new(text).map_err(|_| "the diagram text holds a NUL character".to_owned())?;
        // SAFETY: `c` is NUL terminated and outlives the call, which copies it.
        Ok(unsafe { (self.api.string_create)(c.as_ptr()) })
    }

    fn string_value(&self, text: &str) -> Result<Value, String> {
        let s = self.js_string(text)?;
        // SAFETY: `s` is a live string and `ctx` a live context; the value copies the string.
        let value = unsafe { (self.api.value_make_string)(self.ctx, s) };
        unsafe { (self.api.string_release)(s) };
        Ok(value)
    }

    fn set_global(&self, name: &str, value: Value) -> Result<(), String> {
        let key = self.js_string(name)?;
        let mut exception: Value = std::ptr::null();
        // SAFETY: `ctx`, the global object and `value` are live; `key` is released after use.
        unsafe {
            let global = (self.api.global_object)(self.ctx);
            (self.api.set_property)(self.ctx, global, key, value, 0, &mut exception);
            (self.api.string_release)(key);
        }
        if exception.is_null() { Ok(()) } else { Err(self.describe(exception)) }
    }

    fn eval(&self, script: &str) -> Result<Value, String> {
        let source = self.js_string(script)?;
        let mut exception: Value = std::ptr::null();
        // SAFETY: `ctx` is live and `source` is released after the call.
        let value = unsafe {
            let value = (self.api.evaluate)(
                self.ctx,
                source,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                1,
                &mut exception,
            );
            (self.api.string_release)(source);
            value
        };
        if !exception.is_null() {
            self.broken.set(true);
            return Err(self.describe(exception));
        }
        if value.is_null() {
            return Err("the diagram engine returned nothing".to_owned());
        }
        Ok(value)
    }

    fn describe(&self, exception: Value) -> String {
        self.to_rust_string(exception).unwrap_or_else(|_| "the diagram engine failed".to_owned())
    }

    fn to_rust_string(&self, value: Value) -> Result<String, String> {
        let mut exception: Value = std::ptr::null();
        // SAFETY: `value` belongs to `ctx`; the copied string is released below.
        unsafe {
            let s = (self.api.value_to_string)(self.ctx, value, &mut exception);
            if s.is_null() {
                return Err("the diagram engine answered with a non-string".to_owned());
            }
            let size = (self.api.string_max_size)(s);
            let mut buf = vec![0u8; size];
            let written = (self.api.string_utf8)(s, buf.as_mut_ptr().cast(), size);
            (self.api.string_release)(s);
            // `written` counts the trailing NUL.
            let bytes = &buf[..written.saturating_sub(1)];
            Ok(String::from_utf8_lossy(bytes).into_owned())
        }
    }
}

impl Drop for Js {
    fn drop(&mut self) {
        // SAFETY: `pele` was protected in `start` and `ctx` is released once.
        unsafe {
            if !self.pele.is_null() {
                (self.api.value_unprotect)(self.ctx, self.pele);
            }
            (self.api.context_release)(self.ctx);
            (self.api.group_release)(self.group);
        }
    }
}

// ---- resolving CSS variables ----

/// Replaces every `var(--name, fallback)` in Pele's SVG with a plain value and drops the custom
/// property declarations (`--_fg:var(--pele-fg,#1f1f1f)`) that fed them. `--pele-*` names take the
/// palette's value; others follow their declaration; a name with neither takes the fallback.
fn resolve_vars(svg: &str, palette: &Palette) -> String {
    let decls = declarations(svg);
    let mut out = String::with_capacity(svg.len());
    let mut at = 0;
    for &(start, end, _) in &decls {
        out.push_str(&substitute(&svg[at..start], Some(at), &decls, svg, palette, 0));
        at = end;
    }
    out.push_str(&substitute(&svg[at..], Some(at), &decls, svg, palette, 0));
    // resvg ignores `rx` set through `style`; make it an attribute.
    rx_attribute(&out)
}

/// A custom property declaration: byte span in the SVG (with its `;`) and the name.
type Decl<'a> = (usize, usize, &'a str);

/// Finds `--name:value` declarations inside `style="..."` attributes.
fn declarations(svg: &str) -> Vec<Decl<'_>> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(rel) = svg[from..].find("style=\"") {
        let body = from + rel + 7;
        let close = svg[body..].find('"').map_or(svg.len(), |i| body + i);
        let mut at = body;
        while at < close {
            let Some(rel) = svg[at..close].find("--") else { break };
            let start = at + rel;
            let preceded_ok = start == body || svg.as_bytes()[start - 1] == b';';
            let name_end = svg[start..close].find(':').map(|i| start + i);
            match name_end {
                Some(colon) if preceded_ok => {
                    let value = colon + 1;
                    let end = if svg[value..close].starts_with("var(") {
                        value + paren_span(&svg[value..close])
                    } else {
                        svg[value..close].find(';').map_or(close, |i| value + i)
                    };
                    let end = if svg[end..close].starts_with(';') { end + 1 } else { end };
                    found.push((start, end, &svg[start..colon]));
                    at = end;
                }
                _ => at = start + 2,
            }
        }
        from = close.min(svg.len());
        if from >= svg.len() {
            break;
        }
        from += 1;
    }
    found
}

/// Length of the leading `var( ... )` in `s` with balanced parentheses (all of `s` if unbalanced).
fn paren_span(s: &str) -> usize {
    let mut depth = 0usize;
    for (i, b) in s.bytes().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
    }
    s.len()
}

/// Replaces each `var(...)` in `text`, recursing into fallbacks and declarations. `at` is where
/// `text` starts in `svg` when it is the SVG itself: then only a `var(` inside tag markup is
/// replaced, so label text that mentions one (`A[color: var(--accent)]`) stays as written.
/// Values and fallbacks (`at` is `None`) are replaced throughout.
fn substitute(text: &str, at: Option<usize>, decls: &[Decl<'_>], svg: &str, palette: &Palette, depth: usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut offset = at;
    while let Some(i) = rest.find("var(") {
        out.push_str(&rest[..i]);
        let call = &rest[i..];
        let in_tag = offset.is_none_or(|base| {
            let before = &svg[..base + i];
            before.rfind('<') > before.rfind('>')
        });
        if !in_tag {
            out.push_str("var(");
            rest = &call[4..];
            offset = offset.map(|base| base + i + 4);
            continue;
        }
        let len = paren_span(call);
        let inner = call[4..len.saturating_sub(1).max(4)].trim();
        let (name, fallback) = match split_top_level_comma(inner) {
            Some((n, f)) => (n.trim(), Some(f.trim())),
            None => (inner, None),
        };
        out.push_str(&lookup(name, fallback, decls, svg, palette, depth));
        rest = &call[len..];
        offset = offset.map(|base| base + i + len);
    }
    out.push_str(rest);
    out
}

fn split_top_level_comma(s: &str) -> Option<(&str, &str)> {
    let mut depth = 0i32;
    for (i, b) in s.bytes().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b',' if depth == 0 => return Some((&s[..i], &s[i + 1..])),
            _ => {}
        }
    }
    None
}

fn lookup(name: &str, fallback: Option<&str>, decls: &[Decl<'_>], svg: &str, palette: &Palette, depth: usize) -> String {
    if let Some(value) = palette_value(name, palette) {
        return value;
    }
    // Declarations chain at most a few deep; the cap stops a cycle.
    if depth < 8
        && let Some(&(start, end, _)) = decls.iter().rev().find(|d| d.2 == name)
    {
        let value = svg[start + name.len() + 1..end].trim_end_matches(';').trim();
        return substitute(value, None, decls, svg, palette, depth + 1);
    }
    fallback.map(|f| substitute(f, None, decls, svg, palette, depth + 1)).unwrap_or_default()
}

fn palette_value(name: &str, p: &Palette) -> Option<String> {
    let hex = |c: u32| format!("#{:06x}", c & 0xff_ffff);
    Some(match name {
        "--pele-bg" => hex(p.bg),
        "--pele-fg" => hex(p.fg),
        "--pele-line" => hex(p.line),
        "--pele-muted" => hex(p.muted),
        "--pele-surface" => hex(p.surface),
        "--pele-surface-alt" => hex(p.surface_alt),
        "--pele-border" => hex(p.border),
        "--pele-accent" => hex(p.accent),
        // Inside an XML attribute: quotes and ampersands in a family name must be escaped.
        "--pele-font" if !p.font.trim().is_empty() => {
            p.font.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;")
        }
        _ => {
            let n: usize = name.strip_prefix("--pele-series-")?.parse().ok()?;
            hex(*p.series.get(n.checked_sub(1)?)?)
        }
    })
}

/// `style="rx:4px"` to `rx="4"`.
fn rx_attribute(svg: &str) -> String {
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(i) = rest.find("style=\"rx:") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 10..];
        let Some(close) = after.find('"') else { break };
        let value = after[..close].trim_end_matches(';').trim_end_matches("px");
        out.push_str(&format!("rx=\"{value}\""));
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLOWCHART: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" class="pele pele-flowchart" viewBox="0 0 231 77" width="231" height="77" style="max-width:100%;height:auto;--_bg:var(--pele-bg,#fff);--_fg:var(--pele-fg,#1f1f1f);--_m:var(--pele-muted,#6e6e6e);--_l:var(--pele-line,#1f1f1f);--_s:var(--pele-surface,#f3f3f3);--_a:var(--pele-surface-alt,#e6e6e6);--_b:var(--pele-border,#8a8a8a);--_c:var(--pele-accent,#5b7bd5);color:var(--_fg);--_r:var(--pele-radius,4px);--_fm:var(--pele-font-mono,monospace);font:16px sans-serif;letter-spacing:normal;word-spacing:normal;text-transform:none;direction:ltr;font-family:var(--pele-font,Inter)" fill="var(--_fg)" role="graphics-document document" aria-roledescription="flowchart"><g class="pele-edges" fill="none" stroke="var(--_l)" stroke-linecap="round"><g class="pele-edge" data-id="L_A_B_0"><path d="M77.79,38.17L118.79,38.17"/><path class="pele-marker" d="M125.79,38.17L117.79,41.53L117.79,34.81Z" fill="var(--_l)" stroke="none"/></g></g><g class="pele-nodes"><g class="pele-node pele-shape-rect" data-id="A" transform="translate(42.9,38.17)"><rect x="-34.9" y="-24" width="69.79" height="48" fill="var(--_s)" stroke="var(--_b)" style="rx:var(--_r)"/><text class="pele-label" x="0" y="5.6" text-anchor="middle">Start</text></g><g class="pele-node pele-shape-diam" data-id="B" transform="translate(174.06,38.17)"><polygon points="0,-30.17 48.27,0 0,30.17 -48.27,0" fill="var(--_s)" stroke="var(--_b)"/><text class="pele-label" x="0" y="5.6" text-anchor="middle">Ok?</text></g></g></svg>"##;
    const SEQUENCE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" class="pele pele-sequence" viewBox="0 0 187 167" width="187" height="167" style="max-width:100%;height:auto;--_bg:var(--pele-bg,#fff);--_fg:var(--pele-fg,#1f1f1f);--_m:var(--pele-muted,#6e6e6e);--_l:var(--pele-line,#1f1f1f);--_s:var(--pele-surface,#f3f3f3);--_a:var(--pele-surface-alt,#e6e6e6);--_b:var(--pele-border,#8a8a8a);--_c:var(--pele-accent,#5b7bd5);color:var(--_fg);--_r:var(--pele-radius,4px);--_fm:var(--pele-font-mono,monospace);font:16px sans-serif;letter-spacing:normal;word-spacing:normal;text-transform:none;direction:ltr;font-family:var(--pele-font,Inter)" fill="var(--_fg)" role="graphics-document document" aria-roledescription="sequence"><g transform="translate(41.34,8)"><g class="pele-lifelines" fill="none" stroke="var(--_b)"><path class="pele-lifeline" data-id="Alice" d="M0,44V107"/><path class="pele-lifeline" data-id="Bob" d="M105.34,44V107"/></g><g class="pele-edges" fill="none" stroke="var(--_l)" stroke-linecap="round"><g class="pele-edge pele-message" data-id="i0"><path d="M0,87H98.34"/><path class="pele-marker" d="M105.34,87L97.34,90.36L97.34,83.64Z" fill="var(--_l)" stroke="none"/></g></g><g class="pele-edge-labels" font-size="14"><g class="pele-edge-label" data-id="i0"><text x="52.67" y="77.4" text-anchor="middle">Hello</text></g></g><g class="pele-nodes"><g class="pele-node pele-actor pele-actor-participant pele-actor-footer" data-id="Alice"><rect x="-33.34" y="107" width="66.67" height="44" fill="var(--_s)" stroke="var(--_b)" style="rx:var(--_r)"/><text class="pele-label" x="0" y="134.6" text-anchor="middle">Alice</text></g><g class="pele-node pele-actor pele-actor-participant pele-actor-footer" data-id="Bob"><rect x="73.34" y="107" width="64" height="44" fill="var(--_s)" stroke="var(--_b)" style="rx:var(--_r)"/><text class="pele-label" x="105.34" y="134.6" text-anchor="middle">Bob</text></g><g class="pele-node pele-actor pele-actor-participant" data-id="Alice"><rect x="-33.34" y="0" width="66.67" height="44" fill="var(--_s)" stroke="var(--_b)" style="rx:var(--_r)"/><text class="pele-label" x="0" y="27.6" text-anchor="middle">Alice</text></g><g class="pele-node pele-actor pele-actor-participant" data-id="Bob"><rect x="73.34" y="0" width="64" height="44" fill="var(--_s)" stroke="var(--_b)" style="rx:var(--_r)"/><text class="pele-label" x="105.34" y="27.6" text-anchor="middle">Bob</text></g></g></g></svg>"##;

    fn palette() -> Palette {
        Palette {
            bg: 0x101820,
            fg: 0xe6e1d9,
            line: 0xa0b4c8,
            muted: 0x8a8f98,
            surface: 0x1c2733,
            surface_alt: 0x243242,
            border: 0x3b4a5c,
            accent: 0xff7a59,
            font: "Inter, sans-serif".to_owned(),
            series: [0x111111, 0x222222, 0x333333, 0x444444, 0x555555, 0x666666, 0x777777, 0x888888],
        }
    }

    #[test]
    fn resolves_every_var_in_real_pele_output() {
        for svg in [FLOWCHART, SEQUENCE] {
            assert!(svg.contains("var(--_"), "fixture should be unresolved Pele output");
            let out = resolve_vars(svg, &palette());
            assert!(!out.contains("var("), "left a var(): {out}");
            assert!(!out.contains("--_"), "left a declaration: {out}");
            assert!(!out.contains("style=\"rx"));
            // Shape fills come from the surface, text from the ink, edges from the line.
            assert!(out.contains("#1c2733"), "no surface fill: {out}");
            assert!(out.contains("#e6e1d9"), "no ink: {out}");
            assert!(out.contains("#a0b4c8"), "no line stroke: {out}");
            // None of Pele's own defaults survive.
            for default in ["#1f1f1f", "#f3f3f3", "#8a8a8a", "#5b7bd5"] {
                assert!(!out.contains(default), "kept default {default}: {out}");
            }
            assert!(out.contains("font-family:Inter, sans-serif"), "font not set: {out}");
        }
    }

    #[test]
    fn fallback_applies_when_the_palette_has_no_value() {
        let svg = r##"<svg style="--_r:var(--pele-radius,4px);--_x:var(--pele-nothing,#123456)" rx="var(--_r)" fill="var(--_x)" stroke="var(--missing,red)"/>"##;
        let out = resolve_vars(svg, &palette());
        assert_eq!(out, r##"<svg style="" rx="4px" fill="#123456" stroke="red"/>"##);
    }

    #[test]
    fn label_text_that_mentions_var_is_left_alone() {
        let svg = r##"<text fill="var(--pele-fg)">color: var(--pele-accent) and var(</text><path fill="var(--pele-bg)"/>"##;
        let out = resolve_vars(svg, &palette());
        assert!(out.contains(">color: var(--pele-accent) and var(</text>"), "{out}");
        assert!(!out.contains(r#"fill="var("#), "{out}");
        assert!(out.ends_with("/>") && out.contains("<path fill=\"#"), "{out}");
    }

    #[test]
    fn series_colors_come_from_the_palette() {
        let out = resolve_vars(r##"<g fill="var(--pele-series-3,#abcdef)" stroke="var(--pele-series-9,#abcdef)"/>"##, &palette());
        assert_eq!(out, r##"<g fill="#333333" stroke="#abcdef"/>"##);
    }

    #[test]
    fn renders_a_flowchart_through_javascriptcore() {
        let d = render("flowchart LR\n A[Start] --> B{Ok?}\n B -->|yes| C[Done]", 600.0, &palette(), "t1").expect("renders");
        assert!(loaded());
        assert!(d.width > 50.0 && d.height > 30.0, "{}x{}", d.width, d.height);
        assert!(d.svg.starts_with("<svg"));
        assert!(!d.svg.contains("var("));
        assert!(d.svg.contains("#1c2733"));
        assert!(d.svg.contains("Start"));
    }

    #[test]
    fn a_bad_source_is_an_error_message() {
        let err = render("this is not a diagram", 600.0, &palette(), "t2").expect_err("no diagram type");
        assert!(err.contains("No diagram type"), "{err}");
        // The engine keeps working after an error.
        render("flowchart LR\n A --> B", 600.0, &palette(), "t3").expect("renders after an error");
    }
}
