//! The settings file: `~/Library/Application Support/Finch/settings.toml`.
//!
//! Read at launch, written when something changes, and read again when another program edits the
//! file (`watch`). A missing file or
//! missing keys mean defaults. A file that doesn't parse keeps the defaults, says where the
//! mistake is in an alert, and is never written over. Each feature owns a section (`[window]`,
//! `[editor]`, `[text]`, `[layout]`, `[explorer]`, `[preview]`, `[scratch]`, `[code_colors]`); a write replaces only the
//! sections this file knows and leaves the rest of the file as it was.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

use gpui_kit::*;
use serde::{Deserialize, Serialize};

use crate::tokens::{EDITOR_MIN, PANEL_WIDTH_MIN, WINDOW_MIN_HEIGHT};

/// How long after another program's edit a save leaves the file for the watcher to adopt.
const ADOPT_WAIT: Duration = Duration::from_secs(2);

/// How long a change waits for the next one before it is written.
const WRITE_DELAY: Duration = Duration::from_millis(500);

/// The editor font's range in points (the file and View > Bigger and Smaller keep to it).
pub const FONT_LEAST: f32 = 6.;
pub const FONT_MOST: f32 = 96.;
/// The size View > Actual size goes back to.
pub const FONT_USUAL: f32 = 14.;

/// The editor font size after View > Bigger (`up`) or Smaller: a whole number of points, one at a
/// time up to 24 as TextEdit and Xcode go, two at a time above where one point is hard to see, and
/// kept within the range. A size from a hand edit (a 13.5) first lands on a whole number.
pub fn font_step(size: f32, up: bool) -> f32 {
    let size = size.round();
    let next = if up {
        size + if size < 24. { 1. } else { 2. }
    } else {
        size - if size <= 24. { 1. } else { 2. }
    };
    next.clamp(FONT_LEAST, FONT_MOST)
}

impl Settings {
    /// Brings numbers a renderer can't use into range: the editor font 6 to 96 points, text weight
    /// 100 to 900, gamma 0.2 to 3.0, contrast 0 to 1, and a window at least as large as it can be
    /// resized to. A value that isn't a finite number becomes the usual one.
    fn sane(&mut self) {
        let usual = Self::default();
        let least_width = f32::from(PANEL_WIDTH_MIN + EDITOR_MIN);
        let least_height = f32::from(WINDOW_MIN_HEIGHT);
        self.window.width = within(self.window.width, least_width, f32::MAX, usual.window.width);
        self.window.height = within(self.window.height, least_height, f32::MAX, usual.window.height);
        if let (Some(size), Some(usual)) = (&mut self.editor.font_size, usual.editor.font_size) {
            *size = within(*size, FONT_LEAST, FONT_MOST, usual);
        }
        self.text.weight = within(self.text.weight, 100., 900., usual.text.weight);
        self.text.gamma = within(self.text.gamma, 0.2, 3., usual.text.gamma);
        self.text.contrast = within(self.text.contrast, 0., 1., usual.text.contrast);
    }

    /// The keys (section, key) where `self`, as typed in the file, is out of range and `now` still
    /// holds what `sane` made of it, so the file's own text stays.
    fn out_of_range(&self, now: &Self) -> Vec<(&'static str, &'static str)> {
        let mut fixed = self.clone();
        fixed.sane();
        let keys = [
            ("window", "width", Some(self.window.width), Some(fixed.window.width), Some(now.window.width)),
            ("window", "height", Some(self.window.height), Some(fixed.window.height), Some(now.window.height)),
            ("editor", "font_size", self.editor.font_size, fixed.editor.font_size, now.editor.font_size),
            ("text", "weight", Some(self.text.weight), Some(fixed.text.weight), Some(now.text.weight)),
            ("text", "gamma", Some(self.text.gamma), Some(fixed.text.gamma), Some(now.text.gamma)),
            ("text", "contrast", Some(self.text.contrast), Some(fixed.text.contrast), Some(now.text.contrast)),
        ];
        keys.into_iter().filter(|(_, _, typed, fixed, now)| typed != fixed && fixed == now).map(|(section, key, ..)| (section, key)).collect()
    }
}

/// `value` kept between `least` and `most`; `usual` when it isn't a finite number.
fn within(value: f32, least: f32, most: f32, usual: f32) -> f32 {
    if value.is_finite() { value.clamp(least, most) } else { usual }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    pub window: WindowSettings,
    pub editor: EditorSettings,
    pub layout: LayoutSettings,
    pub explorer: ExplorerSettings,
    pub preview: PreviewSettings,
    pub text: TextSettings,
    pub scratch: ScratchSettings,
    pub code_colors: CodeColors,
}

/// The code color scheme View > Code Colors and `[code_colors] scheme` pick. Every scheme has a dark and a light version
/// from its own authors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scheme {
    #[default]
    Xcode,
    Ayu,
    Bluloco,
    Catppuccin,
    Dracula,
    Everforest,
    Flexoki,
    #[serde(rename = "github")]
    GitHub,
    Gruvbox,
    Horizon,
    #[serde(rename = "jetbrains")]
    JetBrains,
    Kanagawa,
    Material,
    Modus,
    MonokaiPro,
    NightOwl,
    Nightfox,
    One,
    Oxocarbon,
    RosePine,
    Solarized,
    Tomorrow,
    TokyoNight,
    Vitesse,
    #[serde(rename = "vscode")]
    VsCode,
}

impl Scheme {
    /// View > Code Colors' order: Xcode, then by name.
    pub const ALL: [Scheme; 25] = [
        Scheme::Xcode,
        Scheme::Ayu,
        Scheme::Bluloco,
        Scheme::Catppuccin,
        Scheme::Dracula,
        Scheme::Everforest,
        Scheme::Flexoki,
        Scheme::GitHub,
        Scheme::Gruvbox,
        Scheme::Horizon,
        Scheme::JetBrains,
        Scheme::Kanagawa,
        Scheme::Material,
        Scheme::Modus,
        Scheme::MonokaiPro,
        Scheme::NightOwl,
        Scheme::Nightfox,
        Scheme::One,
        Scheme::Oxocarbon,
        Scheme::RosePine,
        Scheme::Solarized,
        Scheme::Tomorrow,
        Scheme::TokyoNight,
        Scheme::Vitesse,
        Scheme::VsCode,
    ];

    /// The name View > Code Colors lists.
    pub fn title(self) -> &'static str {
        match self {
            Scheme::Xcode => "Xcode",
            Scheme::Ayu => "Ayu",
            Scheme::Bluloco => "Bluloco",
            Scheme::Catppuccin => "Catppuccin",
            Scheme::Dracula => "Dracula",
            Scheme::Everforest => "Everforest",
            Scheme::Flexoki => "Flexoki",
            Scheme::GitHub => "GitHub",
            Scheme::Gruvbox => "Gruvbox",
            Scheme::Horizon => "Horizon",
            Scheme::JetBrains => "JetBrains",
            Scheme::Kanagawa => "Kanagawa",
            Scheme::Material => "Material",
            Scheme::Modus => "Modus",
            Scheme::MonokaiPro => "Monokai Pro",
            Scheme::NightOwl => "Night Owl",
            Scheme::Nightfox => "Nightfox",
            Scheme::One => "One",
            Scheme::Oxocarbon => "Oxocarbon",
            Scheme::RosePine => "Rosé Pine",
            Scheme::Solarized => "Solarized",
            Scheme::Tomorrow => "Tomorrow",
            Scheme::TokyoNight => "Tokyo Night",
            Scheme::Vitesse => "Vitesse",
            Scheme::VsCode => "VS Code",
        }
    }
}

/// `[code_colors]`: how code is colored (View > Code Colors).
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct CodeColors {
    /// The scheme, by its id: `xcode`, `ayu`, `bluloco`, `catppuccin`, `dracula`, `everforest`, `flexoki`, `github`, `gruvbox`,
    /// `horizon`, `jetbrains`, `kanagawa`, `material`, `modus`, `monokai-pro`, `night-owl`, `nightfox`, `one`, `oxocarbon`,
    /// `rose-pine`, `solarized`, `tomorrow`, `tokyo-night`, `vitesse` or `vscode`. `xcode` when absent.
    pub scheme: Scheme,
    /// Brighter hues: saturation lifted, and every color kept readable against the ground. On when absent.
    pub vivid: bool,
    /// Fields, parameters, constants, enum variants, `self`, built-in types, methods, lifetimes, string escapes, Markdown list markers and quote marks each have their own hue. On when absent.
    pub colored_roles: bool,
    /// Brackets take one of a few hues by how deep they nest. On when absent.
    pub rainbow_brackets: bool,
    /// Names take a hue of their own from their spelling. Off when absent.
    pub semantic: bool,
    /// Hex overrides of single roles in the dark theme, by role name (`keyword = "#FC5FA3"`), applied last. The Settings window writes them (and only their own keys); a table with none isn't written.
    #[serde(deserialize_with = "overrides", skip_serializing_if = "BTreeMap::is_empty")]
    pub dark: BTreeMap<String, String>,
    /// The same for the light theme.
    #[serde(deserialize_with = "overrides", skip_serializing_if = "BTreeMap::is_empty")]
    pub light: BTreeMap<String, String>,
}

impl Default for CodeColors {
    fn default() -> Self {
        Self { scheme: Scheme::Xcode, vivid: true, colored_roles: true, rainbow_brackets: true, semantic: false, dark: BTreeMap::new(), light: BTreeMap::new() }
    }
}

impl CodeColors {
    /// Whether `other` colors the syntax the same way: rainbow brackets and semantic colors don't change the palette.
    pub fn same_palette(&self, other: &Self) -> bool {
        self.scheme == other.scheme && self.vivid == other.vivid && self.colored_roles == other.colored_roles && self.dark == other.dark && self.light == other.light
    }
}

/// `0xRRGGBB` of `#RRGGBB` (or `RRGGBB`), `None` for anything else.
pub fn parse_hex(text: &str) -> Option<u32> {
    let digits = text.trim().strip_prefix('#').unwrap_or(text.trim());
    (digits.len() == 6 && digits.bytes().all(|byte| byte.is_ascii_hexdigit())).then(|| u32::from_str_radix(digits, 16).ok()).flatten()
}

/// A hex color as the file writes it. A value that isn't one fails the file at that value, which the alert names by its key (`plain`).
#[derive(Deserialize)]
#[serde(try_from = "String")]
struct Hex(String);

impl TryFrom<String> for Hex {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        if parse_hex(&text).is_some() { Ok(Self(text)) } else { Err(format!("{HEX_MISTAKE}: {text}")) }
    }
}

/// What the parser says of a value that isn't a hex color, for `plain` to say in words.
const HEX_MISTAKE: &str = "not a hex color";

/// Reads one override table of role names and hex colors.
fn overrides<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<BTreeMap<String, String>, D::Error> {
    Ok(BTreeMap::<String, Hex>::deserialize(deserializer)?.into_iter().map(|(role, hex)| (role, hex.0)).collect())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct WindowSettings {
    pub width: f32,
    pub height: f32,
    /// The window's top-left corner in screen points, with `y`; both absent until a window has moved or resized.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y: Option<f32>,
    /// The UUID of the display the window was on; a launch restores the position only when that display is still connected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    /// Whether the window was windowed, maximized or full screen. `width`, `height`, `x` and `y` hold the size to come back to.
    pub state: WindowState,
    /// Whether the window is in Minimal mode (View > Minimal mode): only the text, no tab bar, tree, status line, preview or window buttons. Written once, on the toggle.
    pub minimal: bool,
    pub theme: ThemeSetting,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WindowState {
    #[default]
    Windowed,
    Maximized,
    Fullscreen,
}

/// A look for the editor's text (View > Text style): `mono` is `[editor] font_family`, `serif` the Mac's New York, `sans` its system font.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Typeface {
    #[default]
    Mono,
    Serif,
    Sans,
}

/// How each folder's rows are ordered in the tree (View > Sort by), folders first either way. Date
/// modified puts the newest first and Size the largest, as Finder does; ties go by name.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExplorerSort {
    #[default]
    Name,
    Kind,
    Date,
    Size,
}

impl ExplorerSort {
    pub const ALL: [Self; 4] = [Self::Name, Self::Kind, Self::Date, Self::Size];

    /// The name the Sort by menus list.
    pub fn title(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Kind => "Kind",
            Self::Date => "Date modified",
            Self::Size => "Size",
        }
    }
}

/// The colors the window starts in: `system` follows the Mac's light or dark mode live; `light` and `dark` pin it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ThemeSetting {
    #[default]
    System,
    Light,
    Dark,
}

/// How text is drawn. Read at launch.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct TextSettings {
    /// Weight for all text that doesn't set its own: 100 thin to 900 black.
    pub weight: f32,
    /// Glyph thickening: 0 off, 1 to 4 stronger (above 4 counts as 4).
    pub thicken: u8,
    /// Gamma of glyph coverage: 1.0 is off.
    pub gamma: f32,
    /// Contrast boost of glyph coverage: 0.0 is off.
    pub contrast: f32,
}

impl Default for TextSettings {
    fn default() -> Self {
        Self {
            weight: 500.,
            thicken: 4,
            gamma: 1.,
            contrast: 0.,
        }
    }
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            width: 900.,
            height: 700.,
            x: None,
            y: None,
            display: None,
            state: WindowState::Windowed,
            minimal: false,
            theme: ThemeSetting::System,
        }
    }
}

/// The scratch window (finch#wjr7).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ScratchSettings {
    /// The global shortcut that slides the scratch window in and out, like `ctrl-alt-space`
    /// (`hotkey::parse` lists the words); `off` or empty turns it off.
    pub shortcut: String,
    /// Whether the app keeps running with no window open, so the shortcut still works. Off: closing
    /// the last window quits.
    pub resident: bool,
}

impl Default for ScratchSettings {
    fn default() -> Self {
        Self { shortcut: "ctrl-alt-space".into(), resident: false }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct EditorSettings {
    /// `None` keeps GPUI Kit's editor font. Google Sans Code NF at first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    /// `None` keeps GPUI Kit's editor font size. 14 at first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f32>,
    pub soft_wrap: bool,
    /// Whether spaces and tabs show as marks in every editor (View > Show invisibles).
    pub show_invisibles: bool,
    /// Whether the line numbers show in the gutter of every editor (View > Show line numbers). On when absent.
    pub line_numbers: bool,
    /// Whether wrapped text stops at a readable width, centered, in prose and in very wide panes (View > Reading width). On when absent.
    pub reading_width: bool,
    /// Whether typing in Markdown and plain text makes quotes curly, `...` an ellipsis and `--` an en dash (View > Smart punctuation). On when absent.
    pub smart_punctuation: bool,
    /// Whether the line you type or move to by a key stays at one height a little above the middle, the page gliding under it (View > Typewriter scrolling). Off when absent.
    pub typewriter: bool,
    /// Whether Markdown files read like a document: semibold headings and faded marks (View > Styled Markdown). Off when absent.
    pub styled_markdown: bool,
    /// Whether the plain style check marks words from the style word list and long sentences in Markdown and plain text (View > Style Check). Off when absent.
    pub style_check: bool,
    /// How many words a sentence may have before the style check marks it. 30 when absent.
    pub style_sentence_words: usize,
    /// The parts of speech View > Highlight tints in Markdown and text files: any of "nouns", "verbs", "adjectives", "adverbs", "conjunctions". None when absent.
    pub highlight: Vec<String>,
    /// Whether misspelled words in Markdown, text, comments and strings get a red underline (View > Check spelling). On when absent.
    pub check_spelling: bool,
    /// Whether a save strips the spaces and tabs at the end of every line (EditorConfig's name).
    pub trim_trailing_whitespace: bool,
    /// Whether a save makes the text end with a newline (EditorConfig's name).
    pub insert_final_newline: bool,
    /// The text style of Markdown and plain text files (View > Text style): mono when absent.
    pub prose_style: Typeface,
    /// The text style of every other file (View > Text style): mono when absent.
    pub code_style: Typeface,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            font_family: Some("Google Sans Code NF".into()),
            font_size: Some(FONT_USUAL),
            soft_wrap: true,
            show_invisibles: false,
            line_numbers: true,
            reading_width: true,
            smart_punctuation: true,
            typewriter: false,
            trim_trailing_whitespace: false,
            styled_markdown: false,
            style_check: false,
            style_sentence_words: 30,
            highlight: Vec::new(),
            check_spelling: true,
            insert_final_newline: false,
            prose_style: Typeface::Mono,
            code_style: Typeface::Mono,
        }
    }
}

impl EditorSettings {
    /// The family to draw code in: the configured one when this Mac has it, else `None`, so the caller
    /// uses the theme's mono family (GPUI's own fallback for a missing family is a proportional font).
    /// Each family is looked up once and remembered; the settings file and the font picker keep the name as written.
    pub fn drawn_family(&self, cx: &App) -> Option<SharedString> {
        static CHECKED: Mutex<BTreeMap<String, bool>> = Mutex::new(BTreeMap::new());
        let family = self.font_family.as_deref()?;
        let mut checked = CHECKED.lock().unwrap_or_else(PoisonError::into_inner);
        let installed = match checked.get(family) {
            Some(installed) => *installed,
            None => {
                let installed = cx.text_system().all_font_names().iter().any(|name| name == family);
                checked.insert(family.to_owned(), installed);
                installed
            }
        };
        installed.then(|| SharedString::from(family.to_owned()))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct LayoutSettings {
    pub left_panel_width: f32,
    pub right_panel_width: f32,
}

impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            left_panel_width: 240.,
            right_panel_width: 420.,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ExplorerSettings {
    /// Whether the folder tree shows (Cmd-B).
    pub visible: bool,
    /// Whether dot files show in the tree. `.git` and `.DS_Store` stay out either way, gitignored paths unless `show_ignored`.
    pub show_hidden: bool,
    /// Whether the paths `.gitignore` and `.ignore` files leave out show in the tree, dimmed. `.git` and
    /// `.DS_Store` stay out either way, and dot files among them only show with `show_hidden`.
    pub show_ignored: bool,
    /// How a new window's tree orders each folder's rows: the last choice any window made.
    pub sort: ExplorerSort,
    /// The folder the tree was rooted at last time, for a launch with no path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_root: Option<PathBuf>,
    /// The files open in the window that last saved for `last_root`, in tab order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub open_files: Vec<PathBuf>,
    /// The showing tab's place in `open_files`, counting from 0.
    pub active_tab: usize,
    /// How many of the first `open_files` are pinned tabs, for a window with one pane (with
    /// several, each pane in `panes` says). Absent in a file from before pins.
    #[serde(skip_serializing_if = "is_zero")]
    pub pinned_tabs: usize,
    /// The single file open last time, from before tabs. Only read: the next write drops it.
    #[serde(skip_serializing)]
    pub last_file: Option<PathBuf>,
    /// Which pane (counting from 0, in reading order) had the keyboard, for `panes`.
    pub active_pane: usize,
    /// How the window's panes were split, each with its tabs. Absent in a file from before panes:
    /// then `open_files` is one pane. `open_files` still lists every pane's files in reading order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub panes: Option<SavedPanes>,
}

/// How a pane was split.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitAxis {
    /// Side by side: the new pane is on the right.
    Right,
    /// One above the other: the new pane is below.
    Down,
}

/// The pane layout as kept in the settings file: a tree of splits with a pane's tabs at each leaf.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SavedPanes {
    Pane {
        files: Vec<PathBuf>,
        /// The showing tab's place in `files`, counting from 0.
        #[serde(default)]
        active: usize,
        /// How many of the first `files` are pinned tabs. Absent in a file from before pins.
        #[serde(default, skip_serializing_if = "is_zero")]
        pinned: usize,
    },
    Split {
        axis: SplitAxis,
        /// The share of the split the first pane takes.
        ratio: f32,
        first: Box<SavedPanes>,
        second: Box<SavedPanes>,
    },
}

impl SavedPanes {
    /// Every file the layout names, in reading order.
    pub fn files(&self) -> Vec<PathBuf> {
        match self {
            Self::Pane { files, .. } => files.clone(),
            Self::Split { first, second, .. } => first.files().into_iter().chain(second.files()).collect(),
        }
    }
}

/// Keeps a pinned count of 0 out of the file, so a file without pins stays as it was.
fn is_zero(count: &usize) -> bool {
    *count == 0
}

impl ExplorerSettings {
    /// The files to bring back as tabs and the showing one's place: `open_files`, or the old
    /// `last_file` alone.
    pub fn saved_tabs(&self) -> (Vec<PathBuf>, usize) {
        if self.open_files.is_empty() {
            (self.last_file.iter().cloned().collect(), 0)
        } else {
            (self.open_files.clone(), self.active_tab)
        }
    }
}

impl Default for ExplorerSettings {
    fn default() -> Self {
        Self {
            visible: true,
            show_hidden: true,
            show_ignored: false,
            sort: ExplorerSort::Name,
            last_root: None,
            open_files: Vec::new(),
            active_tab: 0,
            pinned_tabs: 0,
            last_file: None,
            active_pane: 0,
            panes: None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PreviewSettings {
    /// Whether the markdown preview shows right of the editor (Cmd-Shift-V).
    pub visible: bool,
}

/// The settings in memory and where they are saved. A global, so any view can
/// read a setting or change one.
pub struct Store {
    settings: Settings,
    path: PathBuf,
    /// False in a background launch and after a file that didn't parse.
    writable: bool,
    /// Whether this launch may write at all: false in a background launch with no file of its own.
    allowed: bool,
    /// Counts changes, so a write knows which is newest.
    pending: u64,
    /// When the newest `update_later` change was made; the one debounce task waits until
    /// `WRITE_DELAY` has passed since.
    changed: Instant,
    /// The one task waiting to write the newest `update_later` changes; `None` when none waits.
    /// Dropping it cancels the wait.
    timer: Option<Task<()>>,
    /// A mistake in the file that hasn't been shown yet.
    problem: Option<Problem>,
    /// The text of the mistake last shown, so the same one isn't shown twice.
    alerted: Option<String>,
}

/// A problem with the file: `key` is the file's text (or the error), so one problem shows once;
/// `title` and `detail` are what the alert says, then `ending`; `show_file` adds a button that
/// brings the settings file forward.
struct Problem {
    key: String,
    title: &'static str,
    detail: String,
    ending: &'static str,
    show_file: bool,
}

const TITLE_MISTAKE: &str = "Your settings file has a mistake";
const TITLE_UNREADABLE: &str = "Your settings file can’t be read";
const TITLE_UNSAVED: &str = "Your settings couldn’t be saved";
const ENDING_KEEPS: &str = "Finch keeps the settings in use until it’s fixed.";
const BUTTON_SHOW_FILE: &str = "Show settings file";
const ENDING_DEFAULTS: &str = "Finch is using its defaults until it’s fixed.";
const ENDING_UNSAVED: &str = "Finch keeps your changes for now, but they aren’t in the file.";

impl Global for Store {}

/// "Line 4: font_size must be a number.", for the alert.
fn describe(text: &str, err: &toml::de::Error) -> Problem {
    let start = err.span().map(|span| span.start.min(text.len()));
    let message = plain(err.message(), start.and_then(|at| key_on_line(text, at))).unwrap_or_else(|| err.message().trim().trim_end_matches('.').to_owned());
    let detail = match start {
        Some(at) => format!("Line {}: {message}.", text[..at].matches('\n').count() + 1),
        None => format!("{message}."),
    };
    Problem { key: text.to_owned(), title: TITLE_MISTAKE, detail, ending: ENDING_KEEPS, show_file: true }
}

/// The key whose `=` comes straight before byte `at`, where a failing value starts.
fn key_on_line(text: &str, at: usize) -> Option<&str> {
    let begin = text[..at].rfind('\n').map_or(0, |newline| newline + 1);
    let before = text[begin..at].trim_end().strip_suffix('=')?;
    let key = before.rsplit(['{', ',']).next()?.trim().trim_start_matches('\u{feff}').trim_matches(['"', '\'']);
    (!key.is_empty() && !key.starts_with(['[', '#'])).then_some(key)
}

/// What the parser's invalid type, unknown field and missing field messages say, in plain words
/// naming the key; `None` for any other message, which is shown as it is.
fn plain(message: &str, key: Option<&str>) -> Option<String> {
    let message = message.trim();
    let names = |list: &str| list.split(',').map(|name| name.trim().trim_matches('`').to_owned()).collect::<Vec<_>>();
    if let Some(rest) = message.strip_prefix("unknown field `") {
        return Some(format!("{} isn’t a setting Finch knows", rest.split('`').next()?));
    }
    if let Some(rest) = message.strip_prefix("missing field `") {
        return Some(format!("{} is missing", rest.split('`').next()?));
    }
    if message.starts_with(HEX_MISTAKE) {
        return Some(format!("{} must be a color like #FC5FA3", key?));
    }
    if message.starts_with("unknown variant `") {
        let options = message.split_once("expected one of ")?.1;
        return Some(format!("{} must be one of {}", key?, names(options).join(", ")));
    }
    if message.starts_with("wanted string or table") {
        return Some(format!("{} must be text", key?));
    }
    if message.starts_with("invalid type: ") || message.starts_with("invalid value: ") {
        let wanted = match message.rsplit_once(", expected ")?.1.trim() {
            "f32" | "f64" => "a number",
            "u8" => "a whole number from 0 to 255",
            "usize" => "a whole number, 0 or more",
            "isize" => "a whole number",
            kind if kind.len() > 1 && kind.starts_with('u') && kind[1..].bytes().all(|byte| byte.is_ascii_digit()) => "a whole number, 0 or more",
            kind if kind.len() > 1 && kind.starts_with('i') && kind[1..].bytes().all(|byte| byte.is_ascii_digit()) => "a whole number",
            "a boolean" => "true or false",
            "a string" | "a borrowed string" | "path string" => "text",
            "a sequence" => "a list",
            kind if kind.starts_with("struct ") || kind == "a map" || kind == "string or map" => "a section",
            _ => return None,
        };
        return Some(format!("{} must be {wanted}", key?));
    }
    None
}

/// What `describe` makes of an error from reading the file at all.
fn describe_read(path: &Path, err: &std::io::Error) -> Problem {
    Problem { key: err.to_string(), title: TITLE_UNREADABLE, detail: crate::file_ops::explain("read", path, err), ending: ENDING_KEEPS, show_file: false }
}

/// What a settings write that failed says: the words for the save, and that the changes stay in memory.
fn describe_write(path: &Path, err: &std::io::Error) -> Problem {
    Problem { key: err.to_string(), title: TITLE_UNSAVED, detail: crate::file_ops::explain("save", path, err), ending: ENDING_UNSAVED, show_file: false }
}

/// Reads and parses the file at `path`, with the file's text.
fn read(path: &Path) -> Result<(Settings, String), Problem> {
    let text = fs::read_to_string(path).map_err(|err| describe_read(path, &err))?;
    let mut settings: Settings = toml::from_str(&text).map_err(|err| describe(&text, &err))?;
    settings.sane();
    Ok((settings, text))
}

/// Shared by the read-only lookup and the normal launch's carry-over decision.
static SUPPORT_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

fn paths() -> (PathBuf, PathBuf) {
    let support = std::env::home_dir().unwrap_or_default().join("Library/Application Support");
    (support.join("GPUI Editor"), support.join("Finch"))
}

/// Looking up a path never moves data, including in debug clients and background launches.
pub fn support_dir() -> &'static Path {
    SUPPORT_DIR.get_or_init(|| {
        let (old, new) = paths();
        if new.exists() || !old.exists() { new } else { old }
    })
}

/// Only a normal app launch may carry over the support folder, before reading any saved data.
pub fn carry_over_support() {
    if std::env::var_os("GPUI_EDITOR_BACKGROUND").is_some() {
        return;
    }
    let (old, new) = paths();
    let _ = SUPPORT_DIR.get_or_init(|| carry_over(&old, &new));
}

/// A single same-volume rename keeps every saved byte together. On failure, every caller
/// keeps using the old folder for this run rather than opening an empty settings or draft store.
fn carry_over(old: &Path, new: &Path) -> PathBuf {
    if new.exists() || !old.exists() {
        return new.to_owned();
    }
    // The old app may still be running: share its folder for this run. The move waits for a
    // normal launch after it quits, so its next settings write cannot recreate the old folder.
    // The socket file's presence is enough: a running app with a busy socket still owns the folder.
    if old.join("control.sock").exists() {
        return old.to_owned();
    }
    match fs::rename(old, new) {
        Ok(()) => new.to_owned(),
        Err(err) => {
            eprintln!("finch: can't move {} to {}: {err}; using the old folder for this run", old.display(), new.display());
            old.to_owned()
        }
    }
}

impl Store {
    /// Reads the file at `path`. `writable` is false for a background launch,
    /// which must never change the user's saved settings.
    pub fn load(path: PathBuf, writable: bool) -> Self {
        let (settings, parsed, problem) = match read(&path) {
            Ok((settings, _)) => (settings, true, None),
            Err(_) if matches!(fs::metadata(&path), Err(err) if err.kind() == std::io::ErrorKind::NotFound) => (Settings::default(), true, None),
            Err(mut problem) => {
                eprintln!("finch: Couldn’t read the settings in “{}”. {} Using the defaults instead.", path.display(), problem.detail);
                problem.ending = ENDING_DEFAULTS;
                (Settings::default(), false, Some(problem))
            }
        };
        Self {
            settings,
            path,
            writable: writable && parsed,
            allowed: writable,
            pending: 0,
            changed: Instant::now(),
            timer: None,
            problem,
            alerted: None,
        }
    }

    /// The settings as loaded, for work that happens before the app starts.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// `GPUI_EDITOR_SETTINGS` when set, else the file in Application Support.
    pub fn default_path() -> PathBuf {
        if let Some(path) = std::env::var_os("GPUI_EDITOR_SETTINGS") {
            return path.into();
        }
        support_dir().join("settings.toml")
    }

    /// Writes the settings now, on this thread, as change number `seq`.
    fn write(&self, seq: u64) -> Result<(), Problem> {
        if self.writable {
            write_snapshot(seq, &self.path, &self.settings)?;
        }
        Ok(())
    }
}

/// The newest change that reached the file. Writes take this lock, so they land one at a time and
/// in order: one for a change older than what the file already holds is dropped, since the newer
/// snapshot has everything the older one had.
static WRITTEN: Mutex<u64> = Mutex::new(0);

/// Holds the settings file's write turn while it lives. The editor's own save of settings.toml
/// takes it, so it and a settings write never rename over each other's text.
pub fn hold_writes() -> std::sync::MutexGuard<'static, u64> {
    WRITTEN.lock().unwrap_or_else(PoisonError::into_inner)
}

fn write_snapshot(seq: u64, path: &Path, settings: &Settings) -> Result<(), Problem> {
    let mut written = WRITTEN.lock().unwrap_or_else(PoisonError::into_inner);
    if seq <= *written {
        return Ok(());
    }
    match save(path, settings) {
        Ok(()) => {
            *written = seq;
            Ok(())
        }
        Err(err) => {
            eprintln!("finch: can't write {}: {err}", path.display());
            Err(describe_write(path, &err))
        }
    }
}

pub fn get(cx: &App) -> &Settings {
    &cx.global::<Store>().settings
}

/// Where the style check's word list lives (beside the settings file), and whether this launch may create it.
pub fn style_words_file(cx: &App) -> (PathBuf, bool) {
    let store = cx.global::<Store>();
    (store.path.parent().unwrap_or(Path::new("")).join("style-words.txt"), store.allowed)
}

/// Takes in how a write went: a failure waits to be shown (once per distinct error); a write that
/// landed lets the same error show again if it comes back.
fn settle(cx: &mut App, result: Result<(), Problem>) {
    let store = cx.global_mut::<Store>();
    match result {
        Ok(()) => {
            if store.writable {
                store.alerted = None;
            }
        }
        Err(problem) => {
            store.problem = Some(problem);
            // Callers can be inside a window update, where the prompt's own window update would fail.
            cx.defer(show_problem_soon);
        }
    }
}

/// Changes a setting and writes the file now.
pub fn update(cx: &mut App, change: impl FnOnce(&mut Settings)) {
    fold_geometry(cx);
    let store = cx.global_mut::<Store>();
    change(&mut store.settings);
    store.pending += 1;
    let result = store.write(store.pending);
    settle(cx, result);
}

/// Changes a setting in memory only and writes nothing, for a gesture's steps (a pinch); the
/// gesture's end writes once with `update_later`.
pub fn update_unsaved(cx: &mut App, change: impl FnOnce(&mut Settings)) {
    change(&mut cx.global_mut::<Store>().settings);
}

/// Changes a setting and writes the file once `WRITE_DELAY` passes with no
/// further change, for values that change many times a second (window size)
/// or on a keystroke's path (tab switches, panel toggles). One task waits for the quiet and
/// reads the settings then, so a change while it waits only records its time. The write runs on the
/// background executor, so the main thread never waits on the disk; a newer
/// snapshot always supersedes an older one (see `write_snapshot`).
pub fn update_later(cx: &mut App, change: impl FnOnce(&mut Settings)) {
    fold_geometry(cx);
    let store = cx.global_mut::<Store>();
    change(&mut store.settings);
    if !store.writable {
        return;
    }
    store.pending += 1;
    store.changed = Instant::now();
    if store.timer.is_some() {
        return;
    }
    let path = store.path.clone();
    let task = cx.spawn(async move |cx| {
        let (mine, snapshot) = loop {
            let step = cx.update(|cx| {
                let store = cx.global_mut::<Store>();
                let wait = (store.changed + WRITE_DELAY).saturating_duration_since(Instant::now());
                if !wait.is_zero() {
                    return Err(wait);
                }
                // This task goes on to the write; the next change starts a new one.
                if let Some(task) = store.timer.take() {
                    task.detach();
                }
                Ok((store.pending, store.settings.clone()))
            });
            match step {
                Ok(found) => break found,
                Err(wait) => cx.background_executor().timer(wait).await,
            }
        };
        let result = cx.background_executor().spawn(async move { write_snapshot(mine, &path, &snapshot) }).await;
        cx.update(|cx| settle(cx, result));
    });
    cx.global_mut::<Store>().timer = Some(task);
}

/// Where a window is and how big, as one bounds change reports it.
#[derive(Clone, Copy)]
pub struct Placed {
    pub width: f32,
    pub height: f32,
    pub x: f32,
    pub y: f32,
    pub state: WindowState,
}

/// The newest window geometry that hasn't been folded into the store yet. A drag or a live resize
/// reports it every frame, and the store is a global every settings observer (the Settings window,
/// marks, the code colours) wakes on, though none of them read geometry; so a frame only records
/// here, in a global nothing observes. The geometry reaches the store, and so the file, once the
/// resize has been quiet for `WRITE_DELAY`, or with the next change to any setting, whichever is first.
#[derive(Default)]
struct Moved {
    frame: Option<Frame>,
    changed: Option<Instant>,
    /// The one task waiting for the quiet; `None` when none waits.
    timer: Option<Task<()>>,
}

struct Frame {
    placed: Placed,
    display: Option<String>,
}

impl Frame {
    fn apply(&self, window: &mut WindowSettings) {
        window.width = self.placed.width;
        window.height = self.placed.height;
        window.x = Some(self.placed.x);
        window.y = Some(self.placed.y);
        window.display.clone_from(&self.display);
        window.state = self.placed.state;
    }
}

impl Global for Moved {}

/// The window section as it stands now: the saved one with the newest geometry laid over it.
pub fn window(cx: &App) -> WindowSettings {
    let mut window = get(cx).window.clone();
    if let Some(frame) = cx.try_global::<Moved>().and_then(|moved| moved.frame.as_ref()) {
        frame.apply(&mut window);
    }
    window
}

/// Moves the newest geometry into the store. The store's observers wake, so this runs only where
/// the store changes anyway.
fn fold_geometry(cx: &mut App) {
    if cx.try_global::<Moved>().is_none_or(|moved| moved.frame.is_none()) {
        return;
    }
    if let Some(frame) = cx.global_mut::<Moved>().frame.take() {
        frame.apply(&mut cx.global_mut::<Store>().settings.window);
    }
}

/// Records the window's geometry and writes it to the file once `WRITE_DELAY` passes with no
/// further change, as `update_later` does, but without waking the store's observers on each frame
/// (see `Moved`). The write runs on the background executor.
pub fn update_geometry(cx: &mut App, placed: Placed, display: &Option<String>) {
    let writable = cx.global::<Store>().writable;
    let moved = cx.default_global::<Moved>();
    match &mut moved.frame {
        Some(frame) => {
            frame.placed = placed;
            frame.display.clone_from(display);
        }
        None => moved.frame = Some(Frame { placed, display: display.clone() }),
    }
    moved.changed = Some(Instant::now());
    if !writable || moved.timer.is_some() {
        return;
    }
    let task = cx.spawn(async move |cx| {
        loop {
            let step = cx.update(|cx| {
                let moved = cx.global_mut::<Moved>();
                let wait = moved.changed.map_or(Duration::ZERO, |at| (at + WRITE_DELAY).saturating_duration_since(Instant::now()));
                if !wait.is_zero() {
                    return Err(wait);
                }
                // This task goes on to the write; the next change starts a new one.
                if let Some(task) = moved.timer.take() {
                    task.detach();
                }
                Ok(())
            });
            match step {
                Ok(()) => break,
                Err(wait) => cx.background_executor().timer(wait).await,
            }
        }
        let found = cx.update(|cx| {
            fold_geometry(cx);
            let store = cx.global_mut::<Store>();
            if !store.writable {
                return None;
            }
            store.pending += 1;
            Some((store.pending, store.path.clone(), store.settings.clone()))
        });
        if let Some((mine, path, snapshot)) = found {
            let result = cx.background_executor().spawn(async move { write_snapshot(mine, &path, &snapshot) }).await;
            cx.update(|cx| settle(cx, result));
        }
    });
    cx.global_mut::<Moved>().timer = Some(task);
}

/// The file's text as last written by us or taken in, so the watcher can tell our own writes (and
/// edits it already took in) from another program's.
static SEEN: Mutex<BTreeMap<PathBuf, String>> = Mutex::new(BTreeMap::new());

/// The key a settings file is known by in `SEEN`: where it really lives, so a symlink and its
/// target are one file.
fn seen_key(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| match (path.parent().and_then(|dir| fs::canonicalize(dir).ok()), path.file_name()) {
        (Some(dir), Some(name)) => dir.join(name),
        _ => path.to_path_buf(),
    })
}

fn seen_get(path: &Path) -> Option<String> {
    SEEN.lock().unwrap_or_else(PoisonError::into_inner).get(&seen_key(path)).cloned()
}

fn seen_set(path: &Path, text: String) -> Option<String> {
    SEEN.lock().unwrap_or_else(PoisonError::into_inner).insert(seen_key(path), text)
}

/// Takes the values in the file at the store's path in place of the ones in memory, and makes the
/// settings writable again. A file that doesn't parse is left alone, the values stay as they were,
/// and the mistake waits to be shown (`show_problem`).
fn adopt(store: &mut Store) -> bool {
    match read(&store.path) {
        Ok((settings, text)) => {
            store.settings = settings;
            store.writable = store.allowed;
            store.problem = None;
            store.alerted = None;
            seen_set(&store.path, text);
            true
        }
        Err(problem) => {
            eprintln!("finch: can't read {}: {}; keeping the settings in use", store.path.display(), problem.detail);
            // Nothing may write over the line the alert names, and no timer already waiting may either.
            store.writable = false;
            store.timer = None;
            store.problem = Some(problem);
            false
        }
    }
}

/// Shows the pending mistake in the file as an alert on a window, once per text. Returns false
/// while there is nothing to show it on yet.
fn show_problem(cx: &mut App) -> bool {
    let store = cx.global_mut::<Store>();
    let Some(problem) = store.problem.take() else { return true };
    if store.alerted.as_deref() == Some(problem.key.as_str()) {
        return true;
    }
    let Some(window) = cx.active_window().or_else(|| cx.windows().first().copied()) else {
        cx.global_mut::<Store>().problem = Some(problem);
        return false;
    };
    cx.global_mut::<Store>().alerted = Some(problem.key);
    let detail = format!("{} {}", problem.detail, problem.ending);
    let title = problem.title;
    let buttons: &[&str] = if problem.show_file { &["OK", BUTTON_SHOW_FILE] } else { &["OK"] };
    let _ = window.update(cx, |_, window, cx| {
        let answer = window.prompt(PromptLevel::Warning, title, Some(&detail), buttons, cx);
        if buttons.len() > 1 {
            let handle = window.window_handle();
            // The button runs Open Settings File, which opens or brings forward the settings file's tab.
            cx.spawn(async move |cx| {
                if answer.await.ok() == Some(1) {
                    cx.update(|cx| drop(handle.update(cx, |_, window, cx| window.dispatch_action(Box::new(crate::app::OpenSettingsFile), cx))));
                }
            })
            .detach();
        }
    });
    true
}

/// Shows the pending mistake as soon as a window exists (a launch's first one is still opening).
fn show_problem_soon(cx: &mut App) {
    if show_problem(cx) {
        return;
    }
    cx.spawn(async move |cx| {
        for _ in 0..100 {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            if cx.update(show_problem) {
                break;
            }
        }
    })
    .detach();
}

/// Whether two window sections place the window differently.
fn geometry_differs(a: &WindowSettings, b: &WindowSettings) -> bool {
    (a.width, a.height, a.x, a.y, a.state) != (b.width, b.height, b.x, b.y, b.state) || a.display != b.display
}

/// `adopt` for the store in the global: a disk edit of the window's place or size wins over the
/// geometry still waiting to fold in (`Moved`), which would otherwise write it back over the edit.
/// An edit that leaves the window section's geometry alone keeps the waiting one.
fn adopt_dropping_geometry(cx: &mut App) -> bool {
    let store = cx.global_mut::<Store>();
    let before = store.settings.window.clone();
    let adopted = adopt(store);
    if adopted && geometry_differs(&before, &store.settings.window) {
        if cx.has_global::<Moved>() {
            cx.global_mut::<Moved>().frame = None;
        }
    }
    adopted
}

/// A file was saved at `saved`: when it is the settings file, takes its
/// values in place of the ones in memory, so the next write doesn't put the
/// old ones back over an edit. A file that doesn't parse is left alone and the
/// values stay as they were, with an alert saying where the mistake is. Redraws every window.
pub fn reload(cx: &mut App, saved: &Path) {
    let store = cx.global_mut::<Store>();
    match (fs::canonicalize(saved), fs::canonicalize(&store.path)) {
        (Ok(saved), Ok(own)) if saved == own => {}
        _ => return,
    }
    if adopt_dropping_geometry(cx) {
        cx.refresh_windows();
    } else {
        cx.defer(show_problem_soon);
    }
}

/// Follows the settings file for edits made by other programs (another editor, chezmoi through a
/// symlink) and takes each in as it lands, so it shows at once and the next write keeps it.
/// Not started for a background launch that has no settings file of its own.
pub fn watch(cx: &mut App) {
    let store = cx.global::<Store>();
    if !store.allowed {
        return;
    }
    // A launch that couldn't read the file says so once its window is up.
    show_problem_soon(cx);
    let path = cx.global::<Store>().path.clone();
    let Some((watch, mut events)) = crate::settings_watch::start(&path) else { return };
    if let Ok(text) = fs::read_to_string(&path) {
        if seen_get(&path).is_none() {
            seen_set(&path, text);
        }
    }
    cx.spawn(async move |cx| {
        let _watch = watch;
        while events.recv().await.is_ok() {
            cx.update(changed_outside);
        }
    })
    .detach();
}

/// The watcher saw the file change: reloads unless the text is what we wrote or already took in.
/// A file that parses again after one that didn't makes the settings writable again; one that
/// doesn't parse shows an alert.
fn changed_outside(cx: &mut App) {
    let store = cx.global_mut::<Store>();
    let text = match fs::read_to_string(&store.path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // The file that didn't parse is gone: nothing is left to protect, so saving starts again
            // (the next write creates the file from the settings in memory).
            if store.allowed && !store.writable {
                store.writable = true;
                store.problem = None;
                store.alerted = None;
            }
            return;
        }
        Err(_) => return,
    };
    if seen_get(&store.path).as_deref() == Some(text.as_str()) {
        return;
    }
    if adopt_dropping_geometry(cx) {
        cx.refresh_windows();
        cx.defer(crate::menu::refresh);
    } else {
        show_problem_soon(cx);
    }
}

/// Writes any change not yet in the file, on this thread, after a write already under way on the
/// background executor lets go of the file. Runs as the app quits.
pub fn flush(cx: &App) {
    let store = cx.global::<Store>();
    // A failure is already on stderr; the app is quitting, so there is no window to tell.
    match cx.try_global::<Moved>().and_then(|moved| moved.frame.as_ref()) {
        // A resize that hasn't reached the store yet: written as a newer change than any made so far.
        Some(frame) if store.writable => {
            let mut settings = store.settings.clone();
            frame.apply(&mut settings.window);
            let _ = write_snapshot(store.pending + 1, &store.path, &settings);
        }
        _ => {
            let _ = store.write(store.pending);
        }
    }
}

/// What a new settings file says: every key with its usual value and range, all commented out, so
/// the file parses to the usual settings until a line is uncommented.
const TEMPLATE: &str = "# Settings. Take the # off a line to change it.\n\
\n\
[window]\n\
# width = 900  # in points, at least 480; the size a window opens at\n\
# height = 700  # in points, at least 200\n\
# x = 100  # no default: the window's left edge in points, used only with y and display\n\
# y = 100  # no default: the window's top edge in points, used only with x and display\n\
# display = \"\"  # no default: which display the window was on, used only with x and y\n\
# state = \"windowed\"  # \"windowed\", \"maximized\" or \"fullscreen\"\n\
# minimal = false  # true or false: Minimal mode, only the text on screen (⌃⌘↩)\n\
# theme = \"system\"  # \"system\" follows the Mac, or \"light\" or \"dark\"\n\
\n\
[editor]\n\
# font_family = \"Google Sans Code NF\"\n\
# font_size = 14  # 6 to 96, in points\n\
# soft_wrap = true  # true or false\n\
# show_invisibles = false  # true or false: marks for spaces and tabs\n\
# line_numbers = true  # true or false: the numbers in the gutter\n\
# reading_width = true  # true or false: wrapped text stops at a readable width and centers in wide panes\n\
# smart_punctuation = true  # true or false: curly quotes, ellipsis and en dash as you type in Markdown and text\n\
# typewriter = false  # true or false: the line you type stays at one height and the page glides under it\n\
# styled_markdown = false  # true or false: Markdown headings semibold, its marks faded\n\
# style_check = false  # true or false: faint marks on words from your style word list and on long sentences (View > Style Check)\n\
# style_sentence_words = 30  # a sentence longer than this many words gets a mark when the style check is on\n\
# highlight = []  # any of \"nouns\", \"verbs\", \"adjectives\", \"adverbs\", \"conjunctions\": the words View > Highlight tints in Markdown and text\n\
# check_spelling = true  # true or false: a red underline under misspelled words in prose, comments and strings\n\
# trim_trailing_whitespace = false  # true or false: a save removes spaces and tabs at the ends of lines\n\
# insert_final_newline = false  # true or false: a save ends the file with a newline\n\
# prose_style = \"mono\"  # \"mono\", \"serif\" or \"sans\": the text style of Markdown and plain text (View > Text style)\n\
# code_style = \"mono\"  # \"mono\", \"serif\" or \"sans\": the text style of every other file\n\
\n\
[text]\n\
# weight = 500  # 100 (thin) to 900 (black); applies at once\n\
# thicken = 4  # 0 (off) to 4; applies at once, here or in Settings > Text\n\
# gamma = 1.0  # 0.2 to 3.0, 1.0 is off; applies at once, here or in Settings > Text\n\
# contrast = 0.0  # 0.0 to 1.0, 0.0 is off; applies at once, here or in Settings > Text\n\
\n\
[layout]\n\
# left_panel_width = 240  # the folder tree's width in points; saved when you drag its divider\n\
# right_panel_width = 420  # the preview's width in points\n\
\n\
[explorer]\n\
# visible = true  # true or false: the folder tree (⌘B)\n\
# show_hidden = true  # true or false: dot files in the tree\n\
# show_ignored = false  # true or false: files .gitignore leaves out, dimmed, in the tree\n\
# sort = \"name\"  # name, kind, date or size: how each folder's rows are ordered\n\
# last_root = \"\"  # the folder open last time; saved by the app\n\
# open_files = []  # the tabs open last time, in order; saved by the app\n\
# active_tab = 0  # the showing tab's place in open_files, from 0; saved by the app\n\
# pinned_tabs = 0  # how many of the first open_files are pinned; saved by the app\n\
# active_pane = 0  # which pane had the keyboard, from 0; saved by the app\n\
# The app saves how panes were split as [explorer.panes] tables below.\n\
\n\
[preview]\n\
# visible = false  # true or false: the Markdown preview (⇧⌘V)\n\
\n\
[scratch]\n\
# shortcut = \"ctrl-alt-space\"  # opens the scratch window from any app; ctrl, alt, cmd, shift and one key joined by -, or \"off\"\n\
# resident = false  # true or false: keep running with no window open, so the shortcut still works\n\
\n\
[code_colors]\n\
# scheme = \"xcode\"  # xcode, ayu, bluloco, catppuccin, dracula, everforest, flexoki, github, gruvbox, horizon, jetbrains, kanagawa, material, modus, monokai-pro, night-owl, nightfox, one, oxocarbon, rose-pine, solarized, tomorrow, tokyo-night, vitesse or vscode (View > Code Colors)\n\
# vivid = true  # true or false: brighter hues, each kept readable on the background\n\
# colored_roles = true  # true or false: fields, parameters, constants, enum variants, self, built-in types, methods, lifetimes, string escapes and Markdown list and quote marks each get a hue\n\
# rainbow_brackets = true  # true or false: brackets take a hue by how deep they nest\n\
# semantic = false  # true or false: names take a hue of their own from their spelling\n\
# A role's color in one theme, as a hex value; any role name works (keyword, string, number, comment, type, function,\n\
# variable.parameter, type.builtin, function.method, string.escape, ...). Take the # off the [code_colors.dark] or [code_colors.light] line and the color line under it.\n\
# [code_colors.dark]\n\
# keyword = \"#FC5FA3\"\n\
# [code_colors.light]\n\
# keyword = \"#9B2393\"\n";

/// The text to put in the settings file before it opens for editing: the template when the file
/// is missing or holds nothing but blank space, else `None`. A file with any text is never touched.
fn template_for(existing: std::io::Result<String>) -> Option<&'static str> {
    match existing {
        Ok(text) if text.trim().is_empty() => Some(TEMPLATE),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(TEMPLATE),
        _ => None,
    }
}

/// Before the settings file opens as a tab: writes the template when the file is missing or
/// empty. Does nothing for a launch that may not write, or for a file with anything in it.
pub fn seed_template(cx: &App) {
    let store = cx.global::<Store>();
    if store.allowed {
        write_template(&store.path);
    }
}

fn write_template(path: &Path) {
    let path = &fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let Some(text) = template_for(fs::read_to_string(path)) else { return };
    if let Some(dir) = path.parent() {
        if let Err(err) = fs::create_dir_all(dir) {
            eprintln!("finch: can't write {}: {err}", path.display());
            return;
        }
    }
    // The watcher reads this as our own write, not another program's edit.
    seen_set(path, text.to_string());
    if let Err(err) = fs::write(path, text) {
        eprintln!("finch: can't write {}: {err}", path.display());
    }
}

/// Keys each section leaves out of the file when the app has no value for them. A save removes
/// these from the file when the app's value is gone; every other key it doesn't know stays.
fn optional_keys(section: &str) -> &'static [&'static str] {
    match section {
        "window" => &["x", "y", "display"],
        "editor" => &["font_family", "font_size"],
        "explorer" => &["last_root", "last_file", "open_files", "pinned_tabs", "panes"],
        _ => &[],
    }
}

/// Gives `item` and every table inside it the header position `at`, so tables taken from the
/// app's own text print where the merge put them (new sections last, pane tables right after
/// their section) instead of among the file's tables by their old place.
fn place(item: &mut toml_edit::Item, at: usize) {
    match item {
        toml_edit::Item::Table(table) => {
            table.set_position(at);
            for (_, child) in table.iter_mut() {
                place(child, at);
            }
        }
        toml_edit::Item::ArrayOfTables(tables) => {
            for table in tables.iter_mut() {
                table.set_position(at);
                for (_, child) in table.iter_mut() {
                    place(child, at);
                }
            }
        }
        _ => {}
    }
}

/// Brings the override table `file` to `own` (`None`: no overrides): a role no longer in `own` is removed, a changed
/// color is set in place and a new role added at the end. The table's header, comments and the colors that didn't
/// change keep the text they had.
fn merge_overrides(file: &mut dyn toml_edit::TableLike, own: Option<&toml_edit::Table>) {
    let gone: Vec<String> = file.iter().map(|(role, _)| role.to_owned()).filter(|role| !own.is_some_and(|own| own.contains_key(role))).collect();
    for role in gone {
        file.remove(&role);
    }
    let Some(own) = own else { return };
    for (role, item) in own.iter() {
        match file.get_mut(role) {
            Some(existing) => {
                if existing.as_str() != item.as_str() {
                    let mut value = item.clone();
                    if let (Some(value), Some(old)) = (value.as_value_mut(), existing.as_value()) {
                        *value.decor_mut() = old.decor().clone();
                    }
                    *existing = value;
                }
            }
            None => {
                file.insert(role, item.clone());
            }
        }
    }
}

/// Sets each value of `own` into `file` where it stands, keeping the file's comments, blank lines,
/// key order and keys the app doesn't know. New keys go at the end. Tables below the top level
/// (the saved panes) are replaced whole. `section` is the section's name, empty at the top.
fn merge_table(file: &mut toml_edit::Table, own: &toml_edit::Table, section: &str) {
    let top = section.is_empty();
    if !top {
        let keys = optional_keys(section);
        let gone: Vec<String> = file.iter().map(|(key, _)| key.to_owned()).filter(|key| keys.contains(&key.as_str()) && !own.contains_key(key)).collect();
        for key in gone {
            file.remove(&key);
        }
    }
    // A table written without its own header (`[code_colors.light]` alone) has no position: its children's mark where it sits.
    let at = if top {
        usize::MAX
    } else {
        let children = file.iter().filter_map(|(_, item)| item.as_table()?.position()).max();
        file.position().or(children).unwrap_or(0)
    };
    let mut panes_added = None;
    for (key, item) in own.iter() {
        let mut item = item.clone();
        match (file.get_mut(key), &item) {
            // `[code_colors.dark]` and `[code_colors.light]` are the user's own tables: only their keys change.
            (Some(existing), toml_edit::Item::Table(inner)) if section == "code_colors" && matches!(key, "dark" | "light") && existing.is_table_like() => {
                if let Some(table) = existing.as_table_like_mut() {
                    merge_overrides(table, Some(inner));
                }
            }
            (Some(existing), toml_edit::Item::Table(inner)) if top => match existing.as_table_mut() {
                Some(table) => {
                    let had_panes = table.contains_key("panes");
                    merge_table(table, inner, key);
                    if key == "explorer" && !had_panes && table.contains_key("panes") {
                        panes_added = Some(table.position().unwrap_or(0));
                    }
                }
                None => {
                    place(&mut item, at);
                    *existing = item;
                }
            },
            (Some(existing), toml_edit::Item::Value(value)) => {
                let mut value = value.clone();
                if let Some(old) = existing.as_value() {
                    *value.decor_mut() = old.decor().clone();
                }
                *existing = toml_edit::Item::Value(value);
            }
            (Some(existing), _) => {
                let old = existing.as_table().map(|table| table.decor().clone());
                place(&mut item, at);
                *existing = item;
                if let (Some(old), Some(table)) = (old, existing.as_table_mut()) {
                    *table.decor_mut() = old;
                }
            }
            (None, _) => {
                place(&mut item, at);
                file.insert(key, item);
            }
        }
    }
    if section == "code_colors" {
        // An override table the app no longer has (the last override removed) loses its keys and, when nothing
        // else is in it, its header; a header with a comment above it stays.
        for name in ["dark", "light"] {
            if own.contains_key(name) {
                continue;
            }
            if let Some(table) = file.get_mut(name).and_then(|item| item.as_table_like_mut()) {
                merge_overrides(table, None);
            }
            let bare = file.get(name).and_then(|item| item.as_table()).is_some_and(|table| table.is_empty() && !table.decor().prefix().and_then(|prefix| prefix.as_str()).is_some_and(|prefix| prefix.contains('#')));
            if bare {
                file.remove(name);
            }
        }
    }
    if let Some(at) = panes_added {
        // The comments under `[explorer]` are the next header's prefix, and the new pane tables
        // sit right after `[explorer]`, so the prefix moves onto the first of them.
        let next = file.iter().filter_map(|(key, item)| Some((item.as_table()?.position().filter(|&position| position > at)?, key.to_owned()))).min();
        if let Some((_, next)) = next {
            if let Some(table) = file.get_mut(&next).and_then(|item| item.as_table_mut()) {
                let prefix = table.decor().prefix().cloned();
                table.decor_mut().set_prefix("\n");
                if let (Some(prefix), Some(panes)) = (prefix, file.get_mut("explorer").and_then(|item| item.as_table_mut()).and_then(|table| table.get_mut("panes")).and_then(|item| item.as_table_mut())) {
                    panes.decor_mut().set_prefix(prefix);
                }
            }
        }
    }
}

/// Writes every whole-number float in `table` as an integer (`width = 1440`, not `1440.0`), as the
/// template does; a real fraction stays. Reading takes both forms.
fn whole_floats_as_integers(table: &mut toml_edit::Table) {
    for (_, item) in table.iter_mut() {
        match item {
            toml_edit::Item::Table(inner) => whole_floats_as_integers(inner),
            toml_edit::Item::Value(value) => {
                if let Some(float) = value.as_float().filter(|float| float.is_finite() && float.fract() == 0. && float.abs() < 1e15) {
                    let decor = value.decor().clone();
                    *value = toml_edit::Value::from(float as i64);
                    *value.decor_mut() = decor;
                }
            }
            _ => {}
        }
    }
}

/// Writes `settings` over the sections this file knows, keeping every other
/// section of the file. Writes a temp file in the same folder, then renames it.
fn save(path: &Path, settings: &Settings) -> std::io::Result<()> {
    // A symlinked file (chezmoi) is written where it points, so the link survives the rename.
    // A link whose target is gone is left alone: renaming onto it would replace the link with a plain file.
    let Ok(path) = fs::canonicalize(path) else {
        if fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(std::io::Error::other("the settings file is a link to a file that is missing"));
        }
        return save_to(path, settings);
    };
    save_to(&path, settings)
}

fn save_to(path: &Path, settings: &Settings) -> std::io::Result<()> {
    let path = &path.to_path_buf();
    let current = match fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err),
    };
    // Text in the file that Finch neither wrote nor took in is another program's edit the watcher
    // hasn't adopted yet. Writing now would undo it, so this write waits: the watcher adopts the
    // edit, and the next write builds on it. The watcher batches at 100 ms, so an edit older than
    // `ADOPT_WAIT` means it isn't coming (no watcher, a missed event), and the write goes ahead.
    if let (Some(current), Some(seen)) = (&current, seen_get(path)) {
        let fresh = fs::metadata(path).and_then(|meta| meta.modified()).is_ok_and(|at| at.elapsed().is_ok_and(|age| age < ADOPT_WAIT));
        if *current != seen && fresh {
            return Ok(());
        }
    }
    let mut doc = match current {
        Some(text) => text.parse::<toml_edit::DocumentMut>().map_err(|err| std::io::Error::other(format!("the file no longer parses: {err}")))?,
        None => toml_edit::DocumentMut::new(),
    };
    let mut own = toml::to_string_pretty(settings).map_err(std::io::Error::other)?.parse::<toml_edit::DocumentMut>().map_err(std::io::Error::other)?;
    whole_floats_as_integers(own.as_table_mut());
    // A number the user typed out of range stays in the file as typed: the app holds the clamped
    // value in memory, and only a value it changed itself is written.
    if let Ok(typed) = toml::from_str::<Settings>(&doc.to_string()) {
        for (section, key) in typed.out_of_range(settings) {
            let typed_item = doc.get(section).and_then(|table| table.get(key)).cloned();
            if let (Some(item), Some(table)) = (typed_item, own.get_mut(section).and_then(|table| table.as_table_mut())) {
                table.insert(key, item);
            }
        }
    }
    merge_table(doc.as_table_mut(), own.as_table(), "");
    let text = doc.to_string();
    // The watcher reads this as our own write, not another program's edit.
    let before = seen_set(path, text.clone());

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).inspect_err(|_| restore_seen(path, &before))?;
    }
    // A name of our own, so two Finch processes never write the same temp file.
    static TEMP_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".{}-{}.tmp", std::process::id(), TEMP_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let temp = PathBuf::from(temp);
    let landed = (|| {
        use std::io::Write;
        let mut file = fs::File::create(&temp)?;
        file.write_all(text.as_bytes())?;
        // The bytes reach the disk before the rename, so a power cut leaves the old or the new file, never an empty one.
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)?;
        if let Some(dir) = path.parent() {
            if let Ok(dir) = fs::File::open(dir) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    })();
    landed.inspect_err(|_| {
        let _ = fs::remove_file(&temp);
        restore_seen(path, &before);
    })
}

/// A write that failed leaves the file as it was, so the text last seen is the one from before.
fn restore_seen(path: &Path, before: &Option<String>) {
    let mut seen = SEEN.lock().unwrap_or_else(PoisonError::into_inner);
    match before {
        Some(text) => seen.insert(seen_key(path), text.clone()),
        None => seen.remove(&seen_key(path)),
    };
}

#[cfg(test)]
mod tests {
    use super::{adopt, describe, save, template_for, write_template, SavedPanes, Settings, Store, Typeface, TEMPLATE};

    #[test]
    fn carry_over_moves_every_byte_and_keeps_old_data_on_failure() {
        let scratch = std::env::temp_dir().join(format!("finch-v63x-carry-over-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).unwrap();
        let old = scratch.join("GPUI Editor");
        let new = scratch.join("Finch");
        let settings = b"# keep this comment\n[editor]\nfont_size = 17\n";
        let draft = b"Unsaved text\n\0and every byte\n";
        std::fs::create_dir_all(old.join("drafts")).unwrap();
        std::fs::write(old.join("settings.toml"), settings).unwrap();
        std::fs::write(old.join("drafts/Untitled.txt"), draft).unwrap();
        // An old app's socket file keeps its folder, whether or not it answers right now.
        let listener = std::os::unix::net::UnixListener::bind(old.join("control.sock")).unwrap();
        assert_eq!(super::carry_over(&old, &new), old);
        assert!(!new.exists());
        assert_eq!(std::fs::read(old.join("settings.toml")).unwrap(), settings);
        assert_eq!(std::fs::read(old.join("drafts/Untitled.txt")).unwrap(), draft);
        drop(listener);
        assert_eq!(super::carry_over(&old, &new), old);
        // Once the old app quits it removes its socket, and the folder can move.
        std::fs::remove_file(old.join("control.sock")).unwrap();
        // Old only: the whole folder moves, with settings and drafts unchanged.
        assert_eq!(super::carry_over(&old, &new), new);
        assert!(!old.exists());
        assert_eq!(std::fs::read(new.join("settings.toml")).unwrap(), settings);
        assert_eq!(std::fs::read(new.join("drafts/Untitled.txt")).unwrap(), draft);
        // New only: no old folder is created and no saved byte changes.
        assert_eq!(super::carry_over(&old, &new), new);
        assert!(!old.exists());
        assert_eq!(std::fs::read(new.join("drafts/Untitled.txt")).unwrap(), draft);
        // Both: use Finch without merging or touching the old folder.
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join("settings.toml"), b"old settings").unwrap();
        std::fs::write(old.join("Untitled.txt"), draft).unwrap();
        assert_eq!(super::carry_over(&old, &new), new);
        assert_eq!(std::fs::read(old.join("settings.toml")).unwrap(), b"old settings");
        assert_eq!(std::fs::read(new.join("settings.toml")).unwrap(), settings);
        // A missing destination parent forces a rename failure, even when run as root.
        let failed = scratch.join("missing/Finch");
        assert_eq!(super::carry_over(&old, &failed), old);
        assert!(!failed.exists());
        assert_eq!(std::fs::read(old.join("settings.toml")).unwrap(), b"old settings");
        assert_eq!(std::fs::read(old.join("Untitled.txt")).unwrap(), draft);
        std::fs::remove_dir_all(&scratch).unwrap();
    }

    #[test]
    fn a_mistake_names_its_line_in_plain_words() {
        let text = "[window]\nwidth = 600\ntheme = \"sepia\"\n";
        let err = toml::from_str::<Settings>(text).unwrap_err();
        let problem = describe(text, &err);
        assert!(problem.detail.starts_with("Line 3: "), "{}", problem.detail);
        assert!(problem.detail.ends_with('.') && !problem.detail.contains("::"), "{}", problem.detail);
        let stray = "[window]\nwidth = \"600\n";
        let problem = describe(stray, &toml::from_str::<Settings>(stray).unwrap_err());
        assert!(problem.detail.starts_with("Line "), "{}", problem.detail);
    }

    #[test]
    fn a_wrong_typed_value_says_what_its_key_wants() {
        let cases = [
            ("[window]\nwidth = \"wide\"\n", "Line 2: width must be a number."),
            ("[window]\nx = true\n", "Line 2: x must be a number."),
            ("[window]\ntheme = \"sepia\"\n", "Line 2: theme must be one of system, light, dark."),
            ("[window]\nstate = 3\n", "Line 2: state must be text."),
            ("[editor]\nfont_size = \"big\"\n", "Line 2: font_size must be a number."),
            ("[editor]\nprose_style = \"fancy\"\n", "Line 2: prose_style must be one of mono, serif, sans."),
            ("[editor]\nsoft_wrap = 1\n", "Line 2: soft_wrap must be true or false."),
            ("[editor]\nfont_family = 12\n", "Line 2: font_family must be text."),
            ("[layout]\nleft_panel_width = true\n", "Line 2: left_panel_width must be a number."),
            ("[explorer]\nvisible = \"yes\"\n", "Line 2: visible must be true or false."),
            ("[explorer]\nactive_tab = \"1\"\n", "Line 2: active_tab must be a whole number, 0 or more."),
            ("[explorer]\nopen_files = \"a.txt\"\n", "Line 2: open_files must be a list."),
            ("[explorer]\nlast_root = 5\n", "Line 2: last_root must be text."),
            ("[preview]\nvisible = 0\n", "Line 2: visible must be true or false."),
            ("[text]\nthicken = \"more\"\n", "Line 2: thicken must be a whole number from 0 to 255."),
            ("[text]\nthicken = 300\n", "Line 2: thicken must be a whole number from 0 to 255."),
            ("editor = { font_size = \"big\" }\n", "Line 1: font_size must be a number."),
            ("[text]\nweight = \"bold\"\n", "Line 2: weight must be a number."),
            ("[text]\ngamma = false\n", "Line 2: gamma must be a number."),
            ("[text]\ncontrast = \"high\"\n", "Line 2: contrast must be a number."),
            ("window = 3\n", "Line 1: window must be a section."),
        ];
        let mut wrong = Vec::new();
        for (text, expected) in cases {
            let problem = describe(text, &toml::from_str::<Settings>(text).unwrap_err());
            if problem.detail != expected {
                wrong.push(format!("{text:?} said {:?}, not {expected:?}", problem.detail));
            }
            for raw in ["f32", "u8", "usize", "::", "`"] {
                assert!(!problem.detail.contains(raw), "{raw} in {}", problem.detail);
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn a_missing_field_and_an_unknown_kind_of_message_still_read_plainly() {
        let text = "[explorer.panes]\nkind = \"pane\"\n";
        let problem = describe(text, &toml::from_str::<Settings>(text).unwrap_err());
        assert_eq!(problem.detail, "Line 1: files is missing.");
        assert_eq!(super::plain("something else entirely", Some("key")), None);
        let unknown = super::plain("unknown field `font_sise`, expected one of `font_family`, `font_size`", None).unwrap();
        assert_eq!(unknown, "font_sise isn’t a setting Finch knows");
        let list = "[explorer]\nopen_files = [\"a.txt\", 3]\n";
        let problem = describe(list, &toml::from_str::<Settings>(list).unwrap_err());
        assert!(problem.detail.starts_with("Line 2: invalid type"), "{}", problem.detail);
    }

    #[test]
    fn the_template_writes_shortcuts_as_key_glyphs() {
        assert!(TEMPLATE.contains("(⌘B)") && TEMPLATE.contains("(⇧⌘V)"));
        assert!(!TEMPLATE.contains("Cmd") && !TEMPLATE.contains("Shift"), "{TEMPLATE}");
    }

    #[test]
    fn a_missing_code_colors_table_gives_the_defaults() {
        let usual = super::CodeColors::default();
        assert_eq!(toml::from_str::<Settings>("").unwrap().code_colors, usual);
        assert_eq!(toml::from_str::<Settings>("[code_colors]\n").unwrap().code_colors, usual);
        assert_eq!(usual.scheme, super::Scheme::Xcode);
        assert!(usual.vivid && usual.colored_roles && usual.rainbow_brackets && !usual.semantic);
        assert!(usual.dark.is_empty() && usual.light.is_empty());
        let some = toml::from_str::<Settings>("[code_colors]\nscheme = \"tokyo-night\"\nsemantic = true\n").unwrap().code_colors;
        assert_eq!(some.scheme, super::Scheme::TokyoNight);
        assert!(some.semantic && some.vivid, "a key left out keeps its default");
        assert!(TEMPLATE.contains("# scheme = \"xcode\""));
        assert_eq!(toml::from_str::<Settings>(TEMPLATE).unwrap().code_colors, usual, "the template changes nothing until a line is uncommented");
        // Every scheme's id reads back as that scheme, and the template lists it, so a hand edit can find each one.
        for scheme in super::Scheme::ALL {
            let id = toml::Value::try_from(scheme).unwrap().as_str().unwrap().to_owned();
            let read = toml::from_str::<Settings>(&format!("[code_colors]\nscheme = \"{id}\"\n")).unwrap().code_colors.scheme;
            assert_eq!(read, scheme, "{id}");
            assert!(TEMPLATE.lines().any(|line| line.starts_with("# scheme =") && line.contains(&format!(" {id}"))), "{id} missing from the template");
        }
    }

    #[test]
    fn code_color_overrides_parse_and_a_bad_one_names_its_key() {
        let text = "[code_colors.dark]\nkeyword = \"#FC5FA3\"\n\"variable.parameter\" = \"00ff7f\"\n[code_colors.light]\nstring = \"#c41a16\"\n";
        let colors = toml::from_str::<Settings>(text).unwrap().code_colors;
        assert_eq!(colors.dark.get("keyword").and_then(|hex| super::parse_hex(hex)), Some(0xFC5FA3));
        assert_eq!(colors.dark.get("variable.parameter").and_then(|hex| super::parse_hex(hex)), Some(0x00FF7F));
        assert_eq!(colors.light.get("string").and_then(|hex| super::parse_hex(hex)), Some(0xC41A16));
        assert_eq!(colors.scheme, super::Scheme::Xcode, "the tables alone leave the other keys at their defaults");
        let bad = "[code_colors.dark]\nkeyword = \"pink\"\n";
        let problem = describe(bad, &toml::from_str::<Settings>(bad).unwrap_err());
        assert_eq!(problem.detail, "Line 2: keyword must be a color like #FC5FA3.");
        assert_eq!(super::parse_hex("#12345"), None);
        assert_eq!(super::parse_hex("#12345g"), None);
        let unknown = "[code_colors]\nscheme = \"nord\"\n";
        let problem = describe(unknown, &toml::from_str::<Settings>(unknown).unwrap_err());
        assert!(problem.detail.contains("scheme must be one of"), "{}", problem.detail);
    }

    #[test]
    fn reading_width_is_on_unless_the_file_says_off() {
        assert!(Settings::default().editor.reading_width);
        assert!(toml::from_str::<Settings>("").unwrap().editor.reading_width);
        assert!(!toml::from_str::<Settings>("[editor]\nreading_width = false\n").unwrap().editor.reading_width);
        assert!(TEMPLATE.contains("# reading_width = true"));
    }

    #[test]
    fn minimal_mode_is_off_unless_the_file_says_on() {
        assert!(!Settings::default().window.minimal);
        assert!(!toml::from_str::<Settings>("[window]\nwidth = 600\n").unwrap().window.minimal);
        assert!(toml::from_str::<Settings>("[window]\nminimal = true\n").unwrap().window.minimal);
        assert!(TEMPLATE.contains("# minimal = false"));
    }

    #[test]
    fn line_numbers_are_on_unless_the_file_says_off() {
        assert!(Settings::default().editor.line_numbers);
        assert!(toml::from_str::<Settings>("[editor]\nsoft_wrap = false\n").unwrap().editor.line_numbers);
        assert!(toml::from_str::<Settings>("").unwrap().editor.line_numbers);
        assert!(!toml::from_str::<Settings>("[editor]\nline_numbers = false\n").unwrap().editor.line_numbers);
        assert!(TEMPLATE.contains("# line_numbers = true"));
    }

    #[test]
    fn typewriter_is_off_unless_the_file_says_on() {
        assert!(!Settings::default().editor.typewriter);
        assert!(!toml::from_str::<Settings>("").unwrap().editor.typewriter);
        assert!(toml::from_str::<Settings>("[editor]\ntypewriter = true\n").unwrap().editor.typewriter);
        assert!(TEMPLATE.contains("# typewriter = false"));
    }

    #[test]
    fn a_file_that_cant_be_read_or_saved_says_what_happened_in_plain_words() {
        let path = std::path::Path::new("/Users/someone/Library/Application Support/Finch/settings.toml");
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let read = super::describe_read(path, &denied);
        assert_eq!(read.title, "Your settings file can’t be read");
        assert_eq!(read.detail, "You don’t have permission to read “settings.toml”. Check its permissions in Finder (Get Info).");
        assert_eq!(read.key, denied.to_string(), "one problem shows once, keyed by the raw error");
        let mistake = describe("[window\n", &toml::from_str::<Settings>("[window\n").unwrap_err());
        assert_eq!(mistake.title, "Your settings file has a mistake");
        let full = std::io::Error::from_raw_os_error(28);
        let write = super::describe_write(path, &full);
        assert_eq!(write.title, "Your settings couldn’t be saved");
        assert_eq!(write.detail, "The disk is full. Free up some space, then try to save “settings.toml” again.");
        assert!(write.ending.contains("aren’t in the file"));
    }

    #[test]
    fn a_failed_save_comes_back_as_a_problem_and_the_next_one_lands() {
        let dir = std::env::temp_dir().join(format!("finch-xg3q-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.toml");
        let settings = Settings::default();
        let mut folder = std::fs::metadata(&dir).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut folder, 0o555);
        std::fs::set_permissions(&dir, folder.clone()).unwrap();
        let result = save(&path, &settings);
        std::os::unix::fs::PermissionsExt::set_mode(&mut folder, 0o755);
        std::fs::set_permissions(&dir, folder).unwrap();
        let problem = super::describe_write(&path, &result.expect_err("a folder nobody can write to fails the save"));
        assert_eq!(problem.title, "Your settings couldn’t be saved");
        assert!(problem.detail.contains("permission to save “settings.toml”"), "{}", problem.detail);
        save(&path, &settings).unwrap();
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_save_leaves_a_link_to_a_missing_file_alone_and_no_temp_file_behind() {
        let dir = std::env::temp_dir().join(format!("finch-a57e-dangling-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.toml");
        std::os::unix::fs::symlink(dir.join("gone.toml"), &path).unwrap();
        assert!(save(&path, &Settings::default()).is_err());
        assert!(std::fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
        std::fs::write(dir.join("gone.toml"), "").unwrap();
        save(&path, &Settings::default()).unwrap();
        assert!(std::fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "only the link and its target remain");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_fixed_after_a_mistake_makes_the_settings_writable_and_clears_the_alert() {
        let dir = std::env::temp_dir().join(format!("finch-adopt-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.toml");
        std::fs::write(&path, "[window]\ntheme = \"sepia\"\n").unwrap();
        let mut store = Store::load(path.clone(), true);
        assert!(!store.writable && store.allowed);
        assert!(store.problem.is_some());
        assert!(!adopt(&mut store), "a file still wrong stays unread");
        assert!(!store.writable && store.problem.is_some());
        std::fs::write(&path, "[window]\ntheme = \"dark\"\n").unwrap();
        assert!(adopt(&mut store));
        assert!(store.writable && store.problem.is_none());
        // A good load whose file breaks later stops writing too, and says the settings in use stay.
        let mut store = Store::load(path.clone(), true);
        assert!(store.writable);
        std::fs::write(&path, "[window]\ntheme = \"sepia\"\n").unwrap();
        assert!(!adopt(&mut store));
        assert!(!store.writable && store.timer.is_none());
        assert!(store.problem.as_ref().is_some_and(|problem| problem.ending == super::ENDING_KEEPS));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn waiting_geometry_lays_over_the_window_section_and_a_disk_edit_of_it_is_told_apart() {
        use super::{Frame, Placed, ThemeSetting, WindowSettings, WindowState, geometry_differs};
        let mut window = WindowSettings::default();
        let saved = window.clone();
        let frame = Frame { placed: Placed { width: 700., height: 500., x: 10., y: 20., state: WindowState::Maximized }, display: Some("d".into()) };
        frame.apply(&mut window);
        assert_eq!((window.width, window.height, window.x, window.y, window.state, window.display.as_deref()), (700., 500., Some(10.), Some(20.), WindowState::Maximized, Some("d")));
        assert!(geometry_differs(&saved, &window));
        let mut theme_only = saved.clone();
        theme_only.theme = ThemeSetting::Dark;
        assert!(!geometry_differs(&saved, &theme_only), "an edit outside the geometry keeps the waiting one");
    }

    #[test]
    fn bigger_and_smaller_step_whole_points_within_the_range() {
        use super::font_step;
        assert_eq!((font_step(14., true), font_step(14., false)), (15., 13.));
        assert_eq!((font_step(23., true), font_step(24., true), font_step(24., false), font_step(26., false)), (24., 26., 23., 24.));
        assert_eq!((font_step(13.5, true), font_step(13.5, false)), (15., 13.));
        assert_eq!((font_step(96., true), font_step(95., true), font_step(6., false)), (96., 96., 6.));
        // Up and down from the usual size come back to it for the whole range a person steps through.
        for start in 6..=23 {
            assert_eq!(font_step(font_step(start as f32, true), false), start as f32);
        }
    }

    #[test]
    fn numbers_a_renderer_can_t_use_come_into_range() {
        let sane = |text: &str| {
            let mut settings: Settings = toml::from_str(text).unwrap();
            settings.sane();
            settings
        };
        let low = sane("[editor]\nfont_size = 0\n[text]\nweight = 0\ngamma = 0\ncontrast = -1\n[window]\nwidth = 10\nheight = 10\n");
        assert_eq!((low.editor.font_size, low.text.weight, low.text.gamma, low.text.contrast), (Some(6.), 100., 0.2, 0.));
        assert_eq!((low.window.width, low.window.height), (480., 200.));
        let high = sane("[editor]\nfont_size = 500\n[text]\nweight = 5000\ngamma = 9\ncontrast = 4\n");
        assert_eq!((high.editor.font_size, high.text.weight, high.text.gamma, high.text.contrast), (Some(96.), 900., 3., 1.));
        let nan = sane("[editor]\nfont_size = nan\n[text]\nweight = inf\ngamma = nan\n[window]\nwidth = nan\n");
        let usual = Settings::default();
        assert_eq!((nan.editor.font_size, nan.text.weight, nan.text.gamma, nan.window.width), (usual.editor.font_size, usual.text.weight, usual.text.gamma, usual.window.width));
        let fine = sane("[editor]\nfont_size = 6\n[text]\nweight = 900\ngamma = 3.0\ncontrast = 1.0\n[window]\nwidth = 480\nheight = 200\n");
        assert_eq!((fine.editor.font_size, fine.text.weight, fine.text.gamma, fine.text.contrast, fine.window.width, fine.window.height), (Some(6.), 900., 3., 1., 480., 200.));
    }

    #[test]
    fn a_save_leaves_an_out_of_range_number_as_typed_until_the_app_changes_it() {
        let dir = std::env::temp_dir().join(format!("finch-range-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.toml");
        let old = "[editor]\nfont_size = 0\n\n[text]\nweight = 0 # thin\n\n[window]\nwidth = 10\n";
        std::fs::write(&path, old).unwrap();
        let mut settings: Settings = toml::from_str(old).unwrap();
        settings.sane();
        assert_eq!(settings.editor.font_size, Some(6.));
        settings.explorer.active_tab = 1;
        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("font_size = 0\n") && text.contains("weight = 0 # thin\n") && text.contains("width = 10\n"), "{text}");
        assert!(text.contains("active_tab = 1"), "{text}");
        settings.editor.font_size = Some(20.);
        settings.window.width = 700.;
        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("font_size = 20") && text.contains("width = 700") && text.contains("weight = 0 # thin\n"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_save_waits_for_a_hand_edit_the_watcher_has_not_adopted() {
        let dir = std::env::temp_dir().join(format!("finch-hand-edit-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.toml");
        let mut settings = Settings::default();
        settings.window.width = 800.;
        // A missing file and a first run write.
        save(&path, &settings).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("width = 800"));
        // Another program edits the file; the next write must not undo it.
        let edited = "[window]\nwidth = 650\n# by hand\n";
        std::fs::write(&path, edited).unwrap();
        settings.window.width = 900.;
        save(&path, &settings).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
        // The watcher takes the edit in, and Finch's own writes land again.
        super::seen_set(&path, edited.to_string());
        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("width = 900") && text.contains("# by hand"), "{text}");
        // An edit the watcher never adopts stops holding saves back once it is old.
        let stale = "[window]\nwidth = 640\n";
        std::fs::write(&path, stale).unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(60)).unwrap();
        settings.window.width = 910.;
        save(&path, &settings).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("width = 910"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_template_lands_in_a_missing_or_empty_file_and_never_in_one_with_values() {
        let dir = std::env::temp_dir().join(format!("finch-template-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("sub/settings.toml");
        write_template(&file);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), TEMPLATE);
        std::fs::write(&file, "").unwrap();
        write_template(&file);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), TEMPLATE);
        let mine = "[window]\nwidth = 600\n";
        std::fs::write(&file, mine).unwrap();
        write_template(&file);
        assert_eq!(std::fs::read(&file).unwrap(), mine.as_bytes());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_template_parses_to_the_usual_settings() {
        let settings: Settings = toml::from_str(TEMPLATE).unwrap();
        assert_eq!(toml::to_string_pretty(&settings).unwrap(), toml::to_string_pretty(&Settings::default()).unwrap());
    }

    #[test]
    fn the_save_settings_are_off_until_a_file_turns_them_on() {
        let usual = Settings::default().editor;
        assert!(!usual.trim_trailing_whitespace && !usual.insert_final_newline);
        let on: Settings = toml::from_str("[editor]\ntrim_trailing_whitespace = true\ninsert_final_newline = true\n").unwrap();
        assert!(on.editor.trim_trailing_whitespace && on.editor.insert_final_newline);
    }

    #[test]
    fn the_text_styles_are_mono_until_a_file_picks_one() {
        let usual = Settings::default().editor;
        assert_eq!((usual.prose_style, usual.code_style), (Typeface::Mono, Typeface::Mono));
        let picked: Settings = toml::from_str("[editor]\nprose_style = \"serif\"\ncode_style = \"sans\"\n").unwrap();
        assert_eq!((picked.editor.prose_style, picked.editor.code_style), (Typeface::Serif, Typeface::Sans));
        let back: Settings = toml::from_str(&toml::to_string_pretty(&picked).unwrap()).unwrap();
        assert_eq!((back.editor.prose_style, back.editor.code_style), (Typeface::Serif, Typeface::Sans));
    }

    #[test]
    fn the_template_lists_every_key_the_loader_reads() {
        let mut settings = Settings::default();
        settings.window.x = Some(1.);
        settings.window.y = Some(1.);
        settings.window.display = Some("d".into());
        settings.explorer.last_root = Some("/a".into());
        settings.explorer.open_files = vec!["/a".into()];
        settings.explorer.pinned_tabs = 1;
        settings.explorer.panes = Some(super::SavedPanes::Pane { files: vec!["/a".into()], active: 0, pinned: 0 });
        let toml::Value::Table(sections) = toml::Value::try_from(&settings).unwrap() else { panic!("settings aren't a table") };
        let mut missing = Vec::new();
        for (section, keys) in &sections {
            let toml::Value::Table(keys) = keys else { panic!("{section} isn't a table") };
            let header = format!("\n[{section}]\n");
            let start = TEMPLATE.find(&header).unwrap_or_else(|| panic!("no [{section}] in the template")) + header.len();
            let body = TEMPLATE[start..].split("\n[").next().unwrap();
            for key in keys.keys().map(String::as_str) {
                if !body.contains(&format!("# {key} =")) && !body.contains(&format!("[{section}.{key}]")) {
                    missing.push(format!("{section}.{key}"));
                }
            }
        }
        assert!(missing.is_empty(), "the template leaves out {missing:?}");
    }

    #[test]
    fn only_a_missing_or_blank_file_gets_the_template() {
        let missing = Err(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert_eq!(template_for(missing), Some(TEMPLATE));
        assert_eq!(template_for(Ok(String::new())), Some(TEMPLATE));
        assert_eq!(template_for(Ok(" \n\t\n".into())), Some(TEMPLATE));
        assert_eq!(template_for(Ok("[editor]\nfont_size = 20\n".into())), None);
        assert_eq!(template_for(Ok("# mine\n".into())), None);
        assert_eq!(template_for(Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))), None);
    }

    #[test]
    fn a_file_from_before_pins_loads_with_none_pinned() {
        let old = "[explorer]\nopen_files = [\"/a\", \"/b\"]\nactive_tab = 1\n\n[explorer.panes]\nkind = \"pane\"\nfiles = [\"/a\", \"/b\"]\nactive = 1\n";
        let settings: Settings = toml::from_str(old).unwrap();
        assert_eq!(settings.explorer.pinned_tabs, 0);
        assert_eq!(settings.explorer.panes, Some(SavedPanes::Pane { files: vec!["/a".into(), "/b".into()], active: 1, pinned: 0 }));
    }

    #[test]
    fn pins_survive_a_round_trip_and_zero_is_not_written() {
        let mut settings = Settings::default();
        settings.explorer.open_files = vec!["/a".into(), "/b".into()];
        settings.explorer.pinned_tabs = 1;
        settings.explorer.panes = Some(SavedPanes::Pane { files: vec!["/a".into(), "/b".into()], active: 0, pinned: 2 });
        let text = toml::to_string_pretty(&settings).unwrap();
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back.explorer.pinned_tabs, 1);
        assert_eq!(back.explorer.panes, settings.explorer.panes);
        let none = toml::to_string_pretty(&Settings::default()).unwrap();
        assert!(!none.contains("pinned"));
    }

    #[test]
    fn a_split_saved_over_the_template_keeps_the_explorer_comments_above_the_pane_tables() {
        let dir = std::env::temp_dir().join(format!("finch-template-split-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.toml");
        std::fs::write(&path, TEMPLATE).unwrap();
        let mut settings = Settings::default();
        let pane = |file: &str| Box::new(SavedPanes::Pane { files: vec![file.into()], active: 0, pinned: 0 });
        settings.explorer.panes = Some(SavedPanes::Split { axis: super::SplitAxis::Right, ratio: 0.5, first: pane("/a"), second: pane("/b") });
        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let panes = text.find("[explorer.panes]").unwrap();
        assert!(text.find("# show_hidden =").unwrap() < panes, "{text}");
        assert!(text.find("[preview]").unwrap() > text.rfind("[explorer.panes.").unwrap(), "{text}");
        assert!(text.find("# visible = false").unwrap() > text.find("[preview]").unwrap(), "{text}");
        // A second save changes nothing.
        save(&path, &settings).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_save_keeps_comments_blank_lines_key_order_and_unknown_keys() {
        let dir = std::env::temp_dir().join(format!("finch-settings-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.toml");
        let old = "# my settings\n[editor]\nsoft_wrap = true # wrap long lines\nmy_key = \"mine\"\nfont_size = 14\n\n# the window\n[window]\nheight = 500\nwidth = 600\n\n[custom]\nkeep = 1\n\n[explorer]\nlast_root = \"/old\"\nvisible = true\n";
        std::fs::write(&path, old).unwrap();
        let mut settings: Settings = toml::from_str(old).unwrap();
        settings.editor.font_size = Some(20.);
        settings.editor.soft_wrap = false;
        settings.window.width = 700.;
        settings.explorer.last_root = None;
        settings.explorer.panes = Some(SavedPanes::Pane { files: vec!["/a".into()], active: 0, pinned: 0 });
        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(text.starts_with("# my settings\n"), "{text}");
        assert!(text.contains("soft_wrap = false # wrap long lines\n"), "{text}");
        assert!(text.contains("\n\n# the window\n[window]\nheight = 500\nwidth = 700"), "{text}");
        assert!(text.contains("my_key = \"mine\""), "{text}");
        assert!(text.contains("[custom]\nkeep = 1"), "{text}");
        assert!(text.find("soft_wrap").unwrap() < text.find("my_key").unwrap());
        assert!(text.find("my_key").unwrap() < text.find("font_size").unwrap());
        assert!(text.contains("font_size = 20"), "{text}");
        assert!(!text.contains("last_root"), "{text}");
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back.editor.font_size, Some(20.));
        assert_eq!(back.window.width, 700.);
        assert_eq!(back.explorer.panes, settings.explorer.panes);
    }

    #[test]
    fn split_pane_tables_sit_right_after_their_section_and_stay_put() {
        let dir = std::env::temp_dir().join(format!("finch-settings-split-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.toml");
        std::fs::write(&path, "[editor]\ndisplay = \"mine\"\n\n[explorer]\nvisible = true\n\n[preview]\nvisible = false\n\n[text]\nweight = 500.0\n").unwrap();
        let mut settings = Settings::default();
        let pane = |file: &str| Box::new(SavedPanes::Pane { files: vec![file.into()], active: 0, pinned: 0 });
        settings.explorer.panes = Some(SavedPanes::Split { axis: super::SplitAxis::Right, ratio: 0.5, first: pane("/a"), second: pane("/b") });
        save(&path, &settings).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        save(&path, &settings).unwrap();
        let second = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(first, second);
        assert!(first.contains("display = \"mine\""), "{first}");
        let headers: Vec<&str> = first.lines().filter(|line| line.starts_with('[')).collect();
        let explorer = headers.iter().position(|h| *h == "[explorer]").unwrap();
        let panes: Vec<usize> = headers.iter().enumerate().filter(|(_, h)| h.starts_with("[explorer.panes")).map(|(i, _)| i).collect();
        assert!(panes.len() >= 3, "{first}");
        assert_eq!(panes, (explorer + 1..explorer + 1 + panes.len()).collect::<Vec<_>>(), "{first}");
    }

    #[test]
    fn whole_sizes_are_written_as_integers_and_both_forms_read_back() {
        let dir = std::env::temp_dir().join(format!("finch-j3-07-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.toml");
        let old = "# mine\n[window]\nwidth = 1440.0 # wide\nheight = 900.0\nzzz = 1.0\n";
        std::fs::write(&path, old).unwrap();
        let mut settings: Settings = toml::from_str(old).unwrap();
        settings.window.width = 1440.;
        settings.window.x = Some(120.);
        settings.window.y = Some(37.5);
        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("width = 1440 # wide\n") && text.contains("height = 900\n") && text.contains("x = 120\n") && text.contains("y = 37.5\n"), "{text}");
        assert!(text.starts_with("# mine\n") && text.contains("zzz = 1.0\n"), "{text}");
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!((back.window.width, back.window.x, back.window.y), (1440., Some(120.), Some(37.5)));
        let float: Settings = toml::from_str("[window]\nwidth = 1440.0\n").unwrap();
        assert_eq!(float.window.width, back.window.width);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writing_code_color_overrides_keeps_the_files_other_keys_and_comments() {
        let dir = std::env::temp_dir().join(format!("finch-wg9e-overrides-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.toml");
        let old = "# my settings\n[editor]\nfont_size = 16 # big\n\n[custom]\nkeep = 1\n\n[code_colors]\nscheme = \"dracula\" # my favorite\n\n# my dark picks\n[code_colors.dark]\n# keywords pop\nkeyword = \"#FC5FA3\" # pink\nstring = \"#00ff7f\"\n";
        std::fs::write(&path, old).unwrap();
        let mut settings: Settings = toml::from_str(old).unwrap();
        settings.code_colors.dark.insert("keyword".into(), "#112233".into());
        settings.code_colors.dark.remove("string");
        settings.code_colors.dark.insert("variable.parameter".into(), "#AABBCC".into());
        settings.code_colors.light.insert("comment".into(), "#777777".into());
        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# my settings\n") && text.contains("font_size = 16 # big\n") && text.contains("[custom]\nkeep = 1\n"), "{text}");
        assert!(text.contains("scheme = \"dracula\" # my favorite\n") && text.contains("# my dark picks\n[code_colors.dark]\n# keywords pop\n"), "{text}");
        assert!(text.contains("keyword = \"#112233\" # pink\n") && !text.contains("string = ") && text.contains("\"variable.parameter\" = \"#AABBCC\""), "{text}");
        assert!(text.contains("[code_colors.light]\ncomment = \"#777777\""), "{text}");
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back.code_colors.dark, settings.code_colors.dark);
        assert_eq!(back.code_colors.light, settings.code_colors.light);
        assert_eq!(back.code_colors.scheme, super::Scheme::Dracula);

        // The last overrides removed: the file still parses, and everything that wasn't an override is as it was.
        settings.code_colors.dark.clear();
        settings.code_colors.light.clear();
        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let back: Settings = toml::from_str(&text).unwrap();
        assert!(back.code_colors.dark.is_empty() && back.code_colors.light.is_empty(), "{text}");
        assert!(text.starts_with("# my settings\n") && text.contains("font_size = 16 # big\n") && text.contains("[custom]\nkeep = 1\n"), "{text}");
        assert!(text.contains("scheme = \"dracula\" # my favorite\n") && text.contains("# my dark picks\n"), "{text}");
        assert!(!text.contains("keyword") && !text.contains("variable.parameter") && !text.contains("[code_colors.light]"), "{text}");

        // A file with no `[code_colors]` at all takes the tables, and loses them again, still valid.
        let path = dir.join("fresh.toml");
        let old = "# fresh\n[editor]\nsoft_wrap = false\n";
        std::fs::write(&path, old).unwrap();
        let mut settings: Settings = toml::from_str(old).unwrap();
        settings.code_colors.light.insert("keyword".into(), "#9B2393".into());
        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# fresh\n") && text.contains("[code_colors.light]\nkeyword = \"#9B2393\""), "{text}");
        assert_eq!(toml::from_str::<Settings>(&text).unwrap().code_colors.light, settings.code_colors.light);
        settings.code_colors.light.clear();
        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(toml::from_str::<Settings>(&text).unwrap().code_colors.light.is_empty(), "{text}");
        assert!(text.starts_with("# fresh\n") && text.contains("soft_wrap = false") && !text.contains("[code_colors.light]"), "{text}");

        // `[code_colors.light]` without a `[code_colors]` header: a new dark table lands after it, never above the file's start.
        let path = dir.join("headerless.toml");
        let old = "# top\n[editor]\nsoft_wrap = false\n\n[code_colors.light]\nstring = \"#0A7D2C\"\n";
        std::fs::write(&path, old).unwrap();
        let mut settings: Settings = toml::from_str(old).unwrap();
        settings.code_colors.dark.insert("keyword".into(), "#FF6AC1".into());
        save(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed = toml::from_str::<Settings>(&text).unwrap();
        assert_eq!(parsed.code_colors.dark, settings.code_colors.dark);
        assert_eq!(parsed.code_colors.light, settings.code_colors.light);
        assert!(text.starts_with("# top\n[editor]") && text.find("[code_colors.dark]") > text.find("[code_colors.light]"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
