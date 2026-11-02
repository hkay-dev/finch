//! A real `NSMenu` for the explorer's menus (Harrier's `native_menu.rs` is the model). A list of
//! `Entry`s becomes menu items (a leading SF Symbol or file icon, a key equivalent, a check mark,
//! an Option alternate, submenus, separators) and `popUpMenuPositioningItem:atLocation:inView:`
//! shows it, so the material, radius, row height, hover, pick blink and submenu timing are the
//! system's. The call blocks inside its own tracking loop and returns when the menu closes, so
//! call it from a task, not from inside an update; nothing draws while it is open.
//!
//! The menu takes the look of the window it opens over (the key window, else the one under the
//! pointer), which follows the app's theme. Separators only part two groups: none leads or ends a
//! menu and two never meet. `\r` and the other special keys are set by hand so AppKit draws the
//! glyph (Return) and not the letter.
//!
//! `GPUI_EDITOR_MENU_DUMP=<file>` appends each menu built to that file, one item per line as
//! `<indent>title<TAB>key<TAB>state`, and shows nothing, so a background launch (where native
//! menus can't open) can check what a menu holds. State is `enabled` or `disabled`, plus
//! `,checked`, `,alternate` and `,header` (a section title).
//!
//! The module also holds the system services the menus' items reach: the apps that open a file,
//! Show in Finder and Quick Look.
//!
//! Written against the Objective-C runtime without AppKit's typed bindings, as `menu.rs` and
//! `symbols.rs` are.

use std::{
    cell::RefCell,
    ffi::{CStr, CString, c_char, c_void},
    io::Write as _,
    path::{Path, PathBuf},
    sync::{
        Once,
        atomic::{AtomicIsize, Ordering},
    },
};

use objc2::{
    ClassType, class, define_class, msg_send,
    rc::{Allocated, Retained},
    runtime::{AnyClass, AnyObject, Bool, NSObject, Sel},
    sel,
};

use crate::window_look::{Point, Size};

pub const SHIFT: usize = 1 << 17;
pub const CONTROL: usize = 1 << 18;
pub const OPTION: usize = 1 << 19;
pub const COMMAND: usize = 1 << 20;

/// The side of a file icon in a menu, in points.
const ICON_SIDE: f64 = 16.;

/// The tag of the item the user chose, or -1.
static CHOSEN: AtomicIsize = AtomicIsize::new(-1);

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements and the class has no ivars.
    #[unsafe(super(NSObject))]
    #[name = "GPUIEditorMenuTarget"]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(fire:))]
        fn fire(&self, sender: &AnyObject) {
            let tag: isize = unsafe { msg_send![sender, tag] };
            CHOSEN.store(tag, Ordering::SeqCst);
        }
    }
);

/// An item's leading image.
#[derive(Clone)]
pub enum Image {
    /// An SF Symbol, by name.
    Symbol(&'static str),
    /// The icon Finder shows for the file or folder at this path.
    Icon(PathBuf),
}

/// One command of a menu; `run` is what `pop_up` hands back when it is chosen.
#[derive(Clone)]
pub struct Item<T> {
    pub title: String,
    pub image: Option<Image>,
    /// The key equivalent and its modifiers, written on the item.
    pub key: Option<(&'static str, usize)>,
    pub checked: bool,
    pub disabled: bool,
    /// Shown in place of the item before it while Option is held.
    pub alternate: bool,
    pub run: T,
}

impl<T> Item<T> {
    pub fn new(title: impl Into<String>, run: T) -> Self {
        Self { title: title.into(), image: None, key: None, checked: false, disabled: false, alternate: false, run }
    }

    pub fn symbol(mut self, name: &'static str) -> Self {
        self.image = Some(Image::Symbol(name));
        self
    }

    pub fn icon(mut self, path: PathBuf) -> Self {
        self.image = Some(Image::Icon(path));
        self
    }

    pub fn key(mut self, key: &'static str, modifiers: usize) -> Self {
        self.key = Some((key, modifiers));
        self
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    /// Greyed out and not pickable, like Cut with nothing selected.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Takes the place of the item before it while Option is held.
    pub fn alternate(mut self) -> Self {
        self.alternate = true;
        self.key = Some(("", OPTION));
        self
    }
}

#[derive(Clone)]
pub enum Entry<T> {
    Item(Item<T>),
    Separator,
    /// A grey section title that picks nothing, like "Recent Searches" in a search field's menu: AppKit's
    /// section header where the system has one (macOS 14 and later), else a greyed item.
    Header(String),
    Submenu { title: String, image: Option<Image>, entries: Vec<Entry<T>> },
}

fn string(text: &str) -> Retained<AnyObject> {
    let text = CString::new(text.replace('\0', "")).unwrap_or_default();
    unsafe { msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()] }
}

fn text(string: &AnyObject) -> String {
    let chars: *const c_char = unsafe { msg_send![string, UTF8String] };
    if chars.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(chars) }.to_string_lossy().into_owned()
}

fn lookup_range(state: &gpui_kit::component::input::EditorState) -> Option<std::ops::Range<usize>> {
    let has_word = |range: &std::ops::Range<usize>| {
        state.text().slice(range.clone()).chars().any(char::is_alphanumeric)
    };
    let selection = state.selected_range();
    if !selection.is_empty() {
        return has_word(&selection).then_some(selection);
    }
    [Some(state.cursor()), state.cursor().checked_sub(1)]
        .into_iter()
        .flatten()
        .filter_map(|offset| gpui_kit::base::text_boundary::word_range_in_rope(state.text().slice(..), offset))
        .find(has_word)
}

pub fn lookup_title(state: &gpui_kit::component::input::EditorState) -> Option<String> {
    let range = lookup_range(state)?;
    let slice = state.text().slice(range);
    let mut chars = slice.chars().map(|character| if character.is_whitespace() { ' ' } else { character });
    let label: String = chars.by_ref().take(20).collect();
    Some(format!("Look up “{}{}”", label.trim(), if chars.next().is_some() { "…" } else { "" }))
}

pub fn look_up(state: &gpui_kit::component::input::EditorState, window: &gpui_kit::Window, cx: &gpui_kit::App) {
    use gpui_kit::component::ActiveTheme as _;
    let Some(range) = lookup_range(state) else { return };
    let Some(origin) = state.text_baseline_for_range(range.clone()) else { return };
    let settings = crate::settings::get(cx);
    let theme = cx.theme();
    let drawn = settings.editor.drawn_family(cx);
    let family = drawn.as_deref().unwrap_or(theme.mono_font_family.as_ref());
    let size = settings.editor.font_size.map(gpui_kit::px).unwrap_or(theme.mono_font_size);
    window.show_definition(&state.text().slice(range).to_string(), origin, family, size);
}

/// A background control probe, using only the service's supplied private pasteboard.
pub fn probe_services(view: &AnyObject, replacement: Option<&str>) -> String {
    unsafe {
        let kind = string("public.utf8-plain-text");
        let types: Retained<AnyObject> = msg_send![class!(NSArray), arrayWithObject: &*kind];
        let pasteboard: Retained<AnyObject> = msg_send![class!(NSPasteboard), pasteboardWithUniqueName];
        let send: *mut AnyObject = msg_send![view,
            validRequestorForSendType: &*kind, returnType: std::ptr::null::<AnyObject>()];
        let receive: *mut AnyObject = msg_send![view,
            validRequestorForSendType: std::ptr::null::<AnyObject>(), returnType: &*kind];
        let both: *mut AnyObject = msg_send![view,
            validRequestorForSendType: &*kind, returnType: &*kind];
        let written: Bool = msg_send![view,
            writeSelectionToPasteboard: &*pasteboard, types: &*types];
        let selected: Option<Retained<AnyObject>> = msg_send![&*pasteboard, stringForType: &*kind];
        let selected = selected.as_deref().map(text);
        let read = replacement.map(|replacement| {
            let _: isize = msg_send![&*pasteboard, clearContents];
            let _: Bool = msg_send![&*pasteboard, setString: &*string(replacement), forType: &*kind];
            let read: Bool = msg_send![view, readSelectionFromPasteboard: &*pasteboard];
            read.as_bool()
        });
        let _: () = msg_send![&*pasteboard, releaseGlobally];
        format!(
            "send={} receive={} both={} written={} text={selected:?} read={read:?}",
            !send.is_null(), !receive.is_null(), !both.is_null(), written.as_bool(),
        )
    }
}

/// Shows `entries` as a menu and returns what the user chose, or `None` when it closed without a
/// choice. `at` is where the menu's top left goes, in points from the window's top left; without
/// it the menu opens at the pointer. With `GPUI_EDITOR_MENU_DUMP` set the menu is written to that
/// file and not shown.
pub fn pop_up<T: Clone>(entries: &[Entry<T>], at: Option<(f32, f32)>) -> Option<T> {
    let target: Retained<AnyObject> = unsafe { msg_send![MenuTarget::class(), new] };
    let mut choices = Vec::new();
    let menu = build(entries, &target, &mut choices);
    if let Some(path) = std::env::var_os("GPUI_EDITOR_MENU_DUMP") {
        dump(&menu, Path::new(&path));
        return None;
    }
    let pointer: Point = unsafe { msg_send![class!(NSEvent), mouseLocation] };
    let mut location = pointer;
    unsafe {
        let app: Retained<AnyObject> = msg_send![class!(NSApplication), sharedApplication];
        // The key window, else (a click on a window while another app is active, which doesn't
        // activate it) the window under the pointer.
        let mut window: Option<Retained<AnyObject>> = msg_send![&*app, keyWindow];
        if window.is_none() {
            let number: isize = msg_send![class!(NSWindow), windowNumberAtPoint: pointer, belowWindowWithWindowNumber: 0isize];
            window = msg_send![&*app, windowWithWindowNumber: number];
        }
        if let Some(window) = window {
            let appearance: Retained<AnyObject> = msg_send![&*window, effectiveAppearance];
            let _: () = msg_send![&*menu, setAppearance: &*appearance];
            if let Some((x, y)) = at {
                let view: Retained<AnyObject> = msg_send![&*window, contentView];
                let bounds: crate::window_look::Rect = msg_send![&*view, bounds];
                let in_window = Point { x: f64::from(x), y: bounds.size.height - f64::from(y) };
                location = msg_send![&*window, convertPointToScreen: in_window];
            }
        }
    }
    CHOSEN.store(-1, Ordering::SeqCst);
    let _: Bool = unsafe {
        msg_send![
            &*menu,
            popUpMenuPositioningItem: None::<&AnyObject>,
            atLocation: location,
            inView: None::<&AnyObject>
        ]
    };
    usize::try_from(CHOSEN.swap(-1, Ordering::SeqCst)).ok().and_then(|tag| choices.into_iter().nth(tag))
}

fn new_item(title: &str, action: Option<Sel>) -> Retained<AnyObject> {
    let alloc: Allocated<AnyObject> = unsafe { msg_send![class!(NSMenuItem), alloc] };
    unsafe { msg_send![alloc, initWithTitle: &*string(title), action: action, keyEquivalent: &*string("")] }
}

fn build<T: Clone>(entries: &[Entry<T>], target: &AnyObject, choices: &mut Vec<T>) -> Retained<AnyObject> {
    let menu: Retained<AnyObject> = unsafe { msg_send![class!(NSMenu), new] };
    // Every item is enabled unless the caller greyed it (`Item::disabled`).
    let _: () = unsafe { msg_send![&*menu, setAutoenablesItems: Bool::NO] };
    // Separators only ever part two groups: none leads a menu, two never meet, and one left
    // last is taken off below.
    let mut open = true;
    for entry in entries {
        let item: Retained<AnyObject> = match entry {
            Entry::Separator if open => continue,
            Entry::Separator => unsafe { msg_send![class!(NSMenuItem), separatorItem] },
            Entry::Header(title) => unsafe {
                let has: Bool = msg_send![class!(NSMenuItem), respondsToSelector: sel!(sectionHeaderWithTitle:)];
                if has.as_bool() {
                    msg_send![class!(NSMenuItem), sectionHeaderWithTitle: &*string(title)]
                } else {
                    let item = new_item(title, None);
                    let _: () = msg_send![&*item, setEnabled: Bool::NO];
                    item
                }
            },
            Entry::Item(Item { title, image, key, checked, disabled, alternate, run }) => {
                let item = new_item(title, Some(sel!(fire:)));
                unsafe {
                    let _: () = msg_send![&*item, setTarget: target];
                    let _: () = msg_send![&*item, setTag: choices.len() as isize];
                    if let Some((key, modifiers)) = key {
                        let _: () = msg_send![&*item, setKeyEquivalent: &*string(key)];
                        let _: () = msg_send![&*item, setKeyEquivalentModifierMask: *modifiers];
                    }
                    if *alternate {
                        let _: () = msg_send![&*item, setAlternate: Bool::YES];
                    }
                    if *checked {
                        let _: () = msg_send![&*item, setState: 1isize];
                    }
                    if *disabled {
                        let _: () = msg_send![&*item, setEnabled: Bool::NO];
                    }
                }
                choices.push(run.clone());
                set_image(&item, image.as_ref());
                item
            }
            Entry::Submenu { title, image, entries } => {
                let sub = build(entries, target, choices);
                // A submenu whose items all left has nothing to open.
                let count: isize = unsafe { msg_send![&*sub, numberOfItems] };
                if count == 0 {
                    continue;
                }
                let item = new_item(title, None);
                unsafe {
                    let _: () = msg_send![&*sub, setTitle: &*string(title)];
                    let _: () = msg_send![&*item, setSubmenu: &*sub];
                }
                set_image(&item, image.as_ref());
                item
            }
        };
        // True at the start and after a separator, where another separator would part nothing.
        open = matches!(entry, Entry::Separator);
        let _: () = unsafe { msg_send![&*menu, addItem: &*item] };
    }
    let count: isize = unsafe { msg_send![&*menu, numberOfItems] };
    if open && count > 0 {
        let _: () = unsafe { msg_send![&*menu, removeItemAtIndex: count - 1] };
    }
    menu
}

/// Puts the image on the item and makes it show where the system hides menu images by default
/// (macOS 27 and later; earlier systems show it anyway).
fn set_image(item: &AnyObject, image: Option<&Image>) {
    let Some(image) = image.and_then(|image| match image {
        Image::Symbol(name) => symbol(name),
        Image::Icon(path) => file_icon(path),
    }) else {
        return;
    };
    unsafe {
        let _: () = msg_send![item, setImage: &*image];
        let has: Bool = msg_send![item, respondsToSelector: sel!(setPreferredImageVisibility:)];
        if has.as_bool() {
            let _: () = msg_send![item, setPreferredImageVisibility: 1isize];
        }
    }
}

/// Puts an SF Symbol on a menu item built elsewhere (the menu bar's, which GPUI makes without images).
pub fn set_symbol(item: &AnyObject, name: &'static str) {
    set_image(item, Some(&Image::Symbol(name)));
}

fn symbol(name: &str) -> Option<Retained<AnyObject>> {
    unsafe {
        msg_send![
            class!(NSImage),
            imageWithSystemSymbolName: &*string(name),
            accessibilityDescription: None::<&AnyObject>
        ]
    }
}

/// Finder's icon for `path`, a copy sized for a menu (the workspace shares its own).
fn file_icon(path: &Path) -> Option<Retained<AnyObject>> {
    unsafe {
        let workspace: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
        let icon: Option<Retained<AnyObject>> = msg_send![&*workspace, iconForFile: &*string(&path.to_string_lossy())];
        let icon: Retained<AnyObject> = msg_send![&*icon?, copy];
        let _: () = msg_send![&*icon, setSize: Size { width: ICON_SIDE, height: ICON_SIDE }];
        Some(icon)
    }
}

/// Appends `menu` to the file at `path` (see the module docs for the format).
fn dump(menu: &AnyObject, path: &Path) {
    let mut out = String::from("== menu\n");
    walk(menu, 0, &mut out);
    let written = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut file| file.write_all(out.as_bytes()));
    if let Err(err) = written {
        eprintln!("finch: can't write {}: {err}", path.display());
    }
}

fn walk(menu: &AnyObject, depth: usize, out: &mut String) {
    let count: isize = unsafe { msg_send![menu, numberOfItems] };
    for index in 0..count {
        let item: Retained<AnyObject> = unsafe { msg_send![menu, itemAtIndex: index] };
        let indent = "  ".repeat(depth);
        let separator: Bool = unsafe { msg_send![&*item, isSeparatorItem] };
        if separator.as_bool() {
            out.push_str(&format!("{indent}---\n"));
            continue;
        }
        let responds: Bool = unsafe { msg_send![&*item, respondsToSelector: sel!(isSectionHeader)] };
        let header = responds.as_bool() && {
            let header: Bool = unsafe { msg_send![&*item, isSectionHeader] };
            header.as_bool()
        };
        let title: Retained<AnyObject> = unsafe { msg_send![&*item, title] };
        let key: Retained<AnyObject> = unsafe { msg_send![&*item, keyEquivalent] };
        let modifiers: usize = unsafe { msg_send![&*item, keyEquivalentModifierMask] };
        let state: isize = unsafe { msg_send![&*item, state] };
        let alternate: Bool = unsafe { msg_send![&*item, isAlternate] };
        let key = text(&key);
        let keys = if key.is_empty() && !alternate.as_bool() {
            String::new()
        } else {
            let flags: String = [(CONTROL, '\u{2303}'), (OPTION, '\u{2325}'), (SHIFT, '\u{21E7}'), (COMMAND, '\u{2318}')]
                .iter()
                .filter(|(bit, _)| modifiers & bit != 0)
                .map(|(_, glyph)| *glyph)
                .collect();
            let key = match key.as_str() {
                "\r" => "\u{21A9}".to_owned(),
                "\u{8}" => "\u{232B}".to_owned(),
                "\u{F700}" => "\u{2191}".to_owned(),
                "\u{F701}" => "\u{2193}".to_owned(),
                "\u{F702}" => "\u{2190}".to_owned(),
                "\u{F703}" => "\u{2192}".to_owned(),
                " " => "Space".to_owned(),
                other => other.to_uppercase(),
            };
            format!("{flags}{key}")
        };
        let enabled: Bool = unsafe { msg_send![&*item, isEnabled] };
        let state = format!(
            "{}{}{}{}",
            if enabled.as_bool() { "enabled" } else { "disabled" },
            if state == 1 { ",checked" } else { "" },
            if alternate.as_bool() { ",alternate" } else { "" },
            if header { ",header" } else { "" }
        );
        out.push_str(&format!("{indent}{}\t{keys}\t{state}\n", text(&title)));
        let sub: Option<Retained<AnyObject>> = unsafe { msg_send![&*item, submenu] };
        if let Some(sub) = sub {
            walk(&sub, depth + 1, out);
        }
    }
}

/// Finder's name for the file or folder at `path` ("Macintosh HD" for `/`).
pub fn display_name(path: &Path) -> String {
    unsafe {
        let manager: Retained<AnyObject> = msg_send![class!(NSFileManager), defaultManager];
        let name: Retained<AnyObject> = msg_send![&*manager, displayNameAtPath: &*string(&path.to_string_lossy())];
        text(&name)
    }
}

// ---- System services behind the items ----

/// An app that opens a file.
pub struct App {
    pub name: String,
    pub path: PathBuf,
    /// The app Finder opens the file with.
    pub is_default: bool,
}

fn file_url(path: &Path) -> Retained<AnyObject> {
    unsafe { msg_send![class!(NSURL), fileURLWithPath: &*string(&path.to_string_lossy())] }
}

fn url_path(url: &AnyObject) -> PathBuf {
    let path: Retained<AnyObject> = unsafe { msg_send![url, path] };
    PathBuf::from(text(&path))
}

/// The apps that open the file at `path`, the default first and the rest in Finder's order,
/// leaving out this app.
pub fn apps_for(path: &Path) -> Vec<App> {
    let (found, default): (Retained<AnyObject>, Option<Retained<AnyObject>>) = unsafe {
        let workspace: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
        let url = file_url(path);
        (msg_send![&*workspace, URLsForApplicationsToOpenURL: &*url], msg_send![&*workspace, URLForApplicationToOpenURL: &*url])
    };
    let own = std::env::current_exe().ok();
    let default = default.map(|url| url_path(&url));
    let count: usize = unsafe { msg_send![&*found, count] };
    let mut paths: Vec<PathBuf> = Vec::new();
    for index in 0..count {
        let url: Retained<AnyObject> = unsafe { msg_send![&*found, objectAtIndex: index] };
        let app = url_path(&url);
        if !paths.contains(&app) && !own.as_deref().is_some_and(|own| own.starts_with(&app)) {
            paths.push(app);
        }
    }
    let mut apps: Vec<App> = paths
        .into_iter()
        .map(|path| App { name: display_name(&path), is_default: default.as_ref() == Some(&path), path })
        .collect();
    apps.sort_by(|a, b| b.is_default.cmp(&a.is_default).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    // Several copies of one app (builds in other folders) list once.
    let mut seen = std::collections::HashSet::new();
    apps.retain(|app| seen.insert(app.name.clone()));
    apps
}

/// Opens `file` with the app at `app`.
pub fn open_with(file: &Path, app: &Path) {
    unsafe {
        let workspace: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
        let _: Bool = msg_send![&*workspace, openFile: &*string(&file.to_string_lossy()), withApplication: &*string(&app.to_string_lossy())];
    }
}

/// Selects `path` in a Finder window.
pub fn show_in_finder(path: &Path) {
    show_all_in_finder(std::slice::from_ref(&path.to_path_buf()));
}

/// Selects every one of `paths` in one Finder window (the folders they sit in open together).
pub fn show_all_in_finder(paths: &[PathBuf]) {
    unsafe {
        let workspace: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
        let urls: Retained<AnyObject> = msg_send![class!(NSMutableArray), new];
        for path in paths {
            let _: () = msg_send![&*urls, addObject: &*file_url(path)];
        }
        let _: () = msg_send![&*workspace, activateFileViewerSelectingURLs: &*urls];
    }
}

/// The menu under a document's title in Finder and TextEdit: the file, then each folder up to the disk, each with its Finder icon.
/// What a pick hands back is what to select in Finder: the file for itself, the next one down the path for a folder, so the folder
/// opens with that one selected, as the Mac's title menu does.
pub fn path_menu(path: &Path) -> Vec<Entry<PathBuf>> {
    let chain: Vec<&Path> = path.ancestors().filter(|part| !part.as_os_str().is_empty()).collect();
    chain
        .iter()
        .enumerate()
        .map(|(ix, part)| {
            let reveal = if ix == 0 { *part } else { chain[ix - 1] };
            Entry::Item(Item::new(display_name(part), reveal.to_path_buf()).icon(part.to_path_buf()))
        })
        .collect()
}

thread_local! {
    /// The file the Quick Look panel shows, and the data source that hands it over (the panel
    /// keeps its data source weakly).
    static PREVIEW: RefCell<Option<(Retained<AnyObject>, Retained<AnyObject>)>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements and the class has no ivars.
    #[unsafe(super(NSObject))]
    #[name = "GPUIEditorPreviewSource"]
    struct PreviewSource;

    impl PreviewSource {
        #[unsafe(method(numberOfPreviewItemsInPreviewPanel:))]
        fn count(&self, _panel: &AnyObject) -> isize {
            PREVIEW.with_borrow(|shown| isize::from(shown.is_some()))
        }

        #[unsafe(method_id(previewPanel:previewItemAtIndex:))]
        fn item(&self, _panel: &AnyObject, _index: isize) -> Option<Retained<AnyObject>> {
            PREVIEW.with_borrow(|shown| shown.as_ref().map(|(url, _)| url.clone()))
        }
    }
);

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
}

/// Quick Look's panel for `path`: shows it, or closes the panel when it already shows that file.
/// Returns false on a Mac with no Quick Look panel.
pub fn quick_look(path: &Path) -> bool {
    static LOAD: Once = Once::new();
    LOAD.call_once(|| {
        // SAFETY: a plain `dlopen` of a system framework by path; the handle is never closed.
        unsafe { dlopen(c"/System/Library/Frameworks/QuickLookUI.framework/QuickLookUI".as_ptr(), 1) };
    });
    let Some(class) = AnyClass::get(c"QLPreviewPanel") else {
        return false;
    };
    unsafe {
        let panel: Retained<AnyObject> = msg_send![class, sharedPreviewPanel];
        let visible: Bool = msg_send![&*panel, isVisible];
        let url = file_url(path);
        let same = PREVIEW.with_borrow(|shown| shown.as_ref().is_some_and(|(shown, _)| url_path(shown) == path));
        if visible.as_bool() && same {
            let _: () = msg_send![&*panel, orderOut: None::<&AnyObject>];
            return true;
        }
        let source = PREVIEW.with_borrow(|shown| shown.as_ref().map(|(_, source)| source.clone()));
        let source = source.unwrap_or_else(|| msg_send![PreviewSource::class(), new]);
        PREVIEW.with_borrow_mut(|shown| *shown = Some((url, source.clone())));
        let _: () = msg_send![&*panel, setDataSource: &*source];
        let _: () = msg_send![&*panel, reloadData];
        // A test launch (`GPUI_EDITOR_BACKGROUND`) never takes the keyboard (gpui-editor#pcck) and
        // never shows over another app (finch#jxb8).
        if std::env::var_os("GPUI_EDITOR_BACKGROUND").is_some() {
            crate::window_look::keep_under_everything(&panel);
            let _: () = msg_send![&*panel, orderBack: None::<&AnyObject>];
        } else {
            let _: () = msg_send![&*panel, makeKeyAndOrderFront: None::<&AnyObject>];
        }
    }
    true
}

/// The paper margin on every side, in points (three quarters of an inch).
const PRINT_MARGIN: f64 = 54.;

/// Spaces a tab stands for on paper.
const PRINT_TAB_COLUMNS: f64 = 4.;

/// File > Print: prints `text` as plain black type on white (`family` at `size` points, wrapped
/// to the printable width, no line numbers or colours, whatever the theme) through an offscreen
/// `NSTextView`. With `pdf` the job saves to that file and no panel shows (the test path); with
/// `window_view` (GPUI's view) the system's print panel opens as a sheet on its window, and the
/// job runs when the user confirms.
pub fn print_text(text: &str, family: &str, size: f64, window_view: Option<&AnyObject>, pdf: Option<&Path>) -> Result<(), String> {
    use crate::window_look::Rect;
    // SAFETY: AppKit text and print calls on the main thread, where actions and verbs run.
    unsafe {
        let shared: Retained<AnyObject> = msg_send![class!(NSPrintInfo), sharedPrintInfo];
        let info: Retained<AnyObject> = msg_send![&*shared, copy];
        let _: () = msg_send![&*info, setLeftMargin: PRINT_MARGIN];
        let _: () = msg_send![&*info, setRightMargin: PRINT_MARGIN];
        let _: () = msg_send![&*info, setTopMargin: PRINT_MARGIN];
        let _: () = msg_send![&*info, setBottomMargin: PRINT_MARGIN];
        let paper: Size = msg_send![&*info, paperSize];
        let width = (paper.width - 2. * PRINT_MARGIN).max(72.);

        let font: Option<Retained<AnyObject>> = msg_send![class!(NSFont), fontWithName: &*string(family), size: size];
        let font = match font {
            Some(font) => font,
            None => msg_send![class!(NSFont), monospacedSystemFontOfSize: size, weight: 0f64],
        };
        let black: Retained<AnyObject> = msg_send![class!(NSColor), blackColor];
        let white: Retained<AnyObject> = msg_send![class!(NSColor), whiteColor];
        let aqua: Option<Retained<AnyObject>> = msg_send![class!(NSAppearance), appearanceNamed: &*string("NSAppearanceNameAqua")];

        // A tab is four columns of the font, measured from a "0".
        let attributes: Retained<AnyObject> = msg_send![class!(NSDictionary), dictionaryWithObject: &*font, forKey: &*string("NSFont")];
        let zero: Size = msg_send![&*string("0"), sizeWithAttributes: &*attributes];
        let style: Retained<AnyObject> = msg_send![class!(NSMutableParagraphStyle), new];
        let _: () = msg_send![&*style, setDefaultTabInterval: zero.width * PRINT_TAB_COLUMNS];

        let alloc: Allocated<AnyObject> = msg_send![class!(NSTextView), alloc];
        let frame = Rect { origin: Point { x: 0., y: 0. }, size: Size { width, height: 1. } };
        let view: Retained<AnyObject> = msg_send![alloc, initWithFrame: frame];
        let container: Retained<AnyObject> = msg_send![&*view, textContainer];
        let _: () = msg_send![&*container, setWidthTracksTextView: Bool::YES];
        let _: () = msg_send![&*view, setHorizontallyResizable: Bool::NO];
        let _: () = msg_send![&*view, setVerticallyResizable: Bool::YES];
        let _: () = msg_send![&*view, setRichText: Bool::NO];
        let _: () = msg_send![&*view, setDrawsBackground: Bool::YES];
        let _: () = msg_send![&*view, setBackgroundColor: &*white];
        let _: () = msg_send![&*view, setAppearance: aqua.as_deref()];
        let _: () = msg_send![&*view, setDefaultParagraphStyle: &*style];
        let _: () = msg_send![&*view, setString: &*string(text)];
        let _: () = msg_send![&*view, setFont: &*font];
        let _: () = msg_send![&*view, setTextColor: &*black];
        let _: () = msg_send![&*view, sizeToFit];

        // The operation copies the info, so a PDF job's disposition and file go on it first.
        if let Some(pdf) = pdf {
            let _: () = msg_send![&*info, setJobDisposition: &*string("NSPrintSaveJob")];
            let settings: Retained<AnyObject> = msg_send![&*info, dictionary];
            let _: () = msg_send![&*settings, setObject: &*file_url(pdf), forKey: &*string("NSJobSavingURL")];
        }
        let operation: Retained<AnyObject> = msg_send![class!(NSPrintOperation), printOperationWithView: &*view, printInfo: &*info];
        if pdf.is_some() {
            let _: () = msg_send![&*operation, setShowsPrintPanel: Bool::NO];
            let _: () = msg_send![&*operation, setShowsProgressPanel: Bool::NO];
            let done: Bool = msg_send![&*operation, runOperation];
            return if done.as_bool() { Ok(()) } else { Err("the print job failed".to_string()) };
        }
        let native: Option<Retained<AnyObject>> = match window_view {
            Some(view) => msg_send![view, window],
            None => None,
        };
        let native = native.ok_or("no native window to print from")?;
        // The sheet outlives this call; AppKit keeps the operation until the job ends.
        let _: () = msg_send![
            &*operation,
            runOperationModalForWindow: &*native,
            delegate: None::<&AnyObject>,
            didRunSelector: None::<Sel>,
            contextInfo: std::ptr::null_mut::<c_void>()
        ];
        Ok(())
    }
}

/// The gutter's right-click menu: one row, "Show line numbers", checked while the numbers show
/// (Xcode's gutter menu). Returns true when the row was picked. Blocks like `pop_up`, so call it
/// from a task.
pub fn gutter_menu(shown: bool) -> bool {
    pop_up(&[Entry::Item(Item::new("Show line numbers", ()).checked(shown))], None).is_some()
}
