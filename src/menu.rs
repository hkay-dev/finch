//! The menu bar: App, File, Edit, View, Window and Help, built in one place.
//!
//! `bar` is the one function that lists every item; a new item goes there, with
//! its action and key binding in `app.rs`. The bar is set again only when an
//! item's title, check mark or enabled state changed (`refresh`), to avoid
//! unnecessary native menu work. Our platform patch bounds the actions to two bars. What changes the
//! bar: the explorer and preview toggles (their titles) and the soft wrap of
//! the file in the key window (the Soft wrap check) and whether it has two tabs
//! (the Show Previous/Next Tab items) and the key editor's find options (the Match case and
//! Whole word checks) and whether the showing tab has undo history (Undo history), so the bar is refreshed when those change
//! and when another window becomes the key one.
//!
//! `GPUI_EDITOR_DUMP_MENUS=1` prints the live bar to stdout after each build,
//! one item per line as `path<TAB>key<TAB>state<TAB>symbol`, for checking it without
//! opening a menu (native menus can't open in a background launch).

use std::{
    ffi::{CStr, CString, c_char},
    path::{Path, PathBuf},
};

use encoding_rs::Encoding;
use gpui_kit::component::input as editor;
use gpui_kit::*;
use objc2::{
    class, msg_send,
    rc::Retained,
    runtime::{AnyObject, Bool},
};

use crate::{
    app::{
        About, BringAllToFront, ClearRecent, CloseOtherTabs, CloseTab, CloseTabsToTheLeft, CloseTabsToTheRight, CloseWindow, FindNext, FindPrevious, FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp, HideApp, HideOthers, JumpToSelection, Minimize, MoveTabDown, MoveTabLeft, MoveTabRight, MoveTabToNewWindow, MoveTabUp, NewTab, NewWindow, NextTab,
        FocusNextPane, FocusPreviousPane, EvenOutPanes, MergePanes, MergeAllWindows, MoveTabEarlier, MoveTabLater,
        Open, OpenRecent, OpenSettings, PrevTab, Print, Quit, ReopenClosedTab, ReopenWithEncoding, RevertToSaved, Save, SaveAll, SaveAs, SetLineEndings, ShowAll, ShowGoToLine, ShowGoToSymbol, ShowHelp, ShowFindInFolder, ShowOpenQuickly, ShowTab1, ShowTab9, SplitDown, SplitRight, ToggleExplorer, ToggleMinimal, TogglePinTab, TogglePreview,
        ConvertToEncoding, SetWritingGoal, FoldAtCaret, UnfoldAtCaret, FoldAll, UnfoldAll, NextChange, PreviousChange,
        ToggleFullScreen, ToggleInvisibles, ToggleLineNumbers, ToggleMatchCase, ToggleReadingWidth, ToggleRegex, ToggleSmartPunctuation, ToggleSoftWrap, CycleWrapWidth, ToggleStyledMarkdown, ToggleStyleCheck, ToggleTypewriter, ToggleWholeWord, Zoom, BiggerText, SmallerText, ActualTextSize, SetTextStyle, SetCodeScheme, ToggleVivid, ToggleColoredRoles, ToggleRainbowBrackets, ToggleSemanticColors,
        ReplaceAllMatches, ReplaceMatch, ReplaceMatchAndFind, SelectAllSearchMatches, SelectNextOccurrence, SkipNextOccurrence, ToggleFindInSelection, MarkdownBold, MarkdownCode, MarkdownItalic, MarkdownLink, MarkdownStrike, ToggleScratch,
        ToggleIgnoreDiacritics,
    },
    encoding::{self, Endings},
    explorer::{CollapseAllFolders, EnclosingFolder, FocusExplorer, FocusFilter, RevealCurrentFile, SortExplorer, ToggleHiddenFiles, ToggleIgnoredFiles},
    recent, settings,
    templates::{self, NewFromTemplate, OpenTemplatesFolder, Template},
    workspace::{self, BrowseVersions, CopyFilePath, DuplicateFile, ExportHtml, MoveFile, MoveItemHere, NewFileInExplorer, NewFolderInExplorer, RenameFile, ShowDraftList, ShowFileInFinder, ShowUndoHistory, UseSelectionForFind, UseSelectionForReplace, Workspace},
};

const APP: &str = "Finch";

/// What the bar shows now: each item's title, check mark and disabled state in
/// menu order, and the paths behind the Open Recent items, to tell whether the bar needs
/// setting again.
#[derive(Default, PartialEq)]
struct Shown(Vec<(String, bool, bool)>, Vec<PathBuf>);

impl Global for Shown {}

/// The `Look` the bar was last built from: a refresh whose look is the same builds no item tree.
#[derive(Default)]
struct Built(Option<Look>);

impl Global for Built {}

/// The recent items File > Open Recent lists. The list is read from macOS on the main thread only
/// when it may have changed: at start, on an open or save, on Clear menu and on window activation,
/// not on every refresh; each path is checked to exist on a background thread, and the menu is
/// set again when that lands (the old list shows meanwhile). gpui-pre's `on_will_open_app_menu` is
/// crate-private, so a recent deleted while the app stays in front shows until the next activation.
struct Recents {
    paths: Vec<PathBuf>,
    /// The check in flight; a newer read drops it, so an old answer never lands over a new one.
    check: Option<Task<()>>,
}

impl Global for Recents {}

/// Reads the recent items again; the menu shows them once their files are checked.
pub fn reload_recents(cx: &mut App) {
    let listed = recent::list();
    if listed.is_empty() {
        let recents = cx.global_mut::<Recents>();
        recents.paths.clear();
        recents.check = None;
        return;
    }
    let check = cx.spawn(async move |cx| {
        let kept = cx.background_executor().spawn(async move { recent::keep_existing(listed) }).await;
        cx.update(|cx| {
            let recents = cx.global_mut::<Recents>();
            recents.check = None;
            if recents.paths != kept {
                recents.paths = kept;
                refresh(cx);
            }
        });
    });
    cx.global_mut::<Recents>().check = Some(check);
}

/// What the key window's tabs let the menu bar do, compared field by field (`workspace::TabMenu` has no `PartialEq`).
#[derive(Default)]
struct Tab(workspace::TabMenu);

impl std::ops::Deref for Tab {
    type Target = workspace::TabMenu;

    fn deref(&self) -> &workspace::TabMenu {
        &self.0
    }
}

impl PartialEq for Tab {
    fn eq(&self, other: &Self) -> bool {
        let (a, b) = (&self.0, &other.0);
        (a.tabs, a.pinned, a.can_close_others, a.can_close_right, a.can_close_left, a.can_earlier, a.can_later, a.window_tabs, a.can_reopen, a.panes, a.pane_count, a.windows)
            == (b.tabs, b.pinned, b.can_close_others, b.can_close_right, b.can_close_left, b.can_earlier, b.can_later, b.window_tabs, b.can_reopen, b.panes, b.pane_count, b.windows)
    }
}

/// What the bar's titles and checks depend on: everything `bar` reads, so equal looks make equal bars.
#[derive(Default, PartialEq)]
struct Look {
    explorer_visible: bool,
    preview_visible: bool,
    soft_wrap: bool,
    /// Whether the file in front wraps at the window's width, past the reading column.
    wrap_full: bool,
    full_screen: bool,
    /// Whether the key window is in Minimal mode.
    minimal: bool,
    /// Whether spaces and tabs show (a setting, the same in every window).
    show_invisibles: bool,
    /// Whether the gutter shows line numbers (a setting, the same in every window).
    line_numbers: bool,
    /// Whether wrapped text stops at a reading width (a setting, the same in every window).
    reading_width: bool,
    /// Whether typing makes curly quotes, an ellipsis and en dashes (a setting, the same in every window).
    smart_punctuation: bool,
    /// Whether the line you type stays at one height, the page gliding under it (a setting, the same in every window).
    typewriter: bool,
    /// Whether Markdown shows semibold headings and faded marks (a setting, the same in every window).
    styled_markdown: bool,
    /// Whether the style check marks listed words and long sentences (a setting, the same in every window).
    style_check: bool,
    /// Whether misspelled words are underlined (a setting, the same in every window).
    check_spelling: bool,
    /// The editor font size in points (a setting, the same in every window): Bigger stops at the most, Smaller at the least.
    font_size: f32,
    /// The style the key window's file draws in (a setting per kind of file, the same in every window).
    text_style: settings::Typeface,
    /// How code is colored (a setting, the same in every window): View > Code colors' scheme and switches.
    code_colors: settings::CodeColors,
    /// What the key window's tabs allow: the tab items need two tabs, and the close, pin and reopen items their own conditions.
    tab: Tab,
    /// Whether the key window's file has edits to revert (File > Revert to saved).
    can_revert: bool,
    /// Whether the key window's showing tab has undo history and takes edits (Edit > Undo history).
    can_undo_history: bool,
    /// The key window has a showing tab (File > Print).
    has_tab: bool,
    /// Whether the key window's showing tab has a file on disk (File > Rename, Duplicate, Move to, Show in Finder, Copy path).
    has_file: bool,
    /// Whether the key window has a folder tree (File > New file in explorer, New folder in explorer).
    has_tree: bool,
    /// Whether Edit > Move item here applies: the key window has a tree and the clipboard holds files.
    can_move_here: bool,
    /// The key window's showing file is Markdown, so Edit > Markdown applies.
    markdown: bool,
    /// The key window's showing file is JSON, so Edit > Transformations > Format JSON applies.
    json: bool,
    /// The encoding of the key window's file, `None` for an Untitled page or no tab (File > Reopen with encoding).
    encoding: Option<&'static Encoding>,
    /// The encoding the key window's page saves in, `None` with no tab (File > Convert to).
    saves_in: Option<&'static Encoding>,
    /// The line endings of the key window's file, `None` with no tab (Format > Line endings).
    endings: Option<Endings>,
    /// What the key window lights (View > Focus).
    focus: crate::focus::Mode,
    /// The parts of speech View > Highlight tints (a setting, the same in every window).
    highlight: crate::parts_of_speech::Classes,
    show_hidden: bool,
    show_ignored: bool,
    /// How the key window's tree is sorted (View > Sort by).
    explorer_sort: settings::ExplorerSort,
    /// The scratch window is open (File > Show or Hide scratch window).
    scratch_open: bool,
    /// The recent files and folders File > Open Recent lists.
    recents: Vec<PathBuf>,
    /// The files of the templates folder File > New from template lists.
    templates: Vec<PathBuf>,
    /// The key editor's find options, as the find bar's buttons show them.
    match_case: bool,
    ignore_diacritics: bool,
    whole_word: bool,
    regex: bool,
    can_select_all_matches: bool,
    can_select_next_match: bool,
    can_skip_next_match: bool,
    /// The key editor's find bar is open with its replace row, so the replace commands have a field to read.
    can_replace: bool,
    /// The key editor's find bar is open with the search limited to the selection (Find in selection).
    in_selection: bool,
    /// The key editor's find bar is open, so Find in selection has a search to limit.
    can_scope: bool,
}

/// What `look` reads of the key editor's search.
#[derive(Default)]
struct FindLook {
    match_case: bool,
    ignore_diacritics: bool,
    whole_word: bool,
    regex: bool,
    can_select_all_matches: bool,
    can_select_next_match: bool,
    can_skip_next_match: bool,
    can_replace: bool,
    in_selection: bool,
    can_scope: bool,
}

fn look(cx: &mut App) -> Look {
    let full_screen = crate::control::target(cx).ok().is_some_and(|(handle, _)| {
        cx.update_window(handle, |_, window, _| window.is_fullscreen()).unwrap_or(false)
    });
    let settings = settings::get(cx);
    // One key window read serves the four menu readers; the editor and document come from one lookup.
    let key = workspace::key_workspace(cx);
    let key_document = crate::app::key_document(cx);
    let find = key_document.as_ref().and_then(|(_, document)| document.read(cx).built_editor().cloned()).map(|editor| {
        let state = editor.read(cx);
        let session = state.search_session();
        FindLook {
            match_case: !session.case_insensitive,
            ignore_diacritics: session.ignore_diacritics(),
            whole_word: session.whole_word,
            regex: session.regex,
            can_select_all_matches: state.can_select_all_search_matches(),
            can_select_next_match: state.can_select_next_occurrence(),
            can_skip_next_match: state.can_skip_next_occurrence(),
            can_replace: session.open && session.replace_mode && state.is_replaceable(),
            in_selection: session.in_selection(),
            can_scope: session.open,
        }
    }).unwrap_or_default();
    let (soft_wrap, wrap_full) = workspace::key_window_wraps(key.as_ref(), cx).unwrap_or((settings.editor.soft_wrap, false));
    Look {
        explorer_visible: settings.explorer.visible,
        preview_visible: settings.preview.visible,
        soft_wrap,
        wrap_full,
        full_screen,
        minimal: workspace::key_window_minimal(key.as_ref(), cx),
        show_invisibles: settings.editor.show_invisibles,
        line_numbers: settings.editor.line_numbers,
        reading_width: settings.editor.reading_width,
        smart_punctuation: settings.editor.smart_punctuation,
        typewriter: settings.editor.typewriter,
        styled_markdown: settings.editor.styled_markdown,
        style_check: settings.editor.style_check,
        check_spelling: settings.editor.check_spelling,
        font_size: settings.editor.font_size.unwrap_or(settings::FONT_USUAL),
        text_style: text_style_of(key_document.as_ref(), settings, cx),
        code_colors: settings.code_colors.clone(),
        tab: Tab(workspace::key_window_tab_menu(key.as_ref(), cx)),
        can_revert: workspace::key_window_can_revert(key.as_ref(), cx),
        can_undo_history: workspace::key_window_can_undo_history(key.as_ref(), cx),
        has_file: workspace::key_window_has_file(key.as_ref(), cx),
        has_tree: workspace::key_window_has_tree(key.as_ref(), cx),
        can_move_here: workspace::key_window_has_tree(key.as_ref(), cx) && native::pasteboard_has_files(),
        has_tab: key_document.is_some(),
        markdown: key_document.as_ref().is_some_and(|(_, document)| document.read(cx).has_preview()),
        json: key_document.as_ref().is_some_and(|(_, document)| document.read(cx).language(cx) == "json"),
        encoding: key_document.as_ref().and_then(|(_, document)| {
            let document = document.read(cx);
            document.can_reopen().then(|| document.encoding())
        }),
        saves_in: key_document.as_ref().map(|(_, document)| document.read(cx).encoding()),
        endings: key_document.as_ref().map(|(_, document)| document.read(cx).endings()),
        focus: workspace::key_window_focus(key.as_ref(), cx),
        highlight: crate::parts_of_speech::Classes::of(&settings.editor.highlight),
        show_hidden: settings.explorer.show_hidden,
        show_ignored: settings.explorer.show_ignored,
        explorer_sort: workspace::key_window_sort(key.as_ref(), cx),
        recents: cx.global::<Recents>().paths.clone(),
        scratch_open: crate::scratch::is_open(cx),
        templates: templates::files(cx).to_vec(),
        match_case: find.match_case,
        ignore_diacritics: find.ignore_diacritics,
        whole_word: find.whole_word,
        regex: find.regex,
        can_select_all_matches: find.can_select_all_matches,
        can_select_next_match: find.can_select_next_match,
        can_skip_next_match: find.can_skip_next_match,
        can_replace: find.can_replace,
        in_selection: find.in_selection,
        can_scope: find.can_scope,
    }
}

/// File > Open Recent: the recent items (their file names; the full path is what opens), then
/// Clear menu. Grey while there are none.
fn open_recent(recents: &[PathBuf]) -> MenuItem {
    if recents.is_empty() {
        return MenuItem::submenu(Menu::new("Open recent").items([item("No recent items", NoAction).disabled(true)]).disabled(true));
    }
    let mut items: Vec<MenuItem> = recents
        .iter()
        .map(|path| item(recent_title(path, recents), OpenRecent { path: path.clone() }))
        .collect();
    items.push(MenuItem::separator());
    items.push(item("Clear menu", ClearRecent));
    MenuItem::submenu(Menu::new("Open recent").items(items))
}

/// A recent item's title: its file name, and its parent folder's name in parentheses when another
/// recent item has the same file name ("README.md (notes)"), the way Finder's and TextEdit's
/// Open Recent tell two of them apart.
fn recent_title(path: &Path, recents: &[PathBuf]) -> String {
    let name = |path: &Path| path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned();
    let title = name(path);
    if recents.iter().filter(|other| name(other) == title).count() < 2 {
        return title;
    }
    match path.parent().and_then(Path::file_name) {
        Some(folder) => format!("{title} ({})", folder.to_string_lossy()),
        None => title,
    }
}

/// File > New from template: the built-in templates, then the files of the templates folder, then
/// the item that shows that folder.
fn new_from_template(files: &[PathBuf]) -> MenuItem {
    let mut items: Vec<MenuItem> = templates::BUILT_IN
        .iter()
        .enumerate()
        .map(|(ix, (title, ..))| item(*title, NewFromTemplate { template: Template::BuiltIn(ix) }))
        .collect();
    if !files.is_empty() {
        items.push(MenuItem::separator());
        items.extend(files.iter().zip(templates::file_titles(files)).map(|(path, title)| item(title, NewFromTemplate { template: Template::File(path.clone()) })));
    }
    items.push(MenuItem::separator());
    items.push(item("Open templates folder", OpenTemplatesFolder));
    MenuItem::submenu(Menu::new("New from template").items(items))
}

/// File > Reopen with encoding: each common encoding by its Mac name, a check by the key tab's own,
/// then a More encodings submenu with the rest of the labelled ones (the check shows there too).
/// Grey while the key window shows no file (an Untitled page, or no tab).
fn reopen_with_encoding(current: Option<&'static Encoding>) -> MenuItem {
    let row = |&encoding: &&'static Encoding| item(encoding::label(encoding).into_owned(), ReopenWithEncoding { encoding }).checked(current == Some(encoding));
    MenuItem::submenu(Menu::new("Reopen with encoding").items(encoding_rows(row, current)).disabled(current.is_none()))
}

/// File > Convert to: the same encodings, a check by the key page's own; the next save writes the one picked.
/// Grey only with no tab: an Untitled page converts too.
fn convert_to_encoding(current: Option<&'static Encoding>) -> MenuItem {
    let row = |&encoding: &&'static Encoding| item(encoding::label(encoding).into_owned(), ConvertToEncoding { encoding }).checked(current == Some(encoding));
    MenuItem::submenu(Menu::new("Convert to").items(encoding_rows(row, current)).disabled(current.is_none()))
}

/// The rows of an encoding list: the short list, a separator, and the More encodings submenu.
fn encoding_rows(row: impl Fn(&&'static Encoding) -> MenuItem, current: Option<&'static Encoding>) -> Vec<MenuItem> {
    let mut items: Vec<MenuItem> = encoding::REOPEN_CHOICES.iter().map(&row).collect();
    items.push(MenuItem::separator());
    items.push(MenuItem::submenu(Menu::new("More encodings").items(encoding::MORE_CHOICES.iter().map(&row)).disabled(current.is_none())));
    items
}

/// Format > Line endings: LF and CRLF, a check by the key file's own (neither for a Mixed file), the file's own grey (it is already that).
/// Grey as a whole while the key window has no tab.
fn line_endings(current: Option<Endings>) -> MenuItem {
    let row = |title: &str, crlf: bool, style: Endings| {
        item(title.to_string(), SetLineEndings { crlf }).checked(current == Some(style)).disabled(current == Some(style))
    };
    MenuItem::submenu(Menu::new("Line endings").items([row("LF", false, Endings::Lf), row("CRLF", true, Endings::Crlf)]).disabled(current.is_none()))
}

/// The style the key window's kind of file draws in, which View > Text style checks: prose for Markdown and plain text, code for the rest (code with no tab).
fn text_style_of(key_document: Option<&(AnyWindowHandle, Entity<crate::document::Document>)>, settings: &settings::Settings, cx: &App) -> settings::Typeface {
    let prose = key_document.is_some_and(|(_, document)| crate::text_style::is_prose(document.read(cx).language(cx)));
    crate::text_style::of(settings, prose)
}

/// View > Focus: Off, Sentence, Line and Paragraph, one checked (what the key window lights); grey as a whole while the key window has no tab.
fn focus_menu(current: crate::focus::Mode, has_tab: bool) -> MenuItem {
    use crate::focus::{Mode, SetFocus};
    let row = |title: &str, mode: Mode| item(title.to_string(), SetFocus { mode }).checked(current == mode);
    MenuItem::submenu(Menu::new("Focus").items([row("Off", Mode::Off), MenuItem::separator(), row("Sentence", Mode::Sentence), row("Line", Mode::Line), row("Paragraph", Mode::Paragraph)]).disabled(!has_tab))
}

/// View > Highlight: Nouns, Verbs, Adjectives, Adverbs and Conjunctions, each checked while its words are tinted (Markdown and text files).
fn highlight_menu(on: crate::parts_of_speech::Classes) -> MenuItem {
    use crate::parts_of_speech::{Class, ToggleHighlight};
    MenuItem::submenu(Menu::new("Highlight").items(Class::ALL.map(|class| item(class.title(), ToggleHighlight { class }).checked(on.has(class)))))
}

fn item(title: impl Into<SharedString>, action: impl Action) -> MenuItem {
    MenuItem::action(title, action)
}

fn show_or_hide(visible: bool, what: &str) -> String {
    format!("{} {what}", if visible { "Hide" } else { "Show" })
}

/// Every menu of the bar. Edit items send the editor's own actions, which the
/// focused editor answers; GPUI greys an item nothing answers.
fn bar(look: &Look) -> Vec<Menu> {
    vec![
        Menu::new(APP).items([
            item(format!("About {APP}"), About),
            MenuItem::separator(),
            item("Settings\u{2026}", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            item(format!("Hide {APP}"), HideApp),
            item("Hide others", HideOthers),
            item("Show all", ShowAll),
            MenuItem::separator(),
            item(format!("Quit {APP}"), Quit),
        ]),
        Menu::new("File").items([
            item("New tab", NewTab),
            new_from_template(&look.templates),
            item("New window", NewWindow),
            item("New file in explorer", NewFileInExplorer).disabled(!look.has_tree),
            item("New folder in explorer", NewFolderInExplorer).disabled(!look.has_tree),
            item("Open\u{2026}", Open),
            item("Open quickly\u{2026}", ShowOpenQuickly),
            item("Open draft\u{2026}", ShowDraftList),
            item(show_or_hide(look.scratch_open, "scratch window"), ToggleScratch),
            open_recent(&look.recents),
            MenuItem::separator(),
            item("Save", Save),
            item("Save as\u{2026}", SaveAs),
            item("Save all", SaveAll),
            item("Revert to saved", RevertToSaved).disabled(!look.can_revert),
            item("Browse versions\u{2026}", BrowseVersions).disabled(!look.has_file),
            reopen_with_encoding(look.encoding),
            convert_to_encoding(look.saves_in),
            MenuItem::separator(),
            item("Print\u{2026}", Print).disabled(!look.has_tab),
            item("Export as HTML\u{2026}", ExportHtml).disabled(!look.markdown),
            MenuItem::separator(),
            item("Rename\u{2026}", RenameFile).disabled(!look.has_file),
            item("Duplicate", DuplicateFile).disabled(!look.has_file),
            item("Move to\u{2026}", MoveFile).disabled(!look.has_file),
            item("Show in Finder", ShowFileInFinder).disabled(!look.has_file),
            item("Copy path", CopyFilePath).disabled(!look.has_file),
            MenuItem::separator(),
            item("Close tab", CloseTab),
            item("Close other tabs", CloseOtherTabs).disabled(!look.tab.can_close_others),
            item("Close tabs to the left", CloseTabsToTheLeft).disabled(!look.tab.can_close_left),
            item("Close tabs to the right", CloseTabsToTheRight).disabled(!look.tab.can_close_right),
            item("Reopen closed tab", ReopenClosedTab).disabled(!look.tab.can_reopen),
            item("Close window", CloseWindow),
        ]),
        Menu::new("Edit").items([
            item("Undo", editor::Undo),
            item("Redo", editor::Redo),
            item("Undo history\u{2026}", ShowUndoHistory).disabled(!look.can_undo_history),
            MenuItem::separator(),
            item("Cut", editor::Cut),
            item("Copy", editor::Copy),
            item("Paste", editor::Paste),
            item("Move item here", MoveItemHere).disabled(!look.can_move_here),
            MenuItem::separator(),
            item("Select all", editor::SelectAll),
            item("Select line", gpui_kit::base::input::SelectLine),
            item("Select inside brackets", gpui_kit::base::input::SelectToEnclosingBracket),
            item("Expand selection", editor::SelectLargerSyntaxNode),
            item("Shrink selection", editor::SelectSmallerSyntaxNode),
            MenuItem::separator(),
            item("Duplicate line", editor::DuplicateLine),
            item("Move line up", editor::MoveLineUp),
            item("Move line down", editor::MoveLineDown),
            item("Delete line", gpui_kit::base::input::DeleteLine),
            item("Join lines", gpui_kit::base::input::JoinLines),
            item("Transpose", gpui_kit::base::input::Transpose),
            item("Insert line below", gpui_kit::base::input::InsertLineBelow),
            item("Insert line above", gpui_kit::base::input::InsertLineAbove),
            MenuItem::separator(),
            item("Toggle comment", editor::ToggleComments),
            item("Indent", editor::Indent),
            item("Outdent", editor::Outdent),
            item("Re-indent", gpui_kit::base::input::Reindent),
            MenuItem::separator(),
            item("Add cursor above", gpui_kit::base::input::AddCursorAbove),
            item("Add cursor below", gpui_kit::base::input::AddCursorBelow),
            MenuItem::separator(),
            MenuItem::submenu(Menu::new("Surround selection").items([
                item("Parentheses", gpui_kit::base::input::SurroundWithParentheses),
                item("Square brackets", gpui_kit::base::input::SurroundWithBrackets),
                item("Curly braces", gpui_kit::base::input::SurroundWithBraces),
                item("Double quotes", gpui_kit::base::input::SurroundWithDoubleQuotes),
                item("Single quotes", gpui_kit::base::input::SurroundWithSingleQuotes),
                item("Backticks", gpui_kit::base::input::SurroundWithBackticks),
            ])),
            item("Remove surrounding", gpui_kit::base::input::RemoveSurrounding),
            MenuItem::separator(),
            MenuItem::submenu(Menu::new("Transformations").items([
                item("Make upper case", gpui_kit::base::input::MakeUpperCase),
                item("Make lower case", gpui_kit::base::input::MakeLowerCase),
                item("Capitalize", gpui_kit::base::input::Capitalize),
                MenuItem::submenu(Menu::new("Convert case").items([
                    item("Title Case", gpui_kit::base::input::ConvertToTitleCase),
                    item("snake_case", gpui_kit::base::input::ConvertToSnakeCase),
                    item("camelCase", gpui_kit::base::input::ConvertToCamelCase),
                    item("PascalCase", gpui_kit::base::input::ConvertToPascalCase),
                    item("kebab-case", gpui_kit::base::input::ConvertToKebabCase),
                ])),
                item("Sort lines", gpui_kit::base::input::SortLines),
                item("Sort lines descending", gpui_kit::base::input::SortLinesDescending),
                item("Sort lines ignoring case", gpui_kit::base::input::SortLinesIgnoringCase),
                item("Reverse lines", gpui_kit::base::input::ReverseLines),
                item("Remove duplicate lines", gpui_kit::base::input::RemoveDuplicateLines),
                item("Remove blank lines", gpui_kit::base::input::RemoveBlankLines),
                item("Remove trailing spaces", gpui_kit::base::input::TrimTrailingWhitespace),
                item("Hard wrap", gpui_kit::base::input::HardWrap),
                item("Convert indentation to spaces", gpui_kit::base::input::ConvertIndentationToSpaces),
                item("Convert indentation to tabs", gpui_kit::base::input::ConvertIndentationToTabs),
                item("Increment", gpui_kit::base::input::Increment),
                item("Decrement", gpui_kit::base::input::Decrement),
                item("Format JSON", crate::config_check::FormatJson).disabled(!look.json),
            ])),
            MenuItem::submenu(Menu::new("Markdown").items([
                item("Bold", MarkdownBold).disabled(!look.markdown),
                item("Italic", MarkdownItalic).disabled(!look.markdown),
                item("Strikethrough", MarkdownStrike).disabled(!look.markdown),
                item("Inline code", MarkdownCode).disabled(!look.markdown),
                item("Link", MarkdownLink).disabled(!look.markdown),
            ]).disabled(!look.markdown)),
            MenuItem::separator(),
            MenuItem::submenu(Menu::new("Find").items([
                item("Find\u{2026}", editor::Search),
                item("Find and replace\u{2026}", editor::Replace),
                item("Find next", FindNext),
                item("Find previous", FindPrevious),
                item("Find in folder\u{2026}", ShowFindInFolder),
                item("Select all matches", SelectAllSearchMatches).disabled(!look.can_select_all_matches),
                item("Select next match", SelectNextOccurrence).disabled(!look.can_select_next_match),
                item("Skip and select next occurrence", SkipNextOccurrence).disabled(!look.can_skip_next_match),
                MenuItem::separator(),
                item("Replace", ReplaceMatch).disabled(!look.can_replace),
                item("Replace and find", ReplaceMatchAndFind).disabled(!look.can_replace),
                item("Replace all", ReplaceAllMatches).disabled(!look.can_replace),
                MenuItem::separator(),
                item("Match case", ToggleMatchCase).checked(look.match_case),
                item("Whole word", ToggleWholeWord).checked(look.whole_word),
                item("Regular expression", ToggleRegex).checked(look.regex),
                item("Ignore diacritics", ToggleIgnoreDiacritics).checked(look.ignore_diacritics),
                item("Find in selection", ToggleFindInSelection).checked(look.in_selection).disabled(!look.can_scope),
                MenuItem::separator(),
                item("Use selection for find", UseSelectionForFind),
                item("Use selection for replace", UseSelectionForReplace),
                item("Jump to selection", JumpToSelection),
            ])),
            item("Jump to matching bracket", gpui_kit::base::input::MoveToEnclosingBracket),
            item("Go to line\u{2026}", ShowGoToLine),
            item("Go to symbol\u{2026}", ShowGoToSymbol),
            MenuItem::separator(),
            // Reads the selection, or from the caret on, aloud with each word lit (`speech.rs`).
            MenuItem::submenu(Menu::new("Speech").items([
                item("Start speaking", crate::speech::StartSpeaking).disabled(look.endings.is_none()),
                item("Stop speaking", crate::speech::StopSpeaking).disabled(!crate::speech::speaking()),
            ])),
            MenuItem::separator(),
            // AppKit titles it and fills in Take Photo and Scan Documents while the editor can take an image.
            MenuItem::os_submenu("Import from iPhone or iPad", SystemMenuType::ImportFromDevice),
        ]),
        // Format > Writing goal…: a word count to reach in the open file, kept for the session (`writing_goal.rs`); grey with no tab.
        Menu::new("Format").items([line_endings(look.endings), MenuItem::separator(), item("Writing goal\u{2026}", SetWritingGoal).disabled(look.endings.is_none())]),
        Menu::new("View").items([
            item(show_or_hide(look.explorer_visible, "explorer"), ToggleExplorer),
            item(show_or_hide(look.preview_visible, "preview"), TogglePreview),
            MenuItem::separator(),
            // A pane needs a second tab to give away.
            item("Split right", SplitRight).disabled(look.tab.tabs < 2),
            item("Split down", SplitDown).disabled(look.tab.tabs < 2),
            MenuItem::separator(),
            item("Soft wrap", ToggleSoftWrap).checked(look.soft_wrap),
            item("Wrap at window width", CycleWrapWidth).checked(look.wrap_full),
            item("Show invisibles", ToggleInvisibles).checked(look.show_invisibles),
            item("Show line numbers", ToggleLineNumbers).checked(look.line_numbers),
            item("Reading width", ToggleReadingWidth).checked(look.reading_width),
            item("Smart punctuation", ToggleSmartPunctuation).checked(look.smart_punctuation),
            item("Typewriter scrolling", ToggleTypewriter).checked(look.typewriter),
            item("Styled Markdown", ToggleStyledMarkdown).checked(look.styled_markdown),
            item("Check spelling", crate::spell::ToggleSpelling).checked(look.check_spelling),
            focus_menu(look.focus, look.has_tab),
            highlight_menu(look.highlight),
            item("Style Check", ToggleStyleCheck).checked(look.style_check),
            MenuItem::separator(),
            item("Fold", FoldAtCaret),
            item("Unfold", UnfoldAtCaret),
            item("Fold all", FoldAll),
            item("Unfold all", UnfoldAll),
            MenuItem::separator(),
            item("Next change", NextChange).disabled(!look.has_tab),
            item("Previous change", PreviousChange).disabled(!look.has_tab),
            MenuItem::separator(),
            item("Bigger", BiggerText).disabled(look.font_size >= settings::FONT_MOST),
            item("Smaller", SmallerText).disabled(look.font_size <= settings::FONT_LEAST),
            item("Actual size", ActualTextSize),
            MenuItem::submenu(Menu::new("Text style").items([settings::Typeface::Mono, settings::Typeface::Serif, settings::Typeface::Sans].map(|style| item(style.title(), SetTextStyle { style }).checked(look.text_style == style)))),
            MenuItem::submenu(Menu::new("Code colors").items(
                settings::Scheme::ALL
                    .map(|scheme| item(scheme.title(), SetCodeScheme { scheme }).checked(look.code_colors.scheme == scheme))
                    .into_iter()
                    .chain([
                        MenuItem::separator(),
                        item("Vivid", ToggleVivid).checked(look.code_colors.vivid),
                        item("Colored roles", ToggleColoredRoles).checked(look.code_colors.colored_roles),
                        item("Rainbow brackets", ToggleRainbowBrackets).checked(look.code_colors.rainbow_brackets),
                        item("Semantic colors", ToggleSemanticColors).checked(look.code_colors.semantic),
                    ]),
            )),
            MenuItem::separator(),
            item("Enclosing folder", EnclosingFolder),
            item("Reveal current file", RevealCurrentFile),
            item("Show hidden files", ToggleHiddenFiles).checked(look.show_hidden),
            item("Show ignored files", ToggleIgnoredFiles).checked(look.show_ignored),
            MenuItem::submenu(Menu::new("Sort by").items(settings::ExplorerSort::ALL.map(|sort| item(sort.title(), SortExplorer { sort }).checked(look.explorer_sort == sort)))),
            item("Collapse all folders", CollapseAllFolders),
            item("Focus explorer", FocusExplorer),
            item("Filter explorer", FocusFilter),
            MenuItem::separator(),
            item(if look.full_screen { "Exit full screen" } else { "Enter full screen" }, ToggleFullScreen),
            item("Minimal mode", ToggleMinimal).checked(look.minimal),
        ]),
        Menu::new("Window").items([
            item("Minimize", Minimize),
            item("Zoom", Zoom),
            MenuItem::separator(),
            item("Show previous tab", PrevTab).disabled(look.tab.tabs < 2),
            item("Show next tab", NextTab).disabled(look.tab.tabs < 2),
            item("Show first tab", ShowTab1).disabled(look.tab.tabs < 2),
            item("Show last tab", ShowTab9).disabled(look.tab.tabs < 2),
            MenuItem::separator(),
            item(if look.tab.pinned { "Unpin tab" } else { "Pin tab" }, TogglePinTab).disabled(look.tab.tabs == 0),
            item("Move tab earlier", MoveTabEarlier).disabled(!look.tab.can_earlier),
            item("Move tab later", MoveTabLater).disabled(!look.tab.can_later),
            MenuItem::separator(),
            item("Move tab to left pane", MoveTabLeft).disabled(!look.tab.panes[0]),
            item("Move tab to right pane", MoveTabRight).disabled(!look.tab.panes[1]),
            item("Move tab to upper pane", MoveTabUp).disabled(!look.tab.panes[2]),
            item("Move tab to lower pane", MoveTabDown).disabled(!look.tab.panes[3]),
            item("Move tab to new window", MoveTabToNewWindow).disabled(look.tab.window_tabs < 2),
            MenuItem::separator(),
            item("Focus pane left", FocusPaneLeft).disabled(!look.tab.panes[0]),
            item("Focus pane right", FocusPaneRight).disabled(!look.tab.panes[1]),
            item("Focus pane up", FocusPaneUp).disabled(!look.tab.panes[2]),
            item("Focus pane down", FocusPaneDown).disabled(!look.tab.panes[3]),
            item("Focus next pane", FocusNextPane).disabled(look.tab.pane_count < 2),
            item("Focus previous pane", FocusPreviousPane).disabled(look.tab.pane_count < 2),
            MenuItem::separator(),
            item("Even out panes", EvenOutPanes).disabled(look.tab.pane_count < 2),
            item("Merge panes", MergePanes).disabled(look.tab.pane_count < 2),
            item("Merge all windows", MergeAllWindows).disabled(look.tab.windows < 2),
            MenuItem::separator(),
            item("Bring all to front", BringAllToFront),
        ]),
        Menu::new("Help").items([item(format!("{APP} Help"), ShowHelp)]),
    ]
}

/// The bar's titles, checks and disabled states in order.
fn shown(menus: &[Menu], recents: Vec<PathBuf>) -> Shown {
    fn collect(list: &[MenuItem], out: &mut Vec<(String, bool, bool)>) {
        for item in list {
            match item {
                MenuItem::Separator => {}
                MenuItem::Action { name, checked, disabled, .. } => out.push((name.to_string(), *checked, *disabled)),
                MenuItem::SystemMenu(menu) => out.push((menu.name.to_string(), false, false)),
                MenuItem::Submenu(menu) => {
                    out.push((menu.name.to_string(), false, menu.disabled));
                    collect(&menu.items, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    for menu in menus {
        out.push((menu.name.to_string(), false, menu.disabled));
        collect(&menu.items, &mut out);
    }
    Shown(out, recents)
}

/// Sets the bar. Call once the key bindings are bound (the items show their
/// keys) and before the first window opens; the bar follows the key window.
pub fn install(cx: &mut App) {
    cx.on_action(|_: &FocusFilter, cx| focus_filter(cx));
    // Set once: the Dock menu is static and owns its actions separately from the bar.
    cx.set_dock_menu(vec![MenuItem::action("New window", NewWindow)]);
    cx.set_global(Shown::default());
    cx.set_global(Built::default());
    cx.set_global(Recents { paths: Vec::new(), check: None });
    reload_recents(cx);
    templates::install(cx);
    crate::camera_text::install(cx);
    crate::focus::install(cx);
    crate::parts_of_speech::install(cx);
    crate::spell::install(cx);
    refresh(cx);
    cx.observe_new(|state: &mut editor::EditorState, _, cx| {
        // What `look` reads of the selection: whether Select next match and Select all matches have
        // a selection or caret word to work from. A caret move rarely changes either.
        let reads = |state: &editor::EditorState| (state.can_select_all_search_matches(), state.can_select_next_occurrence(), state.can_skip_next_occurrence());
        let mut selection = reads(state);
        let mut scan = state.search_session().matcher.matched_ranges();
        let mut flags = (state.text_revision(), state.search_session().is_active(), state.is_editable());
        cx.observe_self(move |state, cx| {
            let next_scan = state.search_session().matcher.matched_ranges();
            let next_flags = (state.text_revision(), state.search_session().is_active(), state.is_editable());
            let next_selection = reads(state);
            if flags == next_flags && std::sync::Arc::ptr_eq(&scan, &next_scan) && selection == next_selection {
                return;
            }
            flags = next_flags;
            scan = next_scan;
            selection = next_selection;
            cx.defer(refresh);
        }).detach();
        cx.defer(refresh);
    }).detach();
    cx.observe_new(|_: &mut Workspace, window, cx| {
        let Some(window) = window else { return };
        cx.observe_window_activation(window, |_, window, cx| {
            if window.is_window_active() {
                // Also where a recent deleted since shows up gone, and a template added in Finder
                // shows up: neither list is read per refresh.
                reload_recents(cx);
                templates::reload(cx);
                cx.defer(refresh);
            }
        })
        .detach();
        let mut full_screen = window.is_fullscreen();
        cx.observe_window_bounds(window, move |_, window, cx| {
            let current = window.is_fullscreen();
            if current != full_screen {
                full_screen = current;
                cx.defer(refresh);
            }
        })
        .detach();
        // The first window's wrap decides the check before any window is key.
        cx.defer(refresh);
    })
    .detach();
}

/// Cmd-Option-F: shows the explorer when it is hidden and puts the keyboard in
/// its filter field, in the key window.
fn focus_filter(cx: &mut App) {
    let key = cx.active_window();
    let Some((handle, workspace)) = workspace::open_workspaces(cx).into_iter().find(|(handle, _)| Some(*handle) == key) else {
        return;
    };
    // The scratch window has no folder tree.
    if workspace.read(cx).is_scratch() {
        return;
    }
    // Deferred: the action may run while a window is being updated.
    cx.defer(move |cx| {
        let explorer = workspace.read(cx).explorer().clone();
        let hidden = !settings::get(cx).explorer.visible;
        if hidden {
            settings::update(cx, |settings| settings.explorer.visible = true);
        }
        handle
            .update(cx, move |_, window, cx| {
                if hidden {
                    // The panel shows on the next frame.
                    window.defer(cx, move |window, cx| explorer.update(cx, |explorer, cx| explorer.focus_filter(window, cx)));
                } else {
                    explorer.update(cx, |explorer, cx| explorer.focus_filter(window, cx));
                }
            })
            .ok();
    });
}

/// Sets the bar again when an item's title, check mark or enabled state
/// changed since it was last set; otherwise does nothing.
pub fn refresh(cx: &mut App) {
    if !cx.has_global::<Shown>() {
        return;
    }
    let look = look(cx);
    if cx.global::<Built>().0.as_ref() == Some(&look) {
        return;
    }
    let menus = bar(&look);
    let now = shown(&menus, look.recents.clone());
    cx.set_global(Built(Some(look)));
    if *cx.global::<Shown>() == now {
        return;
    }
    cx.set_menus(menus);
    native::fix_up();
    if std::env::var_os("GPUI_EDITOR_DUMP_MENUS").is_some() {
        native::dump();
    }
    cx.set_global(now);
}

/// Sets the bar again whether or not it changed: after a layout change moved the keys, the items
/// show the new ones. The platform keeps at most the current and previous bar's actions.
pub fn rebuild(cx: &mut App) {
    cx.set_global(Shown::default());
    cx.set_global(Built::default());
    refresh(cx);
}

/// The standard About panel (name, version and icon from the app bundle).
pub fn show_about() {
    native::show_about();
}

/// Print AppKit's current menu tree without rebuilding it.
pub fn dump_native() {
    native::dump();
}

/// What GPUI's `set_menus` can't say, set on the live `NSMenu`s after it, and
/// the dump that reads them back.
mod native {
    use super::*;

    const COMMAND: usize = 1 << 20;
    const SHIFT: usize = 1 << 17;
    const OPTION: usize = 1 << 19;
    const CONTROL: usize = 1 << 18;

    fn string(text: &str) -> Retained<AnyObject> {
        let text = CString::new(text).unwrap_or_default();
        unsafe { msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()] }
    }

    fn text(string: &AnyObject) -> String {
        let chars: *const c_char = unsafe { msg_send![string, UTF8String] };
        if chars.is_null() {
            return String::new();
        }
        unsafe { CStr::from_ptr(chars) }.to_string_lossy().into_owned()
    }

    fn app() -> Retained<AnyObject> {
        unsafe { msg_send![class!(NSApplication), sharedApplication] }
    }

    fn main_menu() -> Option<Retained<AnyObject>> {
        unsafe { msg_send![&*app(), mainMenu] }
    }

    fn items(menu: &AnyObject) -> Vec<Retained<AnyObject>> {
        let count: isize = unsafe { msg_send![menu, numberOfItems] };
        (0..count).map(|index| unsafe { msg_send![menu, itemAtIndex: index] }).collect()
    }

    fn submenu(item: &AnyObject) -> Option<Retained<AnyObject>> {
        unsafe { msg_send![item, submenu] }
    }

    fn title(object: &AnyObject) -> String {
        let title: Retained<AnyObject> = unsafe { msg_send![object, title] };
        text(&title)
    }

    /// Whether the general pasteboard holds files (the type a Finder copy and the tree's Copy write), for Edit > Move item here.
    /// Reads only the list of types, and only when the pasteboard's change count moved since the last call; never the data.
    pub fn pasteboard_has_files() -> bool {
        use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
        static COUNT: AtomicIsize = AtomicIsize::new(-1);
        static HAS: AtomicBool = AtomicBool::new(false);
        unsafe {
            let pasteboard: Retained<AnyObject> = msg_send![class!(NSPasteboard), generalPasteboard];
            let count: isize = msg_send![&*pasteboard, changeCount];
            if COUNT.swap(count, Ordering::Relaxed) != count {
                let types: Option<Retained<AnyObject>> = msg_send![&*pasteboard, types];
                let has = types.is_some_and(|types| {
                    let has: Bool = msg_send![&*types, containsObject: &*string("NSFilenamesPboardType")];
                    has.as_bool()
                });
                HAS.store(has, Ordering::Relaxed);
            }
        }
        HAS.load(Ordering::Relaxed)
    }

    pub fn show_about() {
        let none: *mut AnyObject = std::ptr::null_mut();
        let _: () = unsafe { msg_send![&*app(), orderFrontStandardAboutPanel: none] };
    }

    /// Makes the Help menu the system's, so it gets the Search field, and gives
    /// Tab and Return key equivalents the characters AppKit draws as glyphs:
    /// GPUI's `key_to_native` has no Tab or Return, so they arrive as the words
    /// "tab" and "enter" and AppKit draws "T" and "E".
    pub fn fix_up() {
        let Some(main) = main_menu() else { return };
        for top in items(&main) {
            let Some(menu) = submenu(&top) else { continue };
            if title(&menu) == "Help" {
                let _: () = unsafe { msg_send![&*app(), setHelpMenu: &*menu] };
            }
        }
        fix_keys(&main);
        mark_alternates(&main);
        add_symbols(&main);
    }

    fn fix_keys(menu: &AnyObject) {
        for item in items(menu) {
            if let Some(sub) = submenu(&item) {
                fix_keys(&sub);
            }
            let key: Retained<AnyObject> = unsafe { msg_send![&*item, keyEquivalent] };
            let character = match text(&key).as_str() {
                "tab" => "\t",
                "enter" => "\r",
                _ => continue,
            };
            let _: () = unsafe { msg_send![&*item, setKeyEquivalent: &*string(character)] };
        }
    }

    /// Edit > Move item here is Paste's Option alternate, as in Finder: GPUI's `MenuItem` can't say so.
    fn mark_alternates(menu: &AnyObject) {
        for item in items(menu) {
            if let Some(sub) = submenu(&item) {
                mark_alternates(&sub);
            }
            if title(&item) == "Move item here" {
                let _: () = unsafe { msg_send![&*item, setAlternate: Bool::YES] };
            }
        }
    }

    /// The symbol of a row of the bar, the right-click menus' for the same action (Cut, Copy, Paste,
    /// Show in Finder, the tab menu's items) and otherwise the one macOS's own apps show. Rows with
    /// no clear symbol have none. Looked up by title: GPUI's `MenuItem` can't carry an image.
    fn symbol_of(title: &str) -> Option<&'static str> {
        Some(match title {
            "Settings\u{2026}" => "gearshape",
            "New tab" => "doc.badge.plus",
            "New window" => "macwindow.badge.plus",
            "Open\u{2026}" => "folder",
            "Open quickly\u{2026}" => "doc.text.magnifyingglass",
            "Save" => "square.and.arrow.down",
            "Print\u{2026}" => "printer",
            "Revert to saved" => "arrow.counterclockwise",
            "Browse versions\u{2026}" => "clock.arrow.circlepath",
            "Rename\u{2026}" => "pencil",
            "Duplicate" | "Duplicate line" => "plus.square.on.square",
            "Show in Finder" => "finder",
            "Copy path" => "document.on.document",
            "Close tab" => "xmark",
            "Close other tabs" => "xmark.square",
            "Close tabs to the right" => "arrow.right.to.line",
            "Close tabs to the left" => "arrow.left.to.line",
            "Undo" => "arrow.uturn.backward",
            "Redo" => "arrow.uturn.forward",
            "Undo history\u{2026}" => "clock.arrow.circlepath",
            "Cut" => "scissors",
            "Copy" => "doc.on.doc",
            "Paste" | "Move item here" => "doc.on.clipboard",
            "Select all" => "selection.pin.in.out",
            "Toggle comment" => "text.quote",
            "Indent" => "increase.indent",
            "Outdent" => "decrease.indent",
            "Surround selection" => "viewfinder",
            "Parentheses" => "parentheses",
            "Square brackets" => "square.dashed",
            "Curly braces" => "curlybraces",
            "Double quotes" => "quote.opening",
            "Single quotes" => "quote.closing",
            "Backticks" => "chevron.left.forwardslash.chevron.right",
            "Remove surrounding" => "minus.square",
            "Transformations" => "wand.and.stars",
            "Make upper case" => "textformat.size.larger",
            "Make lower case" => "textformat.size.smaller",
            "Capitalize" => "textformat",
            "Convert case" => "textformat.abc",
            "Title Case" => "textformat.characters",
            "snake_case" => "underline",
            "camelCase" => "textformat.alt",
            "PascalCase" => "textformat.size",
            "kebab-case" => "minus",
            "Sort lines" | "Sort by" => "arrow.up.arrow.down",
            "Sort lines descending" => "arrow.down",
            "Sort lines ignoring case" => "textformat.abc.dottedunderline",
            "Reverse lines" => "arrow.up.and.down.text.horizontal",
            "Remove duplicate lines" => "rectangle.on.rectangle.slash",
            "Remove blank lines" => "text.badge.minus",
            "Remove trailing spaces" => "text.badge.xmark",
            "Hard wrap" => "arrow.turn.down.left",
            "Convert indentation to spaces" => "space",
            "Convert indentation to tabs" => "arrow.right.to.line.compact",
            "Increment" => "plus",
            "Decrement" => "minus",
            "Format JSON" => "curlybraces.square",
            "Find" | "Find\u{2026}" | "Use selection for find" => "magnifyingglass",
            "Hide explorer" | "Show explorer" => "sidebar.left",
            "Hide preview" | "Show preview" => "sidebar.right",
            "Split right" => "rectangle.split.2x1",
            "Split down" => "rectangle.split.1x2",
            "Enclosing folder" => "arrow.up",
            "Reveal current file" => "scope",
            "Show hidden files" => "eye.slash",
            "Show ignored files" => "circle.dashed",
            "Focus explorer" => "sidebar.squares.left",
            "Collapse all folders" => "arrow.down.forward.and.arrow.up.backward",
            "Enter full screen" => "arrow.up.left.and.arrow.down.right",
            "Exit full screen" => "arrow.down.right.and.arrow.up.left",
            "Pin tab" => "pin",
            "Unpin tab" => "pin.slash",
            "Move tab to left pane" => "arrow.left.square",
            "Move tab to right pane" => "arrow.right.square",
            "Move tab to upper pane" => "arrow.up.square",
            "Move tab to lower pane" => "arrow.down.square",
            "Move tab to new window" => "arrow.up.forward.app",
            _ => return None,
        })
    }

    /// Sets `symbol_of`'s symbol on every row of the bar that has one, in the submenus too.
    fn add_symbols(menu: &AnyObject) {
        for item in items(menu) {
            if let Some(sub) = submenu(&item) {
                add_symbols(&sub);
            }
            if let Some(name) = symbol_of(&title(&item)) {
                crate::native_menu::set_symbol(&item, name);
            }
        }
    }

    /// Prints the live bar to stdout: `path<TAB>key<TAB>state<TAB>symbol` per item, the
    /// state being `enabled` or `disabled`, plus `checked` when it has a check, and the
    /// symbol the name of the SF Symbol on a row that has an image.
    pub fn dump() {
        let Some(main) = main_menu() else { return };
        println!("== menu bar");
        walk(&main, "");
    }

    fn walk(menu: &AnyObject, path: &str) {
        for item in items(menu) {
            let separator: Bool = unsafe { msg_send![&*item, isSeparatorItem] };
            if separator.as_bool() {
                continue;
            }
            let name = title(&item);
            let path = if path.is_empty() { name.clone() } else { format!("{path} > {name}") };
            let key: Retained<AnyObject> = unsafe { msg_send![&*item, keyEquivalent] };
            let modifiers: usize = unsafe { msg_send![&*item, keyEquivalentModifierMask] };
            let enabled: Bool = unsafe { msg_send![&*item, isEnabled] };
            let state: isize = unsafe { msg_send![&*item, state] };
            let key = text(&key);
            let keys = if key.is_empty() {
                String::new()
            } else {
                let flags: String = [(CONTROL, '\u{2303}'), (OPTION, '\u{2325}'), (SHIFT, '\u{21E7}'), (COMMAND, '\u{2318}')]
                    .iter()
                    .filter(|(bit, _)| modifiers & bit != 0)
                    .map(|(_, glyph)| *glyph)
                    .collect();
                let key = match key.as_str() {
                    "\r" => "\u{21A9}".to_owned(),
                    "\t" => "\u{21E5}".to_owned(),
                    other => other.to_uppercase(),
                };
                format!("{flags}{key}")
            };
            let state = format!("{}{}", if enabled.as_bool() { "enabled" } else { "disabled" }, if state == 1 { ",checked" } else { "" });
            let image: Option<Retained<AnyObject>> = unsafe { msg_send![&*item, image] };
            let symbol = image.and_then(|_| symbol_of(&name)).unwrap_or_default();
            println!("{path}\t{keys}\t{state}\t{symbol}");
            if let Some(sub) = submenu(&item) {
                walk(&sub, &path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Look, MenuItem, bar, recent_title};

    fn count(items: &[MenuItem]) -> usize {
        items.iter().map(|item| 1 + if let MenuItem::Submenu(menu) = item { count(&menu.items) } else { 0 }).sum()
    }

    /// A tab switch that changes nothing the bar reads leaves the look equal, so the refresh builds no
    /// items; one that changes an enabled state (a second tab) or the file's encoding differs, so it builds.
    #[test]
    fn an_unchanged_look_skips_the_item_tree_and_a_changed_one_builds_it() {
        let per_build: usize = bar(&Look::default()).iter().map(|menu| 1 + count(&menu.items)).sum();
        assert!(per_build > 150, "{per_build}");
        let mut built = 0;
        let mut last: Option<Look> = None;
        for _ in 0..20 {
            let look = Look::default();
            if last.as_ref() != Some(&look) {
                built += per_build;
                last = Some(look);
            }
        }
        assert_eq!(built, per_build, "20 identical switches build the bar once");
        let mut two_tabs = Look::default();
        two_tabs.tab.0.tabs = 2;
        assert!(two_tabs != Look::default());
        let mut utf8 = Look::default();
        utf8.encoding = Some(encoding_rs::UTF_8);
        assert!(utf8 != Look::default());
    }

    #[test]
    fn same_named_recents_show_their_folder_and_others_stay_plain() {
        let recents: Vec<PathBuf> = ["/w/notes/README.md", "/w/other/README.md", "/w/notes/todo.txt"].iter().map(PathBuf::from).collect();
        assert_eq!(recent_title(&recents[0], &recents), "README.md (notes)");
        assert_eq!(recent_title(&recents[1], &recents), "README.md (other)");
        assert_eq!(recent_title(&recents[2], &recents), "todo.txt");
    }
}
