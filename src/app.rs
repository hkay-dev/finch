//! The application: actions, key bindings and the window.

use std::{
    cell::{Cell, RefCell},
    ffi::OsString,
    future::Future,
    io,
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};

use gpui_kit::*;

use gpui_kit::component::input::FindReplace;
use gpui_kit::base::input::MarkdownFormat;
use crate::{
    control,
    document::{Document, title},
    drafts,
    find_float::Step,
    explorer::{CollapseAllFolders, EnclosingFolder, ExplorerEvent, FocusExplorer, FocusFilter, RevealCurrentFile, ToggleHiddenFiles},
    encoding::{self, Decoded},
    menu, recent, settings,
    start::{self, Caret, Start},
    theme,
    tokens::{EDITOR_MIN, PANEL_WIDTH_MIN, WINDOW_MIN_HEIGHT},
    window_look,
    workspace::{self, CopyFilePath, MoveItemHere, ShowDraftList, ShowFileInFinder, UseSelectionForFind, UseSelectionForReplace, Workspace},
};

actions!(
    gpui_editor,
    [
        Save, SaveAs, SaveAll, RevertToSaved, Print, Quit, ToggleExplorer, TogglePreview, ToggleSoftWrap, CycleWrapWidth, NewTab, CloseTab, ReopenClosedTab, PrevTab, NextTab, PrevRecentTab, NextRecentTab,
        ShowTab1, ShowTab2, ShowTab3, ShowTab4,
        ShowTab5, ShowTab6, ShowTab7, ShowTab8, ShowTab9, Open, CloseWindow, OpenSettings, OpenSettingsFile, About, HideApp, HideOthers, ShowAll, ToggleFullScreen, Minimize, Zoom, BringAllToFront, ShowHelp,
        SplitRight, SplitDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp, FocusPaneDown, NewWindow, ClearRecent, FindNext, FindPrevious, JumpToSelection,
        FocusNextPane, FocusPreviousPane, EvenOutPanes, MergePanes, MergeAllWindows,
        SelectAllSearchMatches, SelectNextOccurrence, SkipNextOccurrence, ToggleFindInSelection,
        ToggleIgnoreDiacritics,
        ToggleMatchCase, ToggleWholeWord, ToggleRegex, ReplaceMatch, ReplaceMatchAndFind, ReplaceAllMatches, ShowGoToLine, ShowGoToSymbol, ShowOpenQuickly, ShowFindInFolder, ToggleInvisibles, CloseOtherTabs, CloseTabsToTheRight, CloseTabsToTheLeft, TogglePinTab, MoveTabEarlier, MoveTabLater, MoveTabLeft, MoveTabRight, MoveTabUp, MoveTabDown, MoveTabToNewWindow, LookUp,
        MarkdownBold, MarkdownItalic, MarkdownStrike, MarkdownCode, MarkdownLink,
        FoldAtCaret, UnfoldAtCaret, FoldAll, UnfoldAll,
        NextChange, PreviousChange,
        BiggerText, SmallerText, ActualTextSize, ToggleLineNumbers, ToggleReadingWidth, ToggleSmartPunctuation, ToggleTypewriter, ToggleStyledMarkdown, SetWritingGoal, ToggleMinimal, ToggleStyleCheck,
        ToggleVivid, ToggleColoredRoles, ToggleRainbowBrackets, ToggleSemanticColors, ToggleScratch
    ]
);

/// File > Open Recent's item for one path: opens it the way File > Open does.
#[derive(Action, Clone, PartialEq)]
#[action(namespace = gpui_editor, no_json)]
pub struct OpenRecent {
    pub path: PathBuf,
}

/// File > Reopen with encoding's item for one encoding: rereads the key window's file decoded that way.
#[derive(Action, Clone, PartialEq)]
#[action(namespace = gpui_editor, no_json)]
pub struct ReopenWithEncoding {
    pub encoding: &'static encoding_rs::Encoding,
}

/// File > Convert to's item for one encoding: the key window's page saves in it from now on, text untouched.
#[derive(Action, Clone, PartialEq)]
#[action(namespace = gpui_editor, no_json)]
pub struct ConvertToEncoding {
    pub encoding: &'static encoding_rs::Encoding,
}

/// Format > Line endings' item for one style: rewrites every break of the key window's file to CRLF (`crlf`) or LF.
#[derive(Action, Clone, PartialEq)]
#[action(namespace = gpui_editor, no_json)]
pub struct SetLineEndings {
    pub crlf: bool,
}

/// View > Text style's item for one style: sets how the key window's kind of file (prose or code) draws.
#[derive(Action, Clone, PartialEq)]
#[action(namespace = gpui_editor, no_json)]
pub struct SetTextStyle {
    pub style: settings::Typeface,
}

/// View > Code Colors' item for one scheme: the editor's code colors take that scheme.
#[derive(Action, Clone, PartialEq)]
#[action(namespace = gpui_editor, no_json)]
pub struct SetCodeScheme {
    pub scheme: settings::Scheme,
}

/// URLs the system asked us to open, waiting for the app to pick them up. The
/// platform callback has no `App`, so it parks them here and wakes the task.
#[derive(Default)]
struct Inbox {
    urls: Vec<String>,
    waker: Option<Waker>,
}

/// Resolves with the next batch of parked URLs.
struct NextUrls(Rc<RefCell<Inbox>>);

impl Future for NextUrls {
    type Output = Vec<String>;

    fn poll(self: Pin<&mut Self>, task: &mut Context<'_>) -> Poll<Vec<String>> {
        let mut inbox = self.0.borrow_mut();
        if inbox.urls.is_empty() {
            inbox.waker = Some(task.waker().clone());
            Poll::Pending
        } else {
            Poll::Ready(std::mem::take(&mut inbox.urls))
        }
    }
}

/// How far each further window sits right of and below the one it opens from: AppKit's own
/// cascade step.
const CASCADE: f32 = 22.;

/// How far up and left of the pointer a window opened by a dropped tab puts its top-left corner, so the tab bar's first tab sits about where the card was let go.
const DROP_GRAB: (f32, f32) = (150., 18.);

/// Runs the app. The window opens on `start` (its file already read into
/// `text`), unless `defer` is set (launched from the Dock or Finder) and the
/// system handed over a document to open: macOS sends those before it finishes
/// launching, so they're already parked in the inbox when the run closure ends.
/// Every document opened later joins a window as `open_outside` says.
/// With `background` windows open without taking focus or activating the app,
/// for tests.
pub fn run(start: Start, text: (Option<Decoded>, Option<std::time::SystemTime>), store: settings::Store, background: bool, defer: bool) {
    let inbox = Rc::new(RefCell::new(Inbox::default()));
    let app = gpui_kit::application().with_assets(crate::symbols::Assets);
    app.on_open_urls({
        let inbox = inbox.clone();
        move |urls| {
            let mut inbox = inbox.borrow_mut();
            inbox.urls.extend(urls);
            if let Some(waker) = inbox.waker.take() {
                waker.wake();
            }
        }
    });
    app.on_should_quit(move |cx| {
        if workspace::quit_needs_question(cx) {
            // A hidden app must reveal its save question before logout waits for it.
            if !background {
                cx.activate(true);
            }
            workspace::quit(cx);
            false
        } else {
            true
        }
    });
    // With the resident setting on the app can be running with no window (the shortcut still works):
    // the Dock icon then brings one back.
    app.on_reopen(move |cx| {
        if workspace::open_workspaces(cx).iter().all(|(_, workspace)| workspace.read(cx).is_scratch()) {
            new_window(background, cx);
        }
    });
    app.run(move |cx| {
        if background {
            // GPUI makes a windowed app Regular once launching ends: a Dock icon, Cmd-Tab and a
            // menu bar for an agent's build. A background launch stays Accessory (gpui-editor#a6gn).
            cx.set_activation_policy(ActivationPolicy::Accessory);
        }
        gpui_kit::init(cx);
        crate::explorer::icons::init(cx);
        cx.set_global(store);
        settings::watch(cx);
        {
            theme::init(cx);
            tune_text(cx);
        }
        // GPUI Kit's own bindings (its inits bound them above), kept so a layout change can put them back after the clear.
        let kit: Vec<KeyBinding> = cx.key_bindings().borrow().bindings().cloned().collect();
        bind_all(cx);
        let table = Rc::new(RefCell::new(cx.keyboard_mapper().get_key_equivalents().cloned()));
        cx.on_keyboard_layout_change(move |cx| {
            let now = cx.keyboard_mapper().get_key_equivalents().cloned();
            if *table.borrow() == now {
                return;
            }
            *table.borrow_mut() = now;
            cx.clear_key_bindings();
            cx.bind_keys(kit.iter().cloned());
            bind_all(cx);
            menu::rebuild(cx);
        })
        .detach();
        cx.on_action(|_: &Quit, cx| workspace::quit(cx));
        cx.on_action(move |_: &Open, cx| open_picker(background, cx));
        // Deferred: the action runs while its window is being updated, and the open reads windows.
        cx.on_action(move |_: &OpenSettings, cx| cx.defer(move |cx| crate::settings_window::open(None, background, cx)));
        // The settings file as a tab, for the button in the Settings window and the alert's "Show settings file".
        cx.on_action(move |_: &OpenSettingsFile, cx| cx.defer(move |cx| open_settings_file(background, cx)));
        // Deferred: the action runs while its window is being updated, and the cascade reads that window's frame.
        cx.on_action(move |_: &NewWindow, cx| cx.defer(move |cx| new_window(background, cx)));
        cx.on_action(|_: &About, _| menu::show_about());
        // Deferred: the action runs while its window is being updated, and the open reads windows.
        cx.on_action(move |_: &ShowHelp, cx| cx.defer(move |cx| show_help(background, cx)));
        // Deferred: the action runs while its window is being updated, and the step reads windows.
        cx.on_action(|_: &FindNext, cx| cx.defer(|cx| step_find(true, cx)));
        cx.on_action(|_: &FindPrevious, cx| cx.defer(|cx| step_find(false, cx)));
        cx.on_action(|_: &SelectAllSearchMatches, cx| cx.defer(|cx| select_occurrences(Occurrences::All, cx)));
        cx.on_action(|_: &SelectNextOccurrence, cx| cx.defer(|cx| select_occurrences(Occurrences::Next, cx)));
        cx.on_action(|_: &SkipNextOccurrence, cx| cx.defer(|cx| select_occurrences(Occurrences::Skip, cx)));
        cx.on_action(|_: &ToggleMatchCase, cx| cx.defer(|cx| toggle_find_option(true, cx)));
        cx.on_action(|_: &ToggleWholeWord, cx| cx.defer(|cx| toggle_find_option(false, cx)));
        cx.on_action(|_: &ToggleRegex, cx| cx.defer(toggle_find_regex));
        cx.on_action(|_: &ToggleIgnoreDiacritics, cx| cx.defer(toggle_find_diacritics));
        cx.on_action(|_: &ToggleFindInSelection, cx| cx.defer(toggle_find_in_selection));
        cx.on_action(|_: &ReplaceMatch, cx| cx.defer(|cx| replace_matches(FindReplace::Replace, cx)));
        cx.on_action(|_: &ReplaceMatchAndFind, cx| cx.defer(|cx| replace_matches(FindReplace::ReplaceAndFind, cx)));
        cx.on_action(|_: &ReplaceAllMatches, cx| cx.defer(|cx| replace_matches(FindReplace::ReplaceAll, cx)));
        cx.on_action(|_: &JumpToSelection, cx| cx.defer(jump_to_selection));
        cx.on_action(|_: &FoldAtCaret, cx| cx.defer(|cx| fold_command(Fold::AtCaret, cx)));
        cx.on_action(|_: &UnfoldAtCaret, cx| cx.defer(|cx| fold_command(Fold::UnfoldAtCaret, cx)));
        cx.on_action(|_: &FoldAll, cx| cx.defer(|cx| fold_command(Fold::All, cx)));
        cx.on_action(|_: &UnfoldAll, cx| cx.defer(|cx| fold_command(Fold::UnfoldAll, cx)));
        cx.on_action(|_: &ToggleInvisibles, cx| cx.defer(toggle_invisibles));
        cx.on_action(|_: &ToggleLineNumbers, cx| cx.defer(toggle_line_numbers));
        cx.on_action(|_: &ToggleReadingWidth, cx| cx.defer(toggle_reading_width));
        cx.on_action(|_: &ToggleSmartPunctuation, cx| cx.defer(toggle_smart_punctuation));
        cx.on_action(|_: &ToggleTypewriter, cx| cx.defer(toggle_typewriter));
        cx.on_action(|_: &ToggleStyledMarkdown, cx| cx.defer(toggle_styled_markdown));
        cx.on_action(|action: &SetCodeScheme, cx| {
            let scheme = action.scheme;
            cx.defer(move |cx| change_code_colors(cx, move |colors| colors.scheme = scheme));
        });
        cx.on_action(|_: &ToggleVivid, cx| cx.defer(|cx| change_code_colors(cx, |colors| colors.vivid = !colors.vivid)));
        cx.on_action(|_: &ToggleColoredRoles, cx| cx.defer(|cx| change_code_colors(cx, |colors| colors.colored_roles = !colors.colored_roles)));
        cx.on_action(|_: &ToggleRainbowBrackets, cx| cx.defer(|cx| change_code_colors(cx, |colors| colors.rainbow_brackets = !colors.rainbow_brackets)));
        cx.on_action(|_: &ToggleSemanticColors, cx| cx.defer(|cx| change_code_colors(cx, |colors| colors.semantic = !colors.semantic)));
        cx.on_action(|_: &ToggleStyleCheck, cx| cx.defer(|cx| {
            settings::update_later(cx, |settings| settings.editor.style_check = !settings.editor.style_check);
            menu::refresh(cx);
        }));
        cx.on_action(|_: &SetWritingGoal, cx| cx.defer(|cx| {
            if let (Ok((handle, _)), Some(editor)) = (control::target(cx), key_editor(cx)) {
                let _ = cx.update_window(handle, |_, window, cx| crate::status_line::ask_goal(&editor, window, cx));
            }
        }));
        cx.on_action(|_: &Print, cx| cx.defer(|cx| {
            if print_showing(None, cx).is_err() {
                crate::preview::render::beep();
            }
        }));
        cx.on_action(|_: &BiggerText, cx| cx.defer(|cx| change_font_size(Some(true), cx)));
        cx.on_action(|_: &SmallerText, cx| cx.defer(|cx| change_font_size(Some(false), cx)));
        cx.on_action(|_: &ActualTextSize, cx| cx.defer(|cx| change_font_size(None, cx)));
        cx.on_action(|action: &SetTextStyle, cx| {
            let style = action.style;
            cx.defer(move |cx| set_text_style(style, cx));
        });
        cx.on_action(|_: &HideApp, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
        cx.on_action(|_: &ToggleFullScreen, cx| cx.defer(|cx| {
            if let Ok((handle, _)) = control::target(cx) {
                let _ = cx.update_window(handle, |_, window, _| window.toggle_fullscreen());
            }
        }));
        cx.on_action(|_: &LookUp, cx| cx.defer(|cx| {
            if let (Ok((handle, _)), Some(editor)) = (control::target(cx), key_editor(cx)) {
                let _ = cx.update_window(handle, |_, window, cx| crate::native_menu::look_up(editor.read(cx), window, cx));
            }
        }));
        crate::speech::install(cx);
        cx.on_action(|_: &MarkdownBold, cx| cx.defer(|cx| markdown_format(MarkdownFormat::Bold, cx)));
        cx.on_action(|_: &MarkdownItalic, cx| cx.defer(|cx| markdown_format(MarkdownFormat::Italic, cx)));
        cx.on_action(|_: &MarkdownStrike, cx| cx.defer(|cx| markdown_format(MarkdownFormat::Strike, cx)));
        cx.on_action(|_: &MarkdownCode, cx| cx.defer(|cx| markdown_format(MarkdownFormat::Code, cx)));
        cx.on_action(|_: &MarkdownLink, cx| cx.defer(|cx| markdown_format(MarkdownFormat::Link, cx)));
        cx.on_action(|_: &crate::config_check::FormatJson, cx| cx.defer(format_json));
        // Deferred: the item can be picked while a window is being updated. A test launch opens it behind everything.
        cx.on_action(|_: &ToggleScratch, cx| cx.defer(|cx| {
            crate::scratch::toggle(std::env::var_os("GPUI_EDITOR_BACKGROUND").is_some(), cx);
        }));
        cx.on_action(|_: &BringAllToFront, cx| cx.activate(true));
        // An editor built later (a new tab, a tab restored) starts with the setting too.
        cx.observe_new(|state: &mut gpui_kit::component::input::EditorState, window, cx| {
            let editor = &settings::get(cx).editor;
            let (invisibles, numbers, typewriter) = (editor.show_invisibles, editor.line_numbers, editor.typewriter);
            if typewriter {
                state.set_typewriter(true, cx);
            }
            if let Some(window) = window {
                if invisibles {
                    state.set_show_whitespaces(true, window, cx);
                }
                if !numbers {
                    state.set_line_number(false, window, cx);
                }
            }
        })
        .detach();
        // The same for the gutter numbers: the file edited by hand shows or hides them at once.
        let numbered = Rc::new(Cell::new(settings::get(cx).editor.line_numbers));
        cx.observe_global::<settings::Store>(move |cx| {
            let show = settings::get(cx).editor.line_numbers;
            if numbered.replace(show) != show {
                cx.defer(move |cx| {
                    apply_line_numbers(show, cx);
                    menu::refresh(cx);
                });
            }
        })
        .detach();
        // The settings file edited by hand (it reloads on its own) turns the marks on or off at once.
        let shown = Rc::new(Cell::new(settings::get(cx).editor.show_invisibles));
        cx.observe_global::<settings::Store>(move |cx| {
            let show = settings::get(cx).editor.show_invisibles;
            if shown.replace(show) != show {
                cx.defer(move |cx| {
                    apply_invisibles(show, cx);
                    menu::refresh(cx);
                });
            }
        })
        .detach();
        // The same for the reading width.
        let reading = Rc::new(Cell::new(settings::get(cx).editor.reading_width));
        cx.observe_global::<settings::Store>(move |cx| {
            let on = settings::get(cx).editor.reading_width;
            if reading.replace(on) != on {
                cx.defer(move |cx| {
                    apply_reading_width(cx);
                    menu::refresh(cx);
                });
            }
        })
        .detach();
        // The settings file edited by hand turns typewriter scrolling on or off at once.
        let typing = Rc::new(Cell::new(settings::get(cx).editor.typewriter));
        cx.observe_global::<settings::Store>(move |cx| {
            let on = settings::get(cx).editor.typewriter;
            if typing.replace(on) != on {
                cx.defer(move |cx| {
                    apply_typewriter(on, cx);
                    menu::refresh(cx);
                });
            }
        })
        .detach();
        // The settings file edited by hand: a new `theme` switches every color with the fade, as the View menu's appearance does.
        let appearance = Rc::new(Cell::new(settings::get(cx).window.theme));
        cx.observe_global::<settings::Store>(move |cx| {
            let now = settings::get(cx).window.theme;
            if appearance.replace(now) != now {
                cx.defer(|cx| {
                    theme::apply_setting(cx);
                    cx.refresh_windows();
                });
            }
        })
        .detach();
        // The same for the glyph tuning: thickening, gamma and contrast reach the renderer at once.
        let tuning = Rc::new(Cell::new({
            let text = &settings::get(cx).text;
            (text.thicken, text.gamma, text.contrast)
        }));
        cx.observe_global::<settings::Store>(move |cx| {
            let text = &settings::get(cx).text;
            let now = (text.thicken, text.gamma, text.contrast);
            if tuning.replace(now) != now {
                cx.defer(|cx| {
                    tune_text(cx);
                    cx.refresh_windows();
                });
            }
        })
        .detach();
        // View > Styled Markdown and View > Code Colors, or the settings file edited by hand: the code roles follow the settings.
        // Rainbow brackets and semantic colors change no role, so those only hand the editors their palettes.
        let styled = Rc::new(RefCell::new((settings::get(cx).editor.styled_markdown, settings::get(cx).code_colors.clone())));
        cx.observe_global::<settings::Store>(move |cx| {
            let now = (settings::get(cx).editor.styled_markdown, settings::get(cx).code_colors.clone());
            let before = styled.replace(now.clone());
            if before.0 != now.0 || !before.1.same_palette(&now.1) {
                cx.defer(move |cx| {
                    theme::apply_syntax(cx);
                    menu::refresh(cx);
                });
            } else if before.1 != now.1 {
                cx.defer(|cx| {
                    theme::store_palettes(cx);
                    menu::refresh(cx);
                });
            }
        })
        .detach();
        menu::install(cx);
        crate::link_open::install(cx);
        crate::services::install(cx);
        cx.on_action(move |action: &OpenRecent, cx| open_picked(&action.path, background, cx));
        cx.on_action(|action: &ReopenWithEncoding, cx| {
            let encoding = action.encoding;
            // After this dispatch: the window can't be updated from inside it.
            cx.defer(move |cx| {
                let Some((handle, document)) = key_document(cx) else { return };
                handle.update(cx, |_, window, cx| document.update(cx, |document, cx| document.reopen_as(encoding, window, cx))).ok();
            });
        });
        cx.on_action(|action: &SetLineEndings, cx| {
            let crlf = action.crlf;
            cx.defer(move |cx| {
                let Some((handle, document)) = key_document(cx) else { return };
                handle.update(cx, |_, window, cx| document.update(cx, |document, cx| document.set_line_endings(crlf, window, cx))).ok();
                menu::refresh(cx);
            });
        });
        cx.on_action(|action: &ConvertToEncoding, cx| {
            let encoding = action.encoding;
            cx.defer(move |cx| {
                let Some((_, document)) = key_document(cx) else { return };
                document.update(cx, |document, cx| document.convert_to(encoding, cx));
                menu::refresh(cx);
            });
        });
        cx.on_action(|_: &ClearRecent, cx| {
            recent::clear();
            cx.defer(|cx| {
                menu::reload_recents(cx);
                menu::refresh(cx);
            });
        });
        // Opening from the folder tree (Return, double-click, Open to the Side) is an open too.
        cx.observe_new(|workspace: &mut Workspace, _, cx| {
            let explorer = workspace.explorer().clone();
            cx.subscribe(&explorer, |_, _, event: &ExplorerEvent, cx| {
                match event {
                    ExplorerEvent::Open(path) | ExplorerEvent::OpenToSide(path) => note_recent(path, cx),
                    // A file renamed or trashed here leaves Open Recent at once.
                    ExplorerEvent::Renamed { .. } | ExplorerEvent::Gone(_) => cx.defer(|cx| {
                        menu::reload_recents(cx);
                        menu::refresh(cx);
                    }),
                    _ => {}
                }
            })
            .detach();
        })
        .detach();
        cx.on_window_closed(|cx, _| {
            // The Window menu's greys (Merge all windows) follow the window count.
            cx.defer(menu::refresh);
            // Resident (`[scratch] resident`): no window left doesn't quit, so the shortcut still opens the scratch window.
            // The Settings window alone doesn't hold the app open: it closes with the last editor window and the app quits
            // in the same step (a window closing from inside this callback doesn't re-enter it).
            if crate::settings_window::only_settings_left(cx) && !settings::get(cx).scratch.resident {
                crate::settings_window::close(cx);
                workspace::quit_after_writes(cx);
            }
        })
        .detach();
        cx.on_app_quit(|cx| {
            // Quit confirmation has finished. Shutdown observers have only 200 ms, but this
            // synchronous part runs before that timer starts, so an outstanding write can finish.
            while crate::document::writes_in_flight() > 0 {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            settings::flush(cx);
            async {}
        })
        .detach();

        // The socket first: a request arriving while the session comes back is carried out after
        // this closure ends, so it finds the window.
        let parked: Vec<PathBuf> = inbox.borrow().urls.iter().filter_map(|url| url_path(url)).collect();
        control::start(background, &parked, cx);

        // Documents of a Dock or Finder launch arrive before `applicationDidFinishLaunching`, which
        // runs this closure, so an empty inbox here means none is coming and `start` opens now.
        if !defer || inbox.borrow().urls.is_empty() {
            // A file named on the command line is an open; a Dock or Finder launch only brings back the last session.
            if let Some(file) = start.file.as_deref().filter(|_| !defer) {
                note_recent(file, cx);
            }
            open_window(start, text, background, cx);
        }

        // The one global shortcut registration, after launch (`hotkey`); a test launch registers none.
        crate::hotkey::install(background, cx);
        cx.spawn(async move |cx| {
            loop {
                let urls = NextUrls(inbox.clone()).await;
                cx.update(|cx| {
                    for url in urls {
                        open_url(&url, background, cx);
                    }
                });
            }
        })
        .detach();
    });
}

/// One of our bindings, with the layout's key equivalents applied: on QWERTZ or AZERTY macOS moves
/// `{`, `}`, `?`, `.` and the like, and the binding follows (`KeyBinding::new` never maps).
fn key(keys: &str, action: impl Action, cx: &App) -> KeyBinding {
    KeyBinding::load(keys, Box::new(action), None, true, None, cx).expect("a valid key binding")
}

/// Every key binding of the app, the explorer's and the menu's included. Run at launch and again
/// after a layout change moved the keys (`run` clears the keymap and puts GPUI Kit's back first).
fn bind_all(cx: &mut App) {
    let bindings = [
        key("cmd-s", Save, cx),
        key("cmd-shift-s", SaveAs, cx),
        key("cmd-alt-s", SaveAll, cx),
        key("cmd-p", Print, cx),
        key("cmd-n", NewTab, cx),
        key("cmd-t", NewTab, cx),
        key("cmd-shift-n", NewWindow, cx),
        // Option-Cmd-R alone is the tree's own Show in Finder (a key-down on the focused tree), which a
        // binding would shadow, so the File items take Shift as well.
        key("cmd-alt-shift-r", ShowFileInFinder, cx),
        key("cmd-alt-shift-c", CopyFilePath, cx),
        // Finder's Move item here, Paste's Option alternate in Edit; the focused tree's own key does the same.
        key("cmd-alt-v", MoveItemHere, cx),
        key("cmd-o", Open, cx),
        key("cmd-shift-o", ShowOpenQuickly, cx),
        key("cmd-alt-o", ShowDraftList, cx),
        key("cmd-w", CloseTab, cx),
        key("cmd-shift-t", ReopenClosedTab, cx),
        key("cmd-shift-w", CloseWindow, cx),
        // Shift-[ arrives as `{`, so the bindings name the shifted keys (as Zed does).
        key("cmd-{", PrevTab, cx),
        key("cmd-}", NextTab, cx),
        key("ctrl-shift-tab", PrevRecentTab, cx),
        key("ctrl-tab", NextRecentTab, cx),
        key("cmd-1", ShowTab1, cx),
        key("cmd-2", ShowTab2, cx),
        key("cmd-3", ShowTab3, cx),
        key("cmd-4", ShowTab4, cx),
        key("cmd-5", ShowTab5, cx),
        key("cmd-6", ShowTab6, cx),
        key("cmd-7", ShowTab7, cx),
        key("cmd-8", ShowTab8, cx),
        key("cmd-9", ShowTab9, cx),
        key("cmd-,", OpenSettings, cx),
        key("cmd-h", HideApp, cx),
        key("cmd-alt-h", HideOthers, cx),
        key("cmd-ctrl-f", ToggleFullScreen, cx),
        key("cmd-ctrl-enter", ToggleMinimal, cx),
        key("cmd-ctrl-d", LookUp, cx),
        key("cmd-m", Minimize, cx),
        key("cmd-?", ShowHelp, cx),
        key("cmd-q", Quit, cx),
        key("cmd-b", ToggleExplorer, cx),
        key("cmd-shift-v", TogglePreview, cx),
        key("alt-z", CycleWrapWidth, cx),
        // Shift-\ arrives as `|`, so the binding names the shifted key like the tab ones above.
        key("cmd-\\", SplitRight, cx),
        key("cmd-|", SplitDown, cx),
        key("cmd-k left", FocusPaneLeft, cx),
        key("cmd-k right", FocusPaneRight, cx),
        key("cmd-k up", FocusPaneUp, cx),
        key("cmd-k down", FocusPaneDown, cx),
        // The arrow can follow Cmd-K with Cmd still held: the same four moves.
        key("cmd-k cmd-left", FocusPaneLeft, cx),
        key("cmd-k cmd-right", FocusPaneRight, cx),
        key("cmd-k cmd-up", FocusPaneUp, cx),
        key("cmd-k cmd-down", FocusPaneDown, cx),
        // The Edit and View menus show these.
        key("cmd-alt-f", FocusFilter, cx),
        key("cmd-shift-e", FocusExplorer, cx),
        key("cmd-e", UseSelectionForFind, cx),
        // Shift-Option-Cmd-F: the find shortcut with both modifiers the file shortcuts use for their folder-wide forms (free in the app and its kits).
        key("cmd-alt-shift-f", ShowFindInFolder, cx),
        // Option-Cmd-E: nothing else in the app or its kits binds it, and Xcode and TextEdit leave it free.
        key("cmd-alt-e", UseSelectionForReplace, cx),
        key("cmd-g", FindNext, cx),
        key("cmd-shift-g", FindPrevious, cx),
        key("cmd-ctrl-g", SelectAllSearchMatches, cx),
        key("cmd-d", SelectNextOccurrence, cx),
        // Zed's and VS Code's chord for it; Cmd-D itself is Select next match.
        key("cmd-k cmd-d", SkipNextOccurrence, cx),
        key("cmd-alt-c", ToggleMatchCase, cx),
        key("cmd-alt-w", ToggleWholeWord, cx),
        key("cmd-alt-r", ToggleRegex, cx),
        // Limit to selection; Xcode and BBEdit have the command, and nothing else binds Option-Cmd-L.
        key("cmd-alt-l", ToggleFindInSelection, cx),
        // BBEdit's Replace is Cmd-=; Xcode's Replace All is Option-Cmd-Return. Nothing else binds them.
        key("cmd-=", ReplaceMatch, cx),
        key("cmd-alt-=", ReplaceMatchAndFind, cx),
        key("cmd-alt-enter", ReplaceAllMatches, cx),
        key("cmd-l", ShowGoToLine, cx),
        key("cmd-ctrl-j", ShowGoToSymbol, cx),
        key("cmd-alt-i", ToggleInvisibles, cx),
        // Cmd-+ is Shift-Cmd-= on a US keyboard, and Cmd-= is Replace above; Xcode and TextEdit use these three.
        key("cmd-+", BiggerText, cx),
        key("cmd-shift-=", BiggerText, cx),
        key("cmd--", SmallerText, cx),
        key("cmd-0", ActualTextSize, cx),
        key("cmd-j", JumpToSelection, cx),
        // Edit > Markdown. Cmd-B is the tree's, so Bold takes Shift; Cmd-I, Shift-Cmd-X, Shift-Cmd-C and Option-Cmd-K are free.
        key("cmd-shift-b", MarkdownBold, cx),
        key("cmd-i", MarkdownItalic, cx),
        key("cmd-shift-x", MarkdownStrike, cx),
        key("cmd-shift-c", MarkdownCode, cx),
        key("cmd-alt-k", MarkdownLink, cx),
        // A binding with no context counts as the deepest one, so it beat the editor's own Cmd-Up (move to top);
        // under the window's context the focused editor's binding is deeper and wins, and the tree has none.
        KeyBinding::new("cmd-up", EnclosingFolder, Some("EditorWindow")),
        key("cmd-shift-j", RevealCurrentFile, cx),
        key("cmd-shift-.", ToggleHiddenFiles, cx),
        // Option-Cmd-Left and Right fold and unfold at the caret while the code editor has the keyboard; the tree keeps
        // Option-Cmd-Left for Collapse all folders only while it has focus (its own context), so the window's context
        // never competes with the editor's fold key. The editor's element sets its context to `Input mode=editor`
        // (replacing the `line_edit` word the base state gave it), so `line_edit` never matches there.
        KeyBinding::new("cmd-alt-left", CollapseAllFolders, Some("Tree")),
        KeyBinding::new("cmd-alt-left", FoldAtCaret, Some("Input && mode == editor")),
        KeyBinding::new("cmd-alt-right", UnfoldAtCaret, Some("Input && mode == editor")),
    ];
    cx.bind_keys(bindings);
}

/// The editor of the file showing in the key window's active pane (the window verbs act on while
/// none is key), `None` when that tab's editor isn't built yet.
pub fn key_editor(cx: &App) -> Option<Entity<gpui_kit::component::input::EditorState>> {
    let key = cx.active_window();
    let workspace = workspace::open_workspaces(cx).into_iter().find(|(handle, _)| Some(*handle) == key).map(|(_, workspace)| workspace);
    let workspace = workspace.or_else(|| control::target(cx).ok().map(|(_, workspace)| workspace))?;
    let document = workspace.read(cx).active_pane().read(cx).active_document()?.clone();
    document.read(cx).built_editor().cloned()
}

/// The document of the key window's active tab (the window verbs act on while none is key), with
/// the window it sits in; `None` while that window has no tab.
pub fn key_document(cx: &App) -> Option<(AnyWindowHandle, Entity<Document>)> {
    let key = cx.active_window();
    let found = workspace::open_workspaces(cx).into_iter().find(|(handle, _)| Some(*handle) == key);
    let (handle, workspace) = found.or_else(|| control::target(cx).ok())?;
    let document = workspace.read(cx).active_pane().read(cx).active_document()?.clone();
    Some((handle, document))
}

/// File > Print (Cmd-P): prints the showing tab's text, plain, in the editor's font and size. A
/// Markdown tab prints its source. `pdf` saves the job to that file with no panel (the `print-pdf`
/// verb); `None` opens the print sheet on the key window.
pub fn print_showing(pdf: Option<&std::path::Path>, cx: &mut App) -> Result<(), String> {
    let (handle, document) = key_document(cx).ok_or("no tab to print")?;
    let text = document.read(cx).built_editor().ok_or("the tab isn't built yet")?.read(cx).text().to_string();
    let editor = &settings::get(cx).editor;
    let family = editor.drawn_family(cx).map(String::from).unwrap_or_else(|| gpui_kit::component::ActiveTheme::theme(&*cx).mono_font_family.to_string());
    let size = f64::from(editor.font_size.unwrap_or(settings::FONT_USUAL));
    cx.update_window(handle, |_, window, _| {
        let view = raw_window_handle::HasWindowHandle::window_handle(window).ok().and_then(|handle| match handle.as_raw() {
            raw_window_handle::RawWindowHandle::AppKit(native) => Some(native.ns_view.as_ptr().cast::<objc2::runtime::AnyObject>()),
            _ => None,
        });
        // SAFETY: GPUI's view, alive while its window is updated.
        crate::native_menu::print_text(&text, &family, size, view.map(|view| unsafe { &*view }), pdf)
    })
    .map_err(|err| err.to_string())?
}

/// Edit > Find next and Find previous: select the next (or previous) match of the shared find text,
/// wrapping at the ends, with the find bar open or not. With a search in progress (the bar open,
/// or highlighted matches) it steps the session's current match, as the bar's own arrows do, so an
/// open bar's count agrees. With the search closed it steps from the selection. Nothing happens
/// while there is no find text. With the bar closed the step also shows its count and a wrap mark
/// on the workspace's find float.
pub fn step_find(forward: bool, cx: &mut App) {
    let Some(editor) = key_editor(cx) else { return };
    let step = editor.update(cx, |state, cx| {
        if state.search_session().query.is_empty() {
            return None;
        }
        gpui_kit::component::input::remember_find_query(&state.search_session().query, cx);
        let active = state.search_session().is_active();
        let selection = state.selected_range();
        // Also brings a closed search's matches up to date with the text, which it skips scanning.
        let stepped = if forward { state.next_search_match(cx) } else { state.previous_search_match(cx) };
        let ranges = state.search_session().matcher.matched_ranges();
        let target = if active {
            stepped
        } else if forward {
            ranges.iter().find(|range| range.start >= selection.end).or(ranges.first()).cloned()
        } else {
            ranges.iter().rev().find(|range| range.end <= selection.start).or(ranges.last()).cloned()
        }?;
        // It came round the end (or the start) when the match is not past (or before) the selection it stepped from.
        let wrapped = if forward { target.start < selection.end } else { target.end > selection.start };
        let step = ranges.iter().position(|range| *range == target).map(|at| Step { current: at + 1, total: ranges.len(), wrapped: wrapped.then_some(forward) });
        let open = state.search_session().open;
        let at = gpui_kit::component::input::RopeExt::offset_to_position(state.text(), target.start);
        state.unfold_at(at, cx);
        state.start_search_halo(target.clone(), cx);
        state.set_selected_range(target, cx);
        step.filter(|_| !open)
    });
    let Some(step) = step else { return };
    let key = cx.active_window();
    let found = workspace::open_workspaces(cx).into_iter().find(|(handle, _)| Some(*handle) == key);
    let Some((handle, workspace)) = found.or_else(|| control::target(cx).ok()) else { return };
    let _ = cx.update_window(handle, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.show_find_step(&editor, step, window, cx)));
}

/// Edit > Find > Match case and Whole word (Option-Cmd-C and Option-Cmd-W): flips the option of the
/// key editor's search, the one the find bar's buttons show. With the bar closed it only changes
/// the option the next search starts with, so no matches light up.
fn toggle_find_option(case: bool, cx: &mut App) {
    let Some(editor) = key_editor(cx) else { return };
    toggle_find_option_in(&editor, case, cx);
}

/// `toggle_find_option` on `editor`'s search: Find in folder's float flips the same options through it.
pub fn toggle_find_option_in(editor: &Entity<gpui_kit::component::input::EditorState>, case: bool, cx: &mut App) {
    editor.update(cx, |state, cx| {
        let session = state.search_session();
        let (query, was_active) = (session.query.clone(), session.is_active());
        let (ignore_case, whole_word) = (session.case_insensitive, session.whole_word);
        let (ignore_case, whole_word) = if case { (!ignore_case, whole_word) } else { (ignore_case, !whole_word) };
        if case {
            state.mark_search_case_chosen();
        }
        state.set_search_options(query, ignore_case, whole_word, cx);
        if !was_active {
            state.close_search(cx);
        }
    });
    menu::refresh(cx);
}

/// Edit > Find > Replace, Replace and find, Replace all: run on the key editor's open find bar
/// from wherever the caret is, so focus stays put.
fn replace_matches(command: FindReplace, cx: &mut App) {
    let Some(editor) = key_editor(cx) else { return };
    let Some((handle, _)) = key_document(cx) else { return };
    let _ = cx.update_window(handle, |_, window, cx| {
        gpui_kit::component::input::find_replace(&editor, command, window, cx);
    });
    menu::refresh(cx);
}

/// Edit > Find > Regular expression (Option-Cmd-R): the same as the find bar's button.
fn toggle_find_regex(cx: &mut App) {
    let Some(editor) = key_editor(cx) else { return };
    toggle_find_regex_in(&editor, cx);
}

/// `toggle_find_regex` on `editor`'s search (see `toggle_find_option_in`).
pub fn toggle_find_regex_in(editor: &Entity<gpui_kit::component::input::EditorState>, cx: &mut App) {
    editor.update(cx, |state, cx| {
        let was_active = state.search_session().is_active();
        let regex = !state.search_session().regex;
        state.set_search_regex(regex, cx);
        if !was_active {
            state.close_search(cx);
        }
    });
    menu::refresh(cx);
}

/// Edit > Find > Ignore diacritics: letters in the query find the same letters with any accent
/// (`cafe` finds `café`). Like Match case, it belongs to the key editor's search and with the bar
/// closed only changes what the next search starts with.
fn toggle_find_diacritics(cx: &mut App) {
    let Some(editor) = key_editor(cx) else { return };
    editor.update(cx, |state, cx| {
        let was_active = state.search_session().is_active();
        let on = !state.search_session().ignore_diacritics();
        state.set_search_ignore_diacritics(on, cx);
        if !was_active {
            state.close_search(cx);
        }
    });
    menu::refresh(cx);
}

/// Edit > Find > Find in selection (Option-Cmd-L): the same as the find bar's button. It needs the
/// bar open, where a selection can be taken as the scope; the menu row is grey otherwise.
fn toggle_find_in_selection(cx: &mut App) {
    let Some(editor) = key_editor(cx) else { return };
    editor.update(cx, |state, cx| {
        if state.search_session().open {
            let on = !state.search_session().in_selection();
            state.set_search_in_selection(on, cx);
        }
    });
    menu::refresh(cx);
}

/// Edit > Jump to selection: scrolls the editor to its selection.
fn jump_to_selection(cx: &mut App) {
    let Some(editor) = key_editor(cx) else { return };
    editor.update(cx, |state, cx| {
        let selection = state.selected_range();
        state.set_selected_range(selection, cx);
    });
}

/// What View > Fold, Unfold, Fold all and Unfold all do to the key editor.
enum Fold {
    AtCaret,
    UnfoldAtCaret,
    All,
    UnfoldAll,
}

/// View > Fold, Unfold, Fold all and Unfold all (Option-Cmd-Left and Right fold and unfold at the caret). They
/// act on the ranges the language gives, whether the gutter shows or not (minimal mode, line numbers off); a
/// press with nothing to fold or open beeps.
fn fold_command(fold: Fold, cx: &mut App) {
    let Some(editor) = key_editor(cx) else { return };
    let done = editor.update(cx, |state, cx| match fold {
        Fold::AtCaret => state.fold_at_caret(cx),
        Fold::UnfoldAtCaret => state.unfold_at_caret(cx),
        Fold::All => state.fold_all(cx),
        Fold::UnfoldAll => state.unfold_all(cx),
    });
    if !done {
        crate::preview::render::beep();
    }
}

/// Edit > Markdown: wraps or unwraps the key editor's selection, in a Markdown file only and while
/// the text itself has the keyboard (not the find bar, a field or the tree).
fn markdown_format(format: MarkdownFormat, cx: &mut App) {
    let Some((handle, document)) = key_document(cx) else { return };
    if !document.read(cx).has_preview() {
        return;
    }
    let Some(editor) = key_editor(cx) else { return };
    let _ = cx.update_window(handle, |_, window, cx| {
        if !editor.read(cx).focus_handle(cx).is_focused(window) {
            return;
        }
        editor.update(cx, |state, cx| gpui_kit::base::input::apply_markdown_format(state, format, window, cx));
    });
}

/// Edit > Transformations > Format JSON: re-indents the key window's JSON file in one Undo step;
/// a file that doesn't parse is left alone (the status line's pill says where it stops).
fn format_json(cx: &mut App) {
    let Some((handle, document)) = key_document(cx) else { return };
    if document.read(cx).language(cx) != "json" {
        return;
    }
    let Some(editor) = key_editor(cx) else { return };
    let _ = cx.update_window(handle, |_, window, cx| crate::config_check::format_json_in(&editor, window, cx));
}

/// What Edit > Find's multi-caret commands add: every match, the next occurrence, or the next one
/// in place of the newest pick.
#[derive(Clone, Copy)]
enum Occurrences {
    All,
    Next,
    Skip,
}

/// Turn Find results or repeated selected text into editable selections.
fn select_occurrences(which: Occurrences, cx: &mut App) {
    let Some(editor) = key_editor(cx) else { return };
    let Some((handle, _)) = key_document(cx) else { return };
    let _ = cx.update_window(handle, |_, window, cx| {
        editor.update(cx, |state, cx| {
            let changed = match which {
                Occurrences::All => state.select_all_search_matches(cx),
                Occurrences::Next => state.select_next_occurrence(cx),
                Occurrences::Skip => state.skip_next_occurrence(cx),
            };
            if changed {
                state.focus(window, cx);
            }
        });
    });
}

/// View > Show invisibles: flips the setting and puts it on every editor in every window.
fn toggle_invisibles(cx: &mut App) {
    settings::update_later(cx, |settings| settings.editor.show_invisibles = !settings.editor.show_invisibles);
    apply_invisibles(settings::get(cx).editor.show_invisibles, cx);
    menu::refresh(cx);
}

/// View > Show line numbers: flips the setting and puts it on every editor in every window.
fn toggle_line_numbers(cx: &mut App) {
    settings::update_later(cx, |settings| settings.editor.line_numbers = !settings.editor.line_numbers);
    apply_line_numbers(settings::get(cx).editor.line_numbers, cx);
    menu::refresh(cx);
}

/// View > Reading width: flips the setting and puts it on every editor in every window.
fn toggle_reading_width(cx: &mut App) {
    settings::update_later(cx, |settings| settings.editor.reading_width = !settings.editor.reading_width);
    apply_reading_width(cx);
    menu::refresh(cx);
}

/// View > Smart punctuation: flips the setting. Each document reads it when a key lands, so nothing else changes.
fn toggle_smart_punctuation(cx: &mut App) {
    settings::update_later(cx, |settings| settings.editor.smart_punctuation = !settings.editor.smart_punctuation);
    menu::refresh(cx);
}

/// View > Typewriter scrolling: flips the setting (one flag, written once) and puts it on every editor in every window.
fn toggle_typewriter(cx: &mut App) {
    settings::update_later(cx, |settings| settings.editor.typewriter = !settings.editor.typewriter);
    apply_typewriter(settings::get(cx).editor.typewriter, cx);
    menu::refresh(cx);
}

/// View > Styled Markdown: flips the setting (one write) and puts the Markdown roles on the theme, so every editor restyles.
fn toggle_styled_markdown(cx: &mut App) {
    settings::update_later(cx, |settings| settings.editor.styled_markdown = !settings.editor.styled_markdown);
    theme::apply_syntax(cx);
    menu::refresh(cx);
}

/// View > Code Colors, and the Settings window's Code Colors pane: changes one of the `[code_colors]` settings (one write), takes the palette it means, and the menu's checks follow.
pub(crate) fn change_code_colors(cx: &mut App, change: impl FnOnce(&mut settings::CodeColors)) {
    settings::update_later(cx, |settings| change(&mut settings.code_colors));
    theme::apply_syntax(cx);
    menu::refresh(cx);
}

/// The settings file as a tab: writes the template first when the file is missing or empty, then opens it as any
/// file (an open tab comes forward).
pub(crate) fn open_settings_file(background: bool, cx: &mut App) {
    settings::seed_template(cx);
    let _ = open_outside(&settings::Store::default_path(), None, background, cx);
}

/// Writing > Style > Edit word list: the style check's word list as a tab, made from its starting list first when it
/// isn't there (an existing file is never touched). A launch that may not write opens nothing.
pub(crate) fn open_word_list(cx: &mut App) {
    let (path, may_write) = settings::style_words_file(cx);
    if !may_write {
        return;
    }
    crate::style_check::seed_words_file(&path);
    let _ = open_outside(&path, None, std::env::var_os("GPUI_EDITOR_BACKGROUND").is_some(), cx);
}

/// View > Bigger, Smaller (`Some(up)`) and Actual size (`None`): sets the editor font size in the
/// settings, which every editor in every window reads when it draws. A change within 500 ms of the
/// last one shares its write, so holding the key writes the file once.
fn change_font_size(step: Option<bool>, cx: &mut App) {
    let now = settings::get(cx).editor.font_size.unwrap_or(settings::FONT_USUAL);
    let next = match step {
        Some(up) => settings::font_step(now, up),
        None => settings::FONT_USUAL,
    };
    if next == now {
        return;
    }
    settings::update_later(cx, |settings| settings.editor.font_size = Some(next));
    cx.refresh_windows();
    menu::refresh(cx);
}

/// View > Text style: sets the style of the key window's kind of file (prose or code) in the settings, one write per pick. Picking the style already on does nothing.
fn set_text_style(style: settings::Typeface, cx: &mut App) {
    let prose = key_document(cx).is_some_and(|(_, document)| crate::text_style::is_prose(document.read(cx).language(cx)));
    if crate::text_style::of(settings::get(cx), prose) == style {
        return;
    }
    settings::update_later(cx, |settings| {
        if prose {
            settings.editor.prose_style = style;
        } else {
            settings.editor.code_style = style;
        }
    });
    cx.refresh_windows();
    menu::refresh(cx);
}

/// Scale per pinch step: the size moves one rung of `settings::font_step` each time the gesture's
/// accumulated scale passes this (or its inverse).
const PINCH_STEP: f32 = 1.09;
/// The least time between two pinch steps, so a fast pinch re-wraps the text a few times, not once
/// per event.
const PINCH_GAP: std::time::Duration = std::time::Duration::from_millis(60);

/// One trackpad pinch over the editor: the scale its events add up to, and when it last stepped.
struct Pinch {
    scale: f32,
    stepped: Option<std::time::Instant>,
    /// Whether a step changed the size since the last write; the gesture's end (or the settle task)
    /// writes once when it did.
    changed: bool,
    /// Ends the gesture if no event follows for `PINCH_SETTLE`: a cancelled pinch reaches here as a
    /// `Moved` and never ends. Replaced on each event, dropped when the gesture ends.
    settle: Option<Task<()>>,
}

impl Default for Pinch {
    fn default() -> Self {
        Self { scale: 1., stepped: None, changed: false, settle: None }
    }
}

impl Pinch {
    /// Adds one event's `delta` (0.1 is 10% bigger) to the scale. Returns `Some(up)` when the scale
    /// has passed a step and the rate limit allows one; the scale starts over from 1 after a step.
    /// Inside the limit it keeps adding, held to two steps' worth so a reversal answers promptly.
    fn feed(&mut self, delta: f32, now: std::time::Instant) -> Option<bool> {
        if !delta.is_finite() {
            return None;
        }
        self.scale = (self.scale * (1. + delta).max(0.)).clamp(PINCH_STEP.powi(-2), PINCH_STEP.powi(2));
        let up = if self.scale >= PINCH_STEP {
            true
        } else if self.scale <= 1. / PINCH_STEP {
            false
        } else {
            return None;
        };
        if self.stepped.is_some_and(|last| now.saturating_duration_since(last) < PINCH_GAP) {
            return None;
        }
        self.scale = 1.;
        self.stepped = Some(now);
        Some(up)
    }
}

impl Global for Pinch {}

/// How long after the last pinch event a gesture that never ended is taken as ended.
const PINCH_SETTLE: std::time::Duration = std::time::Duration::from_millis(400);

/// A pinch over the editor (`on_pinch`): each step the scale adds up to moves the size one rung and
/// ticks the trackpad (firmer on Actual size), and redraws every window; the settings file is
/// written once, when the gesture ends (`Ended`, or `PINCH_SETTLE` after the last event). A step at
/// either end of the range does nothing. `MayBegin` reaches here as `Started` with a delta of about
/// 0, which only resets the count.
pub fn pinch_text(delta: f32, phase: TouchPhase, cx: &mut App) {
    let now = settings::get(cx).editor.font_size.unwrap_or(settings::FONT_USUAL);
    let pinch = cx.default_global::<Pinch>();
    if phase == TouchPhase::Started {
        // Not a fresh `Pinch`: a cancelled pinch's size is still unwritten, and this gesture's timer carries it into the one write.
        pinch.scale = 1.;
        pinch.stepped = None;
    }
    let step = match phase {
        TouchPhase::Started | TouchPhase::Moved => pinch.feed(delta, std::time::Instant::now()),
        TouchPhase::Ended | TouchPhase::Cancelled => None,
    };
    if let Some(up) = step {
        let next = settings::font_step(now, up);
        if next != now {
            settings::update_unsaved(cx, |settings| settings.editor.font_size = Some(next));
            cx.global_mut::<Pinch>().changed = true;
            if next == settings::FONT_USUAL {
                crate::haptics::level_change();
            } else {
                crate::haptics::align();
            }
            cx.refresh_windows();
        }
    }
    if matches!(phase, TouchPhase::Ended | TouchPhase::Cancelled) {
        // Dropping the pending settle task cancels it.
        let pinch = std::mem::take(cx.global_mut::<Pinch>());
        write_pinch(pinch.changed, cx);
    } else {
        let settle = cx.spawn(async move |cx| {
            cx.background_executor().timer(PINCH_SETTLE).await;
            cx.update(|cx| {
                let pinch = cx.global_mut::<Pinch>();
                // This task is running: it goes on to the write and is not cancelled by the reset.
                if let Some(task) = pinch.settle.take() {
                    task.detach();
                }
                let changed = std::mem::take(pinch).changed;
                write_pinch(changed, cx);
            });
        });
        cx.global_mut::<Pinch>().settle = Some(settle);
    }
}

/// The gesture's one write: whatever size the settings hold now, and the menu's Bigger and Smaller
/// greying. Does nothing when no step changed the size. The gesture's events run inside the pinched
/// window's update, where the menu can't read the key window, so its refresh waits a turn.
fn write_pinch(changed: bool, cx: &mut App) {
    if changed {
        settings::update_later(cx, |_| {});
        cx.defer(menu::refresh);
    }
}

/// Puts the glyph settings on the renderer: it reads them each frame, and `thicken` is part of each glyph's cache key.
fn tune_text(cx: &App) {
    let text = &settings::get(cx).text;
    gpui_apple::text_tuning::set_thicken(text.thicken.min(4));
    gpui_apple::text_tuning::set_gamma(text.gamma);
    gpui_apple::text_tuning::set_contrast(text.contrast);
}

/// Puts the marks for spaces and tabs on or off every open editor in every window.
fn apply_invisibles(show: bool, cx: &mut App) {
    for (handle, workspace) in workspace::open_workspaces(cx) {
        cx.update_window(handle, |_, window, cx| {
            for document in workspace.read(cx).documents(cx) {
                if let Some(editor) = document.read(cx).built_editor().cloned() {
                    editor.update(cx, |state, cx| state.set_show_whitespaces(show, window, cx));
                }
            }
        })
        .ok();
    }
}

/// Puts typewriter scrolling on or off every open editor in every window.
fn apply_typewriter(on: bool, cx: &mut App) {
    for (handle, workspace) in workspace::open_workspaces(cx) {
        cx.update_window(handle, |_, _, cx| {
            for document in workspace.read(cx).documents(cx) {
                if let Some(editor) = document.read(cx).built_editor().cloned() {
                    editor.update(cx, |state, cx| state.set_typewriter(on, cx));
                }
            }
        })
        .ok();
    }
}

/// Puts the gutter numbers on or off every open editor in every window. The wrap width follows the
/// gutter at the next draw, which holds the top line.
fn apply_line_numbers(show: bool, cx: &mut App) {
    for (handle, workspace) in workspace::open_workspaces(cx) {
        cx.update_window(handle, |_, window, cx| {
            for document in workspace.read(cx).documents(cx) {
                if let Some(editor) = document.read(cx).built_editor().cloned() {
                    editor.update(cx, |state, cx| state.set_line_number(show, window, cx));
                }
            }
        })
        .ok();
    }
}

/// Puts every open editor's reading column on or off, from the setting and each file's language.
fn apply_reading_width(cx: &mut App) {
    for (handle, workspace) in workspace::open_workspaces(cx) {
        cx.update_window(handle, |_, _, cx| {
            for document in workspace.read(cx).documents(cx) {
                document.update(cx, |document, cx| document.apply_reading_width(cx));
            }
        })
        .ok();
    }
}

/// Opens the document or folder a `file://` URL names, as `open_outside` does.
fn open_url(url: &str, background: bool, cx: &mut App) {
    if let Some(path) = url_path(url) {
        let _ = open_outside(&path, None, background, cx);
    }
}

/// Tells macOS `path` was opened, so File > Open Recent and the Dock list it (Untitled drafts and
/// paths that don't exist are left out), and brings the menu up to date.
pub fn note_recent(path: &Path, cx: &mut App) {
    if !path.exists() || drafts::is_draft(path) {
        return;
    }
    cx.add_recent_document(path);
    cx.defer(|cx| {
        menu::reload_recents(cx);
        menu::refresh(cx);
    });
}

/// The window a file opened from outside joins: the one whose tree root holds it (the deepest
/// such root), else the key window, else the one the control socket last acted on.
fn window_for(file: &Path, cx: &mut App) -> Option<(AnyWindowHandle, Entity<Workspace>)> {
    let all = workspace::open_workspaces(cx);
    let mut best: Option<(usize, (AnyWindowHandle, Entity<Workspace>))> = None;
    // Files from outside never land in the scratch window.
    for (handle, workspace) in all.iter().filter(|(_, workspace)| !workspace.read(cx).is_scratch()) {
        let root = workspace.read(cx).root();
        if !file.starts_with(root) {
            continue;
        }
        let depth = root.components().count();
        if best.as_ref().is_none_or(|(deepest, _)| depth > *deepest) {
            best = Some((depth, (*handle, workspace.clone())));
        }
    }
    best.map(|(_, window)| window).or_else(|| key_window(cx))
}

/// The key window's workspace, else the one the control socket last acted on.
fn key_window(cx: &App) -> Option<(AnyWindowHandle, Entity<Workspace>)> {
    let key = cx.active_window();
    let outside = |(_, workspace): &(AnyWindowHandle, Entity<Workspace>)| !workspace.read(cx).is_scratch();
    workspace::open_workspaces(cx)
        .into_iter()
        .filter(outside)
        .find(|(handle, _)| Some(*handle) == key)
        .or_else(|| control::target(cx).ok().filter(outside))
        .or_else(|| workspace::open_workspaces(cx).into_iter().rev().find(outside))
}

/// The folder of the guide's pages: the app bundle's `Contents/Resources/guide` (which `scripts/ship.sh`
/// fills), else, for a binary run outside a bundle, the repo's `docs/guide`. `None` when neither
/// holds an `index.md`. Canonical, so it compares to the paths tabs hold.
pub fn guide_dir() -> Option<&'static Path> {
    static DIR: std::sync::LazyLock<Option<PathBuf>> = std::sync::LazyLock::new(|| {
        let bundled = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.canonicalize().ok())
            .and_then(|exe| Some(exe.parent()?.parent()?.join("Resources/guide")));
        let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/guide");
        [bundled, Some(dev)].into_iter().flatten().find(|dir| dir.join("index.md").is_file()).and_then(|dir| dir.canonicalize().ok())
    });
    DIR.as_deref()
}

/// Help > Finch Help: the guide's index in a new tab of the key window, with the preview
/// showing. The tab is read-only (`document::writable`). It isn't added to Open Recent.
fn show_help(background: bool, cx: &mut App) {
    let Some(index) = guide_dir().map(|dir| dir.join("index.md")) else { return };
    if !settings::get(cx).preview.visible {
        settings::update_later(cx, |settings| settings.preview.visible = true);
    }
    let Some((handle, workspace)) = key_window(cx) else {
        let _ = open_path(&index, None, background, cx);
        return;
    };
    cx.update_window(handle, |_, window, cx| {
        workspace.update(cx, |workspace, cx| workspace.open_file(index, window, cx));
        if !background {
            window.activate_window();
        }
    })
    .ok();
    if !background {
        cx.activate(true);
    }
}

/// What a waiting `finch --wait` watches for the end of: the tab a file opened in, or the
/// window a folder (or a file that doesn't exist yet) opened.
pub enum Closing {
    Tab(Entity<Document>),
    Window(Entity<Workspace>),
}

impl Closing {
    /// Runs `done` once the tab or window is gone; dropping the subscription cancels it.
    pub fn on_close(self, cx: &mut App, done: impl FnOnce() + 'static) -> Subscription {
        match self {
            Closing::Tab(document) => cx.observe_release(&document, move |_, _| done()),
            Closing::Window(workspace) => cx.observe_release(&workspace, move |_, _| done()),
        }
    }
}

/// Opens a file as a tab (or brings its tab forward) in the window `window_for` picks, with the
/// caret on `caret` when given, and brings that window forward unless `background`. A folder, a
/// file that doesn't exist yet, or no window open gets a new window as `open_path` makes. Returns
/// what to watch for its closing, or the read error after showing why it failed.
pub fn open_outside(path: &Path, caret: Option<Caret>, background: bool, cx: &mut App) -> io::Result<Closing> {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    note_recent(&path, cx);
    let Some((handle, workspace)) = path.is_file().then(|| window_for(&path, cx)).flatten() else {
        return open_path(&path, caret, background, cx);
    };
    let document = cx
        .update_window(handle, |_, window, cx| {
            let document = workspace.update(cx, |workspace, cx| workspace.open_file_at(path, caret, window, cx));
            // A draft open in another window was raised there: this one stays behind.
            let here = document.as_ref().is_ok_and(|document| workspace.read(cx).documents(cx).contains(document));
            if !background && here {
                window.activate_window();
            }
            document
        })
        .map_err(|err| io::Error::other(err.to_string()))?;
    if !background {
        cx.activate(true);
    }
    document.map(Closing::Tab)
}

/// Opens a new window for the document or folder at `path`, with the caret on `caret` when given
/// and `path` is a file. Returns what to watch for its closing, or the read error after showing why.
pub fn open_path(path: &Path, caret: Option<Caret>, background: bool, cx: &mut App) -> io::Result<Closing> {
    let cwd = PathBuf::from("/");
    let mut start = start::resolve(Some(path), &cwd, &settings::get(cx).explorer);
    start.caret = caret;
    // A scrap draft opens as its Untitled page, not as a file tab that would fight the page.
    let draft = (path.is_file() && drafts::is_draft(path)).then(|| path.to_path_buf());
    if draft.is_some() {
        start.file = None;
    }
    match read_text(start.file.as_deref()) {
        Ok(opened) => {
            let workspace = open_window(start, opened, background, cx);
            if let Some(draft) = draft {
                let workspace = workspace.clone();
                cx.defer(move |cx| {
                    let found = workspace::open_workspaces(cx).into_iter().find(|(_, other)| *other == workspace);
                    if let Some((handle, workspace)) = found {
                        handle.update(cx, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.open_draft(draft, window, cx))).ok();
                    }
                });
            }
            Ok(Closing::Window(workspace))
        }
        Err(err) => {
            let detail = crate::file_ops::explain("open", path, &err);
            eprintln!("finch: {detail}");
            let title = crate::file_ops::failed_title("open", &[path.to_path_buf()]);
            // Wait until the whole incoming batch has had a chance to open a window.
            // A Finder launch containing only unreadable files still needs somewhere to explain.
            cx.defer(move |cx| {
                if cx.windows().is_empty() {
                    new_window(background, cx);
                }
                if let Some((handle, _)) = key_window(cx) {
                    handle.update(cx, |_, window, cx| {
                        let _ = window.prompt(PromptLevel::Critical, &title, Some(&detail), &["OK"], cx);
                    }).ok();
                }
            });
            Err(err)
        }
    }
}

/// The Dock menu's New Window: an empty window (an Untitled page) rooted where the last one was,
/// else at the home folder.
pub fn new_window(background: bool, cx: &mut App) {
    let mut start = start::resolve(None, Path::new("/"), &settings::get(cx).explorer);
    start.file = None;
    if let Ok(opened) = read_text(None) {
        open_window(start, opened, background, cx);
        if !background {
            cx.activate(true);
        }
    }
}

/// File > Open: asks for files or folders; each file opens as a tab in the key window, each
/// folder in a new window.
fn open_picker(background: bool, cx: &mut App) {
    let picked = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: true,
        multiple: true,
        prompt: None,
    });
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = picked.await else { return };
        cx.update(|cx| {
            for path in paths {
                open_picked(&path, background, cx);
            }
        });
    })
    .detach();
}

/// One path File > Open chose: a file joins the key window (not the window whose tree holds it),
/// a folder gets a new window.
fn open_picked(path: &Path, background: bool, cx: &mut App) {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    note_recent(&path, cx);
    let Some((handle, workspace)) = path.is_file().then(|| key_window(cx)).flatten() else {
        let _ = open_path(&path, None, background, cx);
        return;
    };
    cx.update_window(handle, |_, window, cx| {
        workspace.update(cx, |workspace, cx| workspace.open_file(path, window, cx));
    })
    .ok();
}

/// The path a `file://` URL names, percent-decoded.
fn url_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let mut bytes = Vec::with_capacity(rest.len());
    let raw = rest.as_bytes();
    let mut at = 0;
    while at < raw.len() {
        let decoded = (raw[at] == b'%')
            .then(|| raw.get(at + 1..at + 3))
            .flatten()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match decoded {
            Some(byte) => {
                bytes.push(byte);
                at += 3;
            }
            None => {
                bytes.push(raw[at]);
                at += 1;
            }
        }
    }
    let path = PathBuf::from(OsString::from_vec(bytes));
    path.is_absolute().then_some(path)
}

/// The text and modification time for a start. `None` text defers a large file until its window
/// can ask before reading it. No file, or a missing one, holds an empty page.
pub fn read_text(file: Option<&Path>) -> io::Result<(Option<Decoded>, Option<std::time::SystemTime>)> {
    let mtime = crate::document::path_mtime(file);
    match file.map(encoding::read_file) {
        Some(Err(err)) if err.kind() == io::ErrorKind::FileTooLarge => Ok((None, mtime)),
        Some(Err(err)) if err.kind() != io::ErrorKind::NotFound => Err(err),
        Some(Ok(decoded)) => Ok((Some(decoded), mtime)),
        _ => Ok((Some(Decoded::utf8(String::new())), None)),
    }
}

/// Opens a window on `start` (its file already read into `text`, last written at `mtime`). The
/// first window of a launch comes back where the last one was (`saved_placement`); any other
/// cascades from the key window (`cascaded_placement`) so it doesn't sit exactly on top of one.
fn open_window(start: Start, (text, mtime): (Option<Decoded>, Option<std::time::SystemTime>), background: bool, cx: &mut App) -> Entity<Workspace> {
    let title = title(start.file.as_deref());
    open_with(title, None, background, cx, move |window, cx| Workspace::new(start, text, mtime, window, cx))
}

/// Opens a window whose one tab is `document`, moved from another window (the tab was dragged
/// out, or sent by the control socket): the same entity, so its text, caret, scroll, undo and
/// unsaved edits come with it. `place` is where it lands (`dropped_placement`); `None` cascades.
/// `card` is where the dragged tab's card was let go (global screen points): the window starts there, at the card's size, and grows
/// to its place (`window_look::grow_from`); `None` fades in as any window does.
pub fn open_tab_window(document: Entity<Document>, pinned: bool, root: PathBuf, place: Option<(WindowBounds, Option<DisplayId>)>, card: Option<Bounds<Pixels>>, background: bool, cx: &mut App) -> Entity<Workspace> {
    let start = Start { root, file: document.read(cx).path.clone(), caret: None };
    let title = title(start.file.as_deref());
    let workspace = open_with(title, place, background, cx, move |window, cx| {
        document.update(cx, |document, cx| document.rehome(window, cx));
        if let Some((card, full)) = card.zip(screen_frame(window, cx)) {
            let size = card.size.min(&full.size);
            let at = card.origin - full.origin;
            window_look::grow_from(window, [f64::from(at.x), f64::from(at.y), f64::from(size.width), f64::from(size.height)], cx);
        }
        Workspace::with_document(start, document, pinned, window, cx)
    });
    if !background {
        cx.activate(true);
    }
    workspace
}

/// The window's size from the settings, never under what the explorer at its narrowest and the
/// editor at its readable least need, with that least (the height is the floor the saved size already had).
fn window_size(cx: &App) -> (Size<Pixels>, Size<Pixels>) {
    let window = settings::window(cx);
    let min_size = size(PANEL_WIDTH_MIN + EDITOR_MIN, WINDOW_MIN_HEIGHT);
    (size(px(window.width).max(min_size.width), px(window.height).max(min_size.height)), min_size) // design: settings-driven window size
}

#[repr(C)]
struct CGRect {
    origin: [f64; 2],
    size: [f64; 2],
}

unsafe extern "C" {
    fn CGDisplayBounds(display: u32) -> CGRect;
}

/// Display `id`'s frame in global screen points (the primary display's top-left is 0, 0), which
/// GPUI's own display bounds don't give: they start at 0, 0 on every display.
pub fn display_frame(id: DisplayId) -> Bounds<Pixels> {
    // The id is the CoreGraphics display number.
    let rect = unsafe { CGDisplayBounds(u64::from(id) as u32) };
    Bounds { origin: gpui_kit::point(px(rect.origin[0] as f32), px(rect.origin[1] as f32)), size: size(px(rect.size[0] as f32), px(rect.size[1] as f32)) }
}

/// `window`'s frame in global screen points, so windows on different displays compare.
pub fn screen_frame(window: &Window, cx: &App) -> Option<Bounds<Pixels>> {
    let origin = display_frame(window.display(cx)?.id()).origin;
    let local = window.bounds();
    Some(Bounds { origin: origin + local.origin, size: local.size })
}

/// Where a window opened by a tab let go at `point` (global screen points) goes: on the display
/// under it, the settings size, its top-left a little up and left of the pointer so the tab
/// bar's first tab sits where the card was dropped, kept on that display.
pub fn dropped_placement(point: Point<Pixels>, cx: &App) -> Option<(WindowBounds, Option<DisplayId>)> {
    let display = cx.displays().into_iter().find(|display| display_frame(display.id()).contains(&point))?;
    let (size, _) = window_size(cx);
    let origin = point - display_frame(display.id()).origin - gpui_kit::point(px(DROP_GRAB.0), px(DROP_GRAB.1));
    Some((WindowBounds::Windowed(fit_to(Bounds { origin, size }, display.visible_bounds())), Some(display.id())))
}

/// Opens the editor window `make` builds, titled `title`, at `place` (else the saved or cascaded place).
fn open_with(title: String, place: Option<(WindowBounds, Option<DisplayId>)>, background: bool, cx: &mut App, make: impl FnOnce(&mut Window, &mut gpui_kit::Context<Workspace>) -> Workspace + 'static) -> Entity<Workspace> {
    let window = settings::window(cx);
    // The explorer at its narrowest beside the editor at its readable least; the height is the floor the saved size already had.
    let (size, min_size) = window_size(cx);
    let restored = if cx.windows().is_empty() { saved_placement(&window, size, cx) } else { None };
    let (window_bounds, display_id) = place.or(restored).or_else(|| cascaded_placement(size, cx)).unwrap_or_else(|| (WindowBounds::Windowed(Bounds::centered(None, size, cx)), None));
    let options = WindowOptions {
        window_bounds: Some(window_bounds),
        display_id,
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            appears_transparent: true,
            // None: the unified toolbar (`window_look::unified_title_bar`) centers the
            // lights in the explorer's top band (`tokens::TOP_BAND`).
            traffic_light_position: None,
        }),
        // The top band moves the window and a double-click zooms it
        // (`Workspace` render); AppKit then leaves the title bar to the app.
        app_owns_titlebar_drag: true,
        // The explorer's sidebar material shows through where nothing draws.
        window_background: WindowBackgroundAppearance::Transparent,
        // The explorer at its narrowest and the editor at its readable least always fit.
        window_min_size: Some(min_size),
        focus: !background,
        // Unshown: GPUI would order the window front before its first frame
        // (`window_look::fade_in`, `free_frame_pacing`).
        show: false,
        // No frame cap when the window isn't key (skill gpui, rule 10): GPUI's
        // default 33.3 ms runs motion behind other windows at about 27 fps.
        inactive_frame_interval: None,
        ..Default::default()
    };
    let (_, workspace) = gpui_kit::open_window(options, cx, |window, cx| {
        window_look::free_frame_pacing(window, background);
        if !background {
            window_look::fade_in(window, cx);
        }
        theme::watch(window);
        cx.new(|cx| make(window, cx))
    })
    .expect("failed to open the editor window");
    workspace
}

/// The saved bounds and display for the launch window, when the display they were saved on is still
/// connected and the window's top-left corner still lies on it; `None` centers the window instead.
fn saved_placement(saved: &settings::WindowSettings, size: Size<Pixels>, cx: &App) -> Option<(WindowBounds, Option<DisplayId>)> {
    let (x, y, uuid) = (saved.x?, saved.y?, saved.display.as_ref()?);
    let display = cx.displays().into_iter().find(|display| display.uuid().is_ok_and(|id| id.to_string() == *uuid))?;
    let origin = point(px(x), px(y));
    if !display.bounds().contains(&origin) {
        return None;
    }
    let bounds = fit_to(Bounds { origin, size }, display.visible_bounds());
    let window_bounds = match saved.state {
        settings::WindowState::Windowed => WindowBounds::Windowed(bounds),
        settings::WindowState::Maximized => WindowBounds::Maximized(bounds),
        settings::WindowState::Fullscreen => WindowBounds::Fullscreen(bounds),
    };
    Some((window_bounds, Some(display.id())))
}

/// Where a window opened beside others goes: `CASCADE` down and to the right of the key window's
/// frame (the most recently opened window's while none is key, as in a background launch), on that
/// window's display. `None` with no window to read, so the caller centers it.
fn cascaded_placement(size: Size<Pixels>, cx: &mut App) -> Option<(WindowBounds, Option<DisplayId>)> {
    let key = cx.active_window();
    let mut handles: Vec<_> = workspace::open_workspaces(cx).into_iter().map(|(handle, _)| handle).collect();
    handles.reverse();
    if let Some(at) = key.and_then(|key| handles.iter().position(|handle| *handle == key)) {
        handles.swap(0, at);
    }
    // Display-local points, as `Window::bounds`, `visible_bounds` and `WindowOptions` all use on macOS.
    let (frame, visible, display_id) = handles.into_iter().find_map(|handle| {
        handle
            .update(cx, |_, window, cx| {
                let display = window.display(cx)?;
                Some((window.window_bounds().get_bounds(), display.visible_bounds(), display.id()))
            })
            .ok()
            .flatten()
    })?;
    Some((WindowBounds::Windowed(cascade(frame, size, visible)), Some(display_id)))
}

/// The frame `CASCADE` down and to the right of `from`, wrapping back to the visible area's
/// top-left (as AppKit's cascade does) when that would leave it.
fn cascade(from: Bounds<Pixels>, size: Size<Pixels>, visible: Bounds<Pixels>) -> Bounds<Pixels> {
    let size = size.min(&visible.size);
    let step = px(CASCADE);
    let origin = point(from.origin.x + step, from.origin.y + step);
    let (right, bottom) = (visible.bottom_right().x, visible.bottom_right().y);
    let fits = origin.x >= visible.origin.x && origin.y >= visible.origin.y && origin.x + size.width <= right && origin.y + size.height <= bottom;
    Bounds { origin: if fits { origin } else { visible.origin }, size }
}

/// `bounds` inside `visible`: the size first (never larger than the area), then the origin.
fn fit_to(bounds: Bounds<Pixels>, visible: Bounds<Pixels>) -> Bounds<Pixels> {
    let size = bounds.size.min(&visible.size);
    let max = visible.bottom_right() - point(size.width, size.height);
    let origin = point(bounds.origin.x.max(visible.origin.x).min(max.x), bounds.origin.y.max(visible.origin.y).min(max.y));
    Bounds { origin, size }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Bounds, Pixels, point, px, size};

    use super::{CASCADE, cascade, fit_to};

    fn at(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds { origin: point(px(x), px(y)), size: size(px(w), px(h)) }
    }

    #[test]
    fn cascade_steps_down_right_and_wraps_to_the_visible_corner() {
        let visible = at(0., 25., 1000., 700.);
        let moved = cascade(at(100., 100., 400., 300.), size(px(400.), px(300.)), visible);
        assert_eq!((moved.origin.x, moved.origin.y), (px(100. + CASCADE), px(100. + CASCADE)));
        let wrapped = cascade(at(590., 400., 400., 300.), size(px(400.), px(300.)), visible);
        assert_eq!((wrapped.origin.x, wrapped.origin.y), (px(0.), px(25.)));
    }

    #[test]
    fn fit_clamps_size_before_origin() {
        let visible = at(0., 25., 1000., 700.);
        let fitted = fit_to(at(900., 600., 1400., 900.), visible);
        assert_eq!((fitted.origin.x, fitted.origin.y, fitted.size.width, fitted.size.height), (px(0.), px(25.), px(1000.), px(700.)));
    }

    #[test]
    fn pinch_steps_at_the_threshold_one_rung_at_a_time() {
        use super::{PINCH_GAP, Pinch};
        let start = std::time::Instant::now();
        let mut pinch = Pinch::default();
        // Small events add up multiplicatively: 1.03^3 passes 1.09, 1.03^2 doesn't.
        assert_eq!(pinch.feed(0.03, start), None);
        assert_eq!(pinch.feed(0.03, start), None);
        assert_eq!(pinch.feed(0.03, start), Some(true));
        // The count starts over after a step.
        assert_eq!(pinch.feed(0.03, start + PINCH_GAP), None);
        // Out is the inverse: 0.95 stays inside 1/1.09 (0.917), 0.95^2 passes it.
        let mut pinch = Pinch::default();
        assert_eq!(pinch.feed(-0.05, start), None);
        assert_eq!(pinch.feed(-0.05, start), Some(false));
    }

    #[test]
    fn pinch_is_rate_limited_and_never_banks_more_than_two_steps() {
        use super::{PINCH_GAP, Pinch};
        let start = std::time::Instant::now();
        let mut pinch = Pinch::default();
        assert_eq!(pinch.feed(0.2, start), Some(true));
        // A huge swing inside the gap waits.
        assert_eq!(pinch.feed(2., start + PINCH_GAP / 2), None);
        assert_eq!(pinch.feed(0., start + PINCH_GAP / 2), None);
        // Once the gap is over it takes one step, then waits again.
        assert_eq!(pinch.feed(0., start + PINCH_GAP), Some(true));
        assert_eq!(pinch.feed(0., start + PINCH_GAP + PINCH_GAP / 2), None);
        // The count starts over from 1 after a step, so the next one needs a fresh swing.
        let later = start + PINCH_GAP * 4;
        assert_eq!(pinch.feed(-0.05, later), None);
        assert_eq!(pinch.feed(-0.05, later), Some(false));
    }

    #[test]
    fn pinch_ignores_numbers_that_are_not_finite() {
        use super::Pinch;
        let mut pinch = Pinch::default();
        assert_eq!(pinch.feed(f32::NAN, std::time::Instant::now()), None);
        assert_eq!(pinch.feed(f32::INFINITY, std::time::Instant::now()), None);
        assert_eq!(pinch.scale, 1.);
    }

    /// Cmd-K then an arrow with Cmd still held: the pending chord is matched as a whole, so the editor's own one-key Cmd-arrow never takes the second key.
    #[test]
    fn cmd_k_cmd_arrow_is_a_pane_focus_chord_the_editor_arrow_does_not_take() {
        use gpui_kit::{KeyBinding, KeyContext, Keymap, Keystroke};
        let keymap = Keymap::new(vec![
            KeyBinding::new("cmd-left", super::ToggleExplorer, Some("Editor")),
            KeyBinding::new("cmd-k left", super::FocusPaneLeft, None),
            KeyBinding::new("cmd-k cmd-left", super::FocusPaneLeft, None),
            KeyBinding::new("cmd-k cmd-down", super::FocusPaneDown, None),
        ]);
        let context = [KeyContext::parse("Editor").unwrap()];
        let press = |text: &'static str| Keystroke::parse(text).unwrap();
        let (_, pending) = keymap.bindings_for_input(&[press("cmd-k")], &context);
        assert!(pending, "Cmd-K waits for a second key");
        let (found, pending) = keymap.bindings_for_input(&[press("cmd-k"), press("cmd-left")], &context);
        assert!(!pending);
        assert_eq!(found.len(), 1);
        assert!(found[0].action().as_any().is::<super::FocusPaneLeft>());
        let (found, _) = keymap.bindings_for_input(&[press("cmd-k"), press("cmd-down")], &context);
        assert!(found[0].action().as_any().is::<super::FocusPaneDown>());
        // Alone, the arrow is still the editor's.
        let (found, _) = keymap.bindings_for_input(&[press("cmd-left")], &context);
        assert!(found[0].action().as_any().is::<super::ToggleExplorer>());
    }
}
