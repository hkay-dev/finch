//! The Settings window's panes: General, Editor, Writing, Code Colors and Text. Each is a list of sections built from
//! `settings_controls`, reading the settings as the pane is built and writing through `controls::write` (or the helper the View
//! menu's item uses, `app::change_code_colors`), so a change shows in the editors and the menu's checks at once and a file edited by
//! hand shows here. Wording is plain and short; every footnote says what the setting does, not how it is stored.

use std::{cell::OnceCell, ffi::CStr, ops::Range, rc::Rc, time::Instant};

use gpui_kit::component::{
    ActiveTheme as _,
    input::{InputEvent, InputState},
};
use gpui_kit::*;
use objc2::{class, msg_send, rc::Retained, runtime::{AnyObject, Bool}};

use crate::{
    app::{self, OpenSettingsFile},
    control,
    native_menu::{Entry, Item},
    motion::{self, Fade},
    settings::{self, CodeColors, Scheme, Settings, ThemeSetting, Typeface},
    settings_controls::{self as controls, Slider, button, field, glyph_button, popup_row, row, section, segmented, slider_row, stepper_row, swatch, switch_row, write},
    settings_window::{Pane, SettingsWindow},
    tab_bar,
    theme,
    tokens::{self, Palette},
};

type Cx<'a> = Context<'a, SettingsWindow>;

impl SettingsWindow {
    /// The sections of `pane`, built from the settings as they stand.
    pub(crate) fn sections(&mut self, pane: Pane, window: &mut Window, cx: &mut Cx) -> Vec<AnyElement> {
        controls::begin_pane();
        match pane {
            Pane::General => self.general(window, cx),
            Pane::Editor => editor(window, cx),
            Pane::Writing => writing(window, cx),
            Pane::CodeColors => self.code_colors(window, cx),
            Pane::Text => text(window, cx),
        }
    }

    /// A field that shows a setting takes the file's text when no one is typing in it, so a hand edit shows here.
    pub(crate) fn follow_file(&mut self, window: &mut Window, cx: &mut Cx) {
        let want = settings::get(cx).scratch.shortcut.clone();
        follow(&self.shortcut, &want, window, cx);
    }

    /// Esc in a field: every field shows the saved text again, and the pane list takes the keys back (Up and Down move panes).
    pub(crate) fn revert_fields(&mut self, window: &mut Window, cx: &mut Cx) {
        let want = settings::get(cx).scratch.shortcut.clone();
        self.shortcut.update(cx, |input, cx| input.set_value(want, window, cx));
        for ((light, role), (input, _)) in &self.hex {
            let colors = &settings::get(cx).code_colors;
            if let Some(held) = (if *light { &colors.light } else { &colors.dark }).get(role).cloned() {
                input.update(cx, |input, cx| input.set_value(held, window, cx));
            }
        }
        window.focus(&self.focus, cx);
    }

    fn general(&mut self, window: &mut Window, cx: &mut Cx) -> Vec<AnyElement> {
        let settings = settings::get(cx);
        let (chosen, minimal, hidden, resident) = (settings.window.theme, settings.window.minimal, settings.explorer.show_hidden, settings.scratch.resident);
        let ignored = settings.explorer.show_ignored;
        let (explorer, preview) = (settings.explorer.visible, settings.preview.visible);
        let bad = crate::hotkey::parse(&self.shortcut.read(cx).value()).is_err();
        // The shortcut in glyphs beside its words, while nobody is typing in the field.
        let hint = if self.shortcut.read(cx).focus_handle(cx).is_focused(window) { None } else { crate::hotkey::glyphs(&self.shortcut.read(cx).value()) };
        vec![
            section(
                Some("Window".into()),
                vec![
                    segmented("general-appearance", "Appearance", Some("System follows your Mac’s light or dark mode."), &["System", "Light", "Dark"], [ThemeSetting::System, ThemeSetting::Light, ThemeSetting::Dark].iter().position(|choice| *choice == chosen).unwrap_or(0), set_appearance),
                    switch_row("general-minimal", "Minimal mode", Some("Only the text shows: no tabs, folder tree or status line. Press ⌃⌘↩ to turn it on or off."), minimal, true, window, cx, set_minimal),
                ],
                None,
            ),
            section(
                Some("Panels".into()),
                vec![
                    switch_row("general-explorer", "Folder tree", None, explorer, true, window, cx, set_explorer),
                    switch_row("general-preview", "Markdown preview", None, preview, true, window, cx, set_preview),
                ],
                Some("⌘B shows or hides the folder tree, and ⇧⌘V the preview.".into()),
            ),
            section(
                Some("Folder tree".into()),
                vec![
                    switch_row("general-hidden", "Show hidden files", Some("Files that start with a dot."), hidden, true, window, cx, set_hidden),
                    switch_row("general-ignored", "Show ignored files", Some("Files .gitignore leaves out, like build output, dimmed."), ignored, true, window, cx, set_ignored),
                ],
                Some("The .git folder stays out either way.".into()),
            ),
            section(
                Some("Scratch window".into()),
                vec![
                    row("Shortcut", None, field(&self.shortcut, tokens::SETTINGS_FIELD_WIDTH, bad, hint, cx)).into_any_element(),
                    switch_row(
                        "general-resident",
                        "Keep Finch running with no window",
                        Some("Closing the last window doesn’t quit, so the shortcut still works."),
                        resident,
                        true,
                        window,
                        cx,
                        |on, cx| write(cx, move |settings| settings.scratch.resident = on),
                    ),
                ],
                Some("The shortcut opens a scratch page from any app. Use ctrl, alt, cmd or shift with one key, joined by dashes, like ctrl-alt-space. Type off to turn it off.".into()),
            ),
            section(
                None,
                vec![row(
                    "Settings file",
                    None,
                    button("general-open-file", "Open settings file").on_click(cx.listener(|_, _, window, cx| window.dispatch_action(Box::new(OpenSettingsFile), cx))),
                )
                .into_any_element()],
                Some("Every setting is kept in a plain text file you can edit by hand. A change there shows up here at once.".into()),
            ),
        ]
    }

    fn code_colors(&mut self, window: &mut Window, cx: &mut Cx) -> Vec<AnyElement> {
        let light = theme::is_light(cx);
        let colors = settings::get(cx).code_colors.clone();
        let scheme = colors.scheme;
        let held = if light { &colors.light } else { &colors.dark };
        // A field whose role is gone (removed here or in the file) or whose appearance isn't shown goes with it.
        self.hex.retain(|(side, role), _| *side == light && held.contains_key(role));
        let mut rows = Vec::with_capacity(held.len() + 1);
        for (role, hex) in held {
            rows.push(self.override_row(light, role, hex, window, cx));
        }
        rows.push(popup_row(
            "colors-add",
            "Add a color",
            None,
            "Choose a role",
            true,
            {
                let taken: Vec<String> = held.keys().cloned().collect();
                move || {
                    ROLES.iter().map(|(role, name)| Entry::Item(Item::new(*name, *role).disabled(taken.iter().any(|taken| taken == role)))).collect()
                }
            },
            cx,
            move |role: &'static str, cx| {
                let hex = format!("#{:06X}", palette_hex(tokens::palette(scheme, light), role));
                cx.defer(move |cx| {
                    app::change_code_colors(cx, move |colors| {
                        (if light { &mut colors.light } else { &mut colors.dark }).entry(role.to_owned()).or_insert(hex);
                    })
                });
            },
        ));
        if !held.is_empty() {
            rows.push(
                row(
                    "Reset colors",
                    Some(format!("Takes off all your colors in the {} theme.", if light { "light" } else { "dark" }).into()),
                    button("colors-reset", "Reset").on_click(move |_, _, cx| {
                        cx.defer(move |cx| app::change_code_colors(cx, move |colors| (if light { &mut colors.light } else { &mut colors.dark }).clear()))
                    }),
                )
                .into_any_element(),
            );
        }
        // The rows are closed into their section here, ahead of the page's other sections, which each take their own rows.
        let appearance = if light { "light" } else { "dark" };
        let yours = section(
            Some(format!("Your colors in the {appearance} theme").into()),
            rows,
            Some(format!("These change single roles of the scheme in the {appearance} theme. Switch the appearance to set the other theme’s.").into()),
        );
        vec![
            section(
                None,
                vec![popup_row(
                    "colors-scheme",
                    "Color scheme",
                    None,
                    scheme.title(),
                    false,
                    move || Scheme::ALL.iter().map(|each| Entry::Item(Item::new(each.title(), *each).checked(*each == scheme))).collect(),
                    cx,
                    |picked: Scheme, cx| cx.defer(move |cx| app::change_code_colors(cx, move |colors| colors.scheme = picked)),
                )],
                Some("Every scheme has a dark and a light version, and Finch uses the one that fits your appearance.".into()),
            ),
            // The preview holds no row, so a search leaves it out.
            if controls::filtering() { div().hidden().into_any_element() } else { preview(&mut self.glide, window, cx) },
            section(
                Some("Options".into()),
                vec![
                    colors_switch("colors-vivid", "Vivid", "Brighter hues, each kept easy to read on the page.", colors.vivid, window, cx, |colors, on| colors.vivid = on),
                    colors_switch("colors-roles", "Colored roles", "Fields, parameters, constants, methods and more each get a hue of their own.", colors.colored_roles, window, cx, |colors, on| colors.colored_roles = on),
                    colors_switch("colors-rainbow", "Rainbow brackets", "Brackets take a hue by how deep they nest.", colors.rainbow_brackets, window, cx, |colors, on| colors.rainbow_brackets = on),
                    colors_switch("colors-semantic", "Semantic colors", "Each variable gets a hue of its own from its spelling.", colors.semantic, window, cx, |colors, on| colors.semantic = on),
                ],
                None,
            ),
            yours,
        ]
    }

    /// A role's row: its swatch and name, a hex field and a button that removes it.
    fn override_row(&mut self, light: bool, role: &str, hex: &str, window: &mut Window, cx: &mut Cx) -> AnyElement {
        let input = match self.hex.get(&(light, role.to_owned())) {
            Some((input, _)) => input.clone(),
            None => {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder("#RRGGBB"));
                input.update(cx, |input, cx| input.set_value(hex.to_owned(), window, cx));
                let name = role.to_owned();
                let subscription = cx.subscribe_in(&input, window, move |_, input, event: &InputEvent, _, cx| {
                    if !matches!(event, InputEvent::Change) {
                        return;
                    }
                    let Some(value) = settings::parse_hex(&input.read(cx).value()) else { return };
                    let hex = format!("#{value:06X}");
                    let colors = &settings::get(cx).code_colors;
                    if (if light { &colors.light } else { &colors.dark }).get(&name).is_some_and(|held| held.eq_ignore_ascii_case(&hex)) {
                        return;
                    }
                    let name = name.clone();
                    cx.defer(move |cx| app::change_code_colors(cx, move |colors| drop((if light { &mut colors.light } else { &mut colors.dark }).insert(name, hex))));
                });
                self.hex.insert((light, role.to_owned()), (input.clone(), subscription));
                input
            }
        };
        follow(&input, hex, window, cx);
        let shown = settings::parse_hex(hex).map_or_else(|| hsla(0., 0., 0., 0.), |value| rgb(value).into());
        let bad = settings::parse_hex(&input.read(cx).value()).is_none();
        let label = ROLES.iter().find(|(name, _)| *name == role).map_or(role, |(_, label)| *label).to_owned();
        controls::tag(&label, None);
        let name = role.to_owned();
        let remove = glyph_button(SharedString::from(format!("colors-remove-{role}-{light}")), "xmark.circle.fill").on_click(move |_, _, cx| {
            let name = name.clone();
            cx.defer(move |cx| app::change_code_colors(cx, move |colors| drop((if light { &mut colors.light } else { &mut colors.dark }).remove(&name))));
        });
        div()
            .min_h(px(tokens::SETTINGS_ROW_HEIGHT))
            .py(px(tokens::SETTINGS_ROW_PAD))
            .flex()
            .items_center()
            .gap(px(tokens::SETTINGS_ROW_GAP))
            .child(swatch(shown))
            .child(controls::lit_label(label.into()).flex_1().min_w_0().truncate())
            .child(field(&input, tokens::SETTINGS_HEX_WIDTH, bad, None, cx))
            .child(remove)
            .into_any_element()
    }
}

/// `input` shows `want` unless someone is typing in it.
fn follow(input: &Entity<InputState>, want: &str, window: &mut Window, cx: &mut Cx) {
    let (focused, same) = {
        let state = input.read(cx);
        (state.focus_handle(cx).is_focused(window), state.value().as_ref() == want)
    };
    if !focused && !same {
        input.update(cx, |input, cx| input.set_value(want.to_owned(), window, cx));
    }
}

// ---- General ----

/// The appearance picker: the setting, the colors it means and every window redrawn, as `debug theme` does.
fn set_appearance(at: usize, cx: &mut App) {
    let choice = [ThemeSetting::System, ThemeSetting::Light, ThemeSetting::Dark][at.min(2)];
    cx.defer(move |cx| {
        settings::update(cx, |settings| settings.window.theme = choice);
        theme::apply_setting(cx);
        cx.refresh_windows();
    });
}

/// Minimal mode for the window the editor commands act on, as the View menu's item does; the setting follows with no window open.
fn set_minimal(on: bool, cx: &mut App) {
    cx.defer(move |cx| {
        if let Ok((handle, workspace)) = control::target(cx) {
            handle.update(cx, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.set_minimal(on, window, cx))).ok();
        }
        write(cx, move |settings| settings.window.minimal = on);
    });
}

/// Hidden files in the folder tree: the explorer re-reads its folders (`toggle_hidden`), and the setting follows with no window open.
fn set_hidden(on: bool, cx: &mut App) {
    cx.defer(move |cx| {
        if settings::get(cx).explorer.show_hidden != on {
            match control::target(cx) {
                Ok((_, workspace)) => workspace.update(cx, |workspace, cx| workspace.explorer().update(cx, |explorer, cx| explorer.toggle_hidden(cx))),
                Err(_) => write(cx, move |settings| settings.explorer.show_hidden = on),
            }
        }
        crate::menu::refresh(cx);
    });
}

/// Ignored files in the folder tree, as `set_hidden` does hidden ones (`toggle_ignored`).
fn set_ignored(on: bool, cx: &mut App) {
    cx.defer(move |cx| {
        if settings::get(cx).explorer.show_ignored != on {
            match control::target(cx) {
                Ok((_, workspace)) => workspace.update(cx, |workspace, cx| workspace.explorer().update(cx, |explorer, cx| explorer.toggle_ignored(cx))),
                Err(_) => write(cx, move |settings| settings.explorer.show_ignored = on),
            }
        }
        crate::menu::refresh(cx);
    });
}

/// A panel's switch: Cmd-B or Shift-Cmd-V in the window the editor commands act on (so its own rules hold, as Minimal mode's, and the
/// View menu's check follows), and the setting alone with no window open.
fn set_panel(on: bool, cx: &mut App, shown: fn(&Settings) -> bool, toggle: fn() -> Box<dyn Action>, set: fn(&mut Settings, bool)) {
    cx.defer(move |cx| {
        if shown(settings::get(cx)) != on {
            match control::target(cx) {
                Ok((handle, _)) => drop(cx.update_window(handle, |_, window, cx| window.dispatch_action(toggle(), cx))),
                Err(_) => write(cx, move |settings| set(settings, on)),
            }
        }
        crate::menu::refresh(cx);
    });
}

fn set_explorer(on: bool, cx: &mut App) {
    set_panel(on, cx, |settings| settings.explorer.visible, || Box::new(app::ToggleExplorer), |settings, on| settings.explorer.visible = on);
}

fn set_preview(on: bool, cx: &mut App) {
    set_panel(on, cx, |settings| settings.preview.visible, || Box::new(app::TogglePreview), |settings, on| settings.preview.visible = on);
}

// ---- Editor ----

fn editor(window: &mut Window, cx: &mut Cx) -> Vec<AnyElement> {
    let settings = settings::get(cx);
    let editor = &settings.editor;
    let (family, size, prose, code) = (editor.font_family.clone(), editor.font_size.unwrap_or(settings::FONT_USUAL), editor.prose_style, editor.code_style);
    let (wrap, width, numbers, invisibles, smart, typewriter, trim, newline) =
        (editor.soft_wrap, editor.reading_width, editor.line_numbers, editor.show_invisibles, editor.smart_punctuation, editor.typewriter, editor.trim_trailing_whitespace, editor.insert_final_newline);
    let shown = family.clone().unwrap_or_else(|| "Default".to_owned());
    let typefaces = |name: &'static str, label: &'static str, now: Typeface, cx: &mut Cx, set: fn(&mut Settings, Typeface)| {
        popup_row(
            name,
            label,
            None,
            match now {
                Typeface::Mono => "Monospaced",
                Typeface::Serif => "Serif",
                Typeface::Sans => "Sans",
            },
            false,
            move || {
                [(Typeface::Mono, "Monospaced"), (Typeface::Serif, "Serif"), (Typeface::Sans, "Sans")].into_iter().map(|(face, title)| Entry::Item(Item::new(title, face).checked(face == now))).collect()
            },
            cx,
            move |face: Typeface, cx| write(cx, move |settings| set(settings, face)),
        )
    };
    vec![
        section(
            Some("Font".into()),
            vec![
                popup_row(
                    "editor-font",
                    "Family",
                    None,
                    shown,
                    false,
                    move || {
                        let mut entries = vec![Entry::Item(Item::new("Default", None::<String>).checked(family.is_none())), Entry::Separator];
                        let installed = monospace_families();
                        // A family the file names that isn't installed as monospace stays in the list, checked.
                        if let Some(current) = family.as_ref().filter(|current| !installed.contains(current)) {
                            entries.push(Entry::Item(Item::new(current.clone(), Some(current.clone())).checked(true)));
                        }
                        entries.extend(installed.into_iter().map(|name| {
                            let checked = family.as_deref() == Some(name.as_str());
                            Entry::Item(Item::new(name.clone(), Some(name)).checked(checked))
                        }));
                        entries
                    },
                    cx,
                    |family: Option<String>, cx| write(cx, move |settings| settings.editor.font_family = family),
                ),
                stepper_row(
                    "editor-size",
                    "Size",
                    None,
                    format!("{size:.0} pt"),
                    (Some(settings::font_step(size, true)).filter(|next| *next != size), Some(settings::font_step(size, false)).filter(|next| *next != size)),
                    true,
                    window,
                    |next, cx| write(cx, move |settings| settings.editor.font_size = Some(next)),
                ),
                typefaces("editor-prose", "Markdown and text", prose, cx, |settings, face| settings.editor.prose_style = face),
                typefaces("editor-code", "Code", code, cx, |settings, face| settings.editor.code_style = face),
            ],
            Some("Serif is the Mac’s New York and Sans is its system font. Monospaced uses the font above.".into()),
        ),
        section(
            Some("Layout".into()),
            vec![
                toggle("editor-wrap", "Wrap long lines", Some("What a file starts with. View > Soft wrap changes it for one file."), wrap, window, cx, |settings, on| settings.editor.soft_wrap = on),
                toggle("editor-width", "Reading width", Some("Wrapped text stops at a readable width. ⌥Z switches one file to the window's width and back. Minimal mode centers it."), width, window, cx, |settings, on| settings.editor.reading_width = on),
                toggle("editor-numbers", "Line numbers", None, numbers, window, cx, |settings, on| settings.editor.line_numbers = on),
                toggle("editor-invisibles", "Show invisibles", Some("Marks for spaces and tabs."), invisibles, window, cx, |settings, on| settings.editor.show_invisibles = on),
            ],
            None,
        ),
        section(
            Some("Typing".into()),
            vec![
                toggle("editor-smart", "Smart punctuation", Some("Curly quotes, … for three dots and an en dash for two hyphens, in Markdown and text."), smart, window, cx, |settings, on| settings.editor.smart_punctuation = on),
                toggle("editor-typewriter", "Typewriter scrolling", Some("The line you type stays put and the page glides under it."), typewriter, window, cx, |settings, on| settings.editor.typewriter = on),
            ],
            None,
        ),
        section(
            Some("Saving".into()),
            vec![
                toggle("editor-trim", "Trim trailing spaces", Some("A save removes spaces and tabs at the ends of lines."), trim, window, cx, |settings, on| settings.editor.trim_trailing_whitespace = on),
                toggle("editor-newline", "End files with a new line", Some("A save makes the text end with a line break."), newline, window, cx, |settings, on| settings.editor.insert_final_newline = on),
            ],
            None,
        ),
    ]
}

/// A switch row for a boolean `set` puts in the settings.
fn toggle(name: &'static str, label: &'static str, note: Option<&'static str>, on: bool, window: &Window, cx: &mut Cx, set: fn(&mut Settings, bool)) -> AnyElement {
    switch_row(name, label, note, on, true, window, cx, move |value, cx| write(cx, move |settings| set(settings, value)))
}

thread_local! {
    static FAMILIES: OnceCell<Vec<String>> = const { OnceCell::new() };
}

/// The installed font families that are monospaced, by name (`NSFontManager`), read once.
fn monospace_families() -> Vec<String> {
    /// `NSFixedPitchFontMask`.
    const FIXED_PITCH: usize = 1 << 10;
    FAMILIES.with(|families| {
        families
            .get_or_init(|| {
                let mut found = Vec::new();
                // SAFETY: NSFontManager's family list and font lookup, on the main thread; each object is checked for nil.
                unsafe {
                    let manager: Retained<AnyObject> = msg_send![class!(NSFontManager), sharedFontManager];
                    let names: Retained<AnyObject> = msg_send![&*manager, availableFontFamilies];
                    let count: usize = msg_send![&*names, count];
                    for at in 0..count {
                        let name: Retained<AnyObject> = msg_send![&*names, objectAtIndex: at];
                        let font: Option<Retained<AnyObject>> = msg_send![&*manager, fontWithFamily: &*name, traits: FIXED_PITCH, weight: 5isize, size: 12.0f64];
                        let Some(font) = font else { continue };
                        let fixed: Bool = msg_send![&*font, isFixedPitch];
                        let chars: *const std::ffi::c_char = msg_send![&*name, UTF8String];
                        if fixed.as_bool() && !chars.is_null() {
                            let name = CStr::from_ptr(chars).to_string_lossy().into_owned();
                            if !name.starts_with('.') {
                                found.push(name);
                            }
                        }
                    }
                }
                found
            })
            .clone()
    })
}

// ---- Writing ----

fn writing(window: &mut Window, cx: &mut Cx) -> Vec<AnyElement> {
    let editor = &settings::get(cx).editor;
    let (styled, spelling, check, words, picked) = (editor.styled_markdown, editor.check_spelling, editor.style_check, editor.style_sentence_words, editor.highlight.clone());
    let classes: Vec<AnyElement> = [("nouns", "Nouns"), ("verbs", "Verbs"), ("adjectives", "Adjectives"), ("adverbs", "Adverbs"), ("conjunctions", "Conjunctions")]
        .into_iter()
        .map(|(key, title)| {
            let name = match key {
                "nouns" => "writing-nouns",
                "verbs" => "writing-verbs",
                "adjectives" => "writing-adjectives",
                "adverbs" => "writing-adverbs",
                _ => "writing-conjunctions",
            };
            switch_row(name, title, None, picked.iter().any(|held| held.as_str() == key), true, window, cx, move |on, cx| {
                write(cx, move |settings| {
                    let list = &mut settings.editor.highlight;
                    list.retain(|held| held.as_str() != key);
                    if on {
                        list.push(key.to_owned());
                    }
                })
            })
        })
        .collect();
    // Five at a time up to 200: a long sentence limit is a round number.
    let up = (words / 5 * 5 + 5).min(200);
    let down = (words.saturating_sub(1) / 5 * 5).max(5);
    // Built here, before the sections above it on the page, so each section takes the rows built just before it (`section`).
    let parts = section(Some("Highlight parts of speech".into()), classes, Some("Tints each kind of word in Markdown and text files.".into()));
    vec![
        section(
            Some("Markdown".into()),
            vec![toggle("writing-styled", "Styled Markdown", Some("Headings are semibold and marks like # and ** fade."), styled, window, cx, |settings, on| settings.editor.styled_markdown = on)],
            None,
        ),
        section(
            Some("Spelling".into()),
            vec![toggle("writing-spelling", "Check spelling", Some("A red underline under misspelled words in prose, comments and strings."), spelling, window, cx, |settings, on| settings.editor.check_spelling = on)],
            None,
        ),
        section(
            Some("Style".into()),
            vec![
                toggle("writing-style", "Style check", Some("Faint marks on words from your style word list and on long sentences."), check, window, cx, |settings, on| settings.editor.style_check = on),
                row(
                    "Word list",
                    Some("The words and phrases the style check marks, one per line.".into()),
                    button("writing-word-list", "Edit word list…").on_click(move |_, _, cx| {
                        if check {
                            cx.defer(app::open_word_list);
                        }
                    }),
                )
                .opacity(controls::fade(controls::dead(window, "writing-word-list", !check)))
                .into_any_element(),
                stepper_row(
                    "writing-words",
                    "Long sentence",
                    None,
                    format!("{words} words"),
                    (Some(up).filter(|next| *next != words), Some(down).filter(|next| *next != words)),
                    check,
                    window,
                    |next, cx| write(cx, move |settings| settings.editor.style_sentence_words = next),
                ),
            ],
            Some("A sentence with more words than this gets a mark. The word list and the length need the style check on.".into()),
        ),
        parts,
    ]
}

// ---- Code Colors ----

/// A Code Colors switch: the `[code_colors]` setting `set` changes, through the helper the View menu's item uses.
fn colors_switch(name: &'static str, label: &'static str, note: &'static str, on: bool, window: &Window, cx: &mut Cx, set: fn(&mut CodeColors, bool)) -> AnyElement {
    switch_row(name, label, Some(note), on, true, window, cx, move |value, cx| cx.defer(move |cx| app::change_code_colors(cx, move |colors| set(colors, value))))
}

/// The roles the Add pop-up offers, with the words the rows use.
const ROLES: [(&str, &str); 21] = [
    ("keyword", "Keywords"),
    ("string", "Strings"),
    ("number", "Numbers"),
    ("comment", "Comments"),
    ("type", "Types"),
    ("type.interface", "Interfaces and traits"),
    ("function", "Functions"),
    ("function.special", "Macros"),
    ("attribute", "Attributes"),
    ("constant", "Constants"),
    ("property", "Fields"),
    ("variable.parameter", "Parameters"),
    ("variable.special", "self"),
    ("variant", "Enum variants"),
    ("type.builtin", "Built-in types"),
    ("function.method", "Methods"),
    ("label", "Labels and lifetimes"),
    ("string.escape", "String escapes"),
    ("punctuation.list_marker", "List markers"),
    ("markup.quote", "Quote marks"),
    ("link_uri", "Links"),
];

/// The scheme's own color for one of `ROLES`, the one a new override starts from.
fn palette_hex(palette: &Palette, role: &str) -> u32 {
    match role {
        "keyword" => palette.keyword,
        "string" => palette.string,
        "number" => palette.number,
        "comment" => palette.comment,
        "type" => palette.type_,
        "type.interface" => palette.declared_type,
        "function" => palette.function,
        "function.special" => palette.macro_,
        "attribute" => palette.attribute,
        "constant" => palette.constant,
        "property" => palette.field,
        "variable.parameter" => palette.parameter,
        "variable.special" => palette.self_,
        "variant" => palette.variant,
        "type.builtin" => palette.builtin_type,
        "function.method" => palette.method,
        "label" => palette.label,
        "string.escape" => palette.escape,
        "punctuation.list_marker" => palette.list_marker,
        "markup.quote" => palette.quote,
        _ => palette.link,
    }
}

/// What a piece of the sample is, so the preview colors it as the editor colors that role.
#[derive(Clone, Copy)]
enum Tone {
    Plain,
    Quiet,
    Comment,
    Keyword,
    Type,
    Builtin,
    Function,
    Macro,
    Attribute,
    Str,
    Escape,
    Number,
    Field,
    Param,
    Var,
    Bracket(usize),
}

use Tone::*;

/// A short Rust sample: a line is its pieces, each with its tone.
const SAMPLE: &[&[(&str, Tone)]] = &[
    &[("// How long is a note?", Comment)],
    &[("#", Attribute), ("[", Bracket(0)), ("derive", Attribute), ("(", Bracket(1)), ("Debug", Type), (")", Bracket(1)), ("]", Bracket(0))],
    &[("struct", Keyword), (" ", Plain), ("Note", Type), (" ", Plain), ("{", Bracket(0)), (" ", Plain), ("title", Field), (": ", Quiet), ("String", Type), (", ", Quiet), ("words", Field), (": ", Quiet), ("usize", Builtin), (" ", Plain), ("}", Bracket(0))],
    &[],
    &[("fn", Keyword), (" ", Plain), ("long", Function), ("(", Bracket(0)), ("note", Param), (": &", Quiet), ("Note", Type), (", ", Quiet), ("limit", Param), (": ", Quiet), ("usize", Builtin), (")", Bracket(0)), (" -> ", Quiet), ("bool", Builtin), (" ", Plain), ("{", Bracket(0))],
    &[("    ", Plain), ("let", Keyword), (" ", Plain), ("total", Var), (" = ", Plain), ("note", Var), (".", Quiet), ("words", Field), (" + ", Plain), ("42", Number), (";", Quiet)],
    &[("    ", Plain), ("println!", Macro), ("(", Bracket(1)), ("\"{}", Str), ("\\n", Escape), ("\"", Str), (", ", Quiet), ("note", Var), (".", Quiet), ("title", Field), (")", Bracket(1)), (";", Quiet)],
    &[("    ", Plain), ("total", Var), (" > ", Plain), ("limit", Var)],
    &[("}", Bracket(0))],
];

/// What the preview's runs were drawn in, so picking a scheme eases them to the new scheme's colors (`motion::Fade` on `FADE_QUICK`,
/// interrupted from where it is drawn) instead of snapping. Other changes (a hex edit, an option, the appearance) land at once.
#[derive(Default)]
pub(crate) struct Glide {
    scheme: Option<Scheme>,
    /// The run colors the ease starts from and the ones it heads for, one for each piece of `SAMPLE` in order.
    from: Vec<Hsla>,
    to: Vec<Hsla>,
    fade: Option<Fade>,
}

impl Glide {
    /// The colors to draw at `now`, given the ones the settings call for.
    fn settle(&mut self, scheme: Scheme, targets: Vec<Hsla>, now: Instant) -> Vec<Hsla> {
        if self.scheme != Some(scheme) {
            let level = self.fade.map_or(1., |fade| fade.level(now));
            if self.scheme.is_some() && self.to.len() == targets.len() {
                self.from = self.from.iter().zip(&self.to).map(|(from, to)| tab_bar::mix(*from, *to, level)).collect();
                let mut fade = Fade::with(0., tokens::FADE_QUICK);
                fade.aim(1., now);
                self.fade = Some(fade);
            } else {
                self.from = targets.clone();
                self.fade = None;
            }
            self.scheme = Some(scheme);
        }
        if self.from.len() != targets.len() {
            self.from = targets.clone();
        }
        self.to = targets;
        let level = self.fade.map_or(1., |fade| fade.level(now));
        self.from.iter().zip(&self.to).map(|(from, to)| tab_bar::mix(*from, *to, level)).collect()
    }

    fn is_moving(&self, now: Instant) -> bool {
        self.fade.is_some_and(|fade| fade.is_moving(now))
    }
}

/// A card with the sample in the scheme, appearance and options now chosen, colored from the same palette, overrides and lifts the editor
/// uses (`theme::tuned`, the bracket and name palettes), so it matches the page.
fn preview(glide: &mut Glide, window: &mut Window, cx: &Cx) -> AnyElement {
    let settings = settings::get(cx);
    let colors = &settings.code_colors;
    let light = theme::is_light(cx);
    let palette = tokens::palette(colors.scheme, light);
    let ink = cx.theme().sidebar_foreground;
    let (plain, quiet) = (if light { rgb(tokens::TEXT_LIGHT).into() } else { tokens::ink(light, ink, tokens::ALPHA_PRIMARY) }, tokens::ink(light, ink, tokens::ALPHA_SECONDARY));
    let held = if light { &colors.light } else { &colors.dark };
    let comment: Hsla = rgb(held.get("comment").and_then(|hex| settings::parse_hex(hex)).unwrap_or(palette.comment)).into();
    let roles = colors.colored_roles;
    let palettes = theme::code_palettes(cx);
    let paint = |name: &str, hex: u32| theme::tuned(colors, light, name, hex);
    // A name's hue from its spelling, when semantic colors are on.
    let named = |text: &str| {
        let names = palettes.names.as_ref().filter(|names| !names.is_empty())?;
        let hash = text.bytes().fold(5381_usize, |hash, byte| hash.wrapping_mul(33) ^ usize::from(byte));
        Some(names[hash % names.len()])
    };
    let color_of = |tone: Tone, text: &str| -> Hsla {
        match tone {
            Plain => plain,
            Quiet => quiet,
            Comment => comment,
            Keyword => paint("keyword", palette.keyword),
            Type => paint("type", palette.type_),
            Builtin => if roles { paint("type.builtin", palette.builtin_type) } else { paint("type", palette.type_) },
            Function => paint("function", palette.function),
            Macro => paint("function.special", palette.macro_),
            Attribute => paint("attribute", palette.attribute),
            Str => paint("string", palette.string),
            Escape => if roles { paint("string.escape", palette.escape) } else { paint("string", palette.string) },
            Number => paint("number", palette.number),
            Field => if roles { paint("property", palette.field) } else { plain },
            Param => named(text).unwrap_or_else(|| if roles { paint("variable.parameter", palette.parameter) } else { plain }),
            Var => named(text).unwrap_or(plain),
            Bracket(depth) => palettes.brackets.as_ref().filter(|brackets| !brackets.is_empty()).map_or(quiet, |brackets| brackets[depth % brackets.len()]),
        }
    };
    let look = controls::look();
    let editor = &settings.editor;
    let family = editor.drawn_family(cx).unwrap_or_else(|| cx.theme().mono_font_family.clone());
    let targets: Vec<Hsla> = SAMPLE.iter().flat_map(|pieces| pieces.iter()).map(|(piece, tone)| color_of(*tone, piece)).collect();
    let now = motion::frame_now(window.window_handle().window_id());
    let shown = glide.settle(colors.scheme, targets, now);
    if glide.is_moving(now) {
        window.request_animation_frame();
    }
    let mut next = shown.into_iter();
    let lines: Vec<AnyElement> = SAMPLE
        .iter()
        .map(|pieces| {
            let mut text = String::new();
            let mut runs: Vec<(Range<usize>, HighlightStyle)> = Vec::with_capacity(pieces.len());
            for (piece, _) in pieces.iter() {
                let start = text.len();
                text.push_str(piece);
                runs.push((start..text.len(), HighlightStyle { color: next.next(), ..Default::default() }));
            }
            let line = div().h(px(tokens::SETTINGS_PREVIEW_LINE)).whitespace_nowrap();
            if text.is_empty() { line.into_any_element() } else { line.child(StyledText::new(text).with_highlights(runs)).into_any_element() }
        })
        .collect();
    div()
        .flex()
        .flex_col()
        .flex_none()
        .gap(px(tokens::SETTINGS_HEADER_GAP))
        .child(
            div()
                .h(px(tokens::SETTINGS_HEADER_HEIGHT))
                .px(px(tokens::SETTINGS_GROUP_INSET + 1.))
                .flex()
                .items_center()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(look.text)
                .child("Preview"),
        )
        .child(
            div()
                .rounded(px(tokens::SETTINGS_GROUP_RADIUS))
                .bg(tokens::chrome_ground(light))
                .border_1()
                .border_color(look.field_edge)
                .p(px(tokens::SETTINGS_PREVIEW_PAD))
                .overflow_hidden()
                .flex()
                .flex_col()
                .font_family(family)
                .text_size(px(tokens::SETTINGS_PREVIEW_SIZE))
                .line_height(px(tokens::SETTINGS_PREVIEW_LINE))
                .font_weight(FontWeight(settings.text.weight))
                .text_color(plain)
                .children(lines),
        )
        .into_any_element()
}

// ---- Text ----

/// Puts the glyph settings on the renderer: it reads them each frame, and `thicken` is part of each glyph's cache key.
fn tune(cx: &mut App) {
    let text = &settings::get(cx).text;
    gpui_apple::text_tuning::set_thicken(text.thicken.min(4));
    gpui_apple::text_tuning::set_gamma(text.gamma);
    gpui_apple::text_tuning::set_contrast(text.contrast);
}

fn text(window: &mut Window, cx: &mut Cx) -> Vec<AnyElement> {
    let text = settings::get(cx).text.clone();
    let slider = |name: &'static str, label: &'static str, value: f64, range: (f64, f64), step: f64, shown: String, set: fn(&mut settings::TextSettings, f64)| {
        slider_row(
            Slider {
                name,
                label,
                value,
                range,
                step,
                shown,
                set: Rc::new(move |value, cx| {
                    write(cx, move |settings| set(&mut settings.text, value));
                    tune(cx);
                }),
            },
            window,
        )
    };
    let usual = settings::TextSettings::default();
    vec![
        section(
            None,
            vec![slider("text-weight", "Weight", f64::from(text.weight), (100., 900.), 10., format!("{:.0}", text.weight), |text, value| text.weight = value as f32)],
            Some("100 is thin and 900 is black. It sets the weight of all the text.".into()),
        ),
        section(
            Some("Glyphs".into()),
            vec![
                slider("text-thicken", "Thicken", f64::from(text.thicken), (0., 4.), 1., if text.thicken == 0 { "Off".into() } else { text.thicken.min(4).to_string() }, |text, value| text.thicken = value as u8),
                slider("text-gamma", "Gamma", f64::from(text.gamma), (0.2, 3.), 0.05, format!("{:.2}", text.gamma), |text, value| text.gamma = value as f32),
                slider("text-contrast", "Contrast", f64::from(text.contrast), (0., 1.), 0.05, format!("{:.2}", text.contrast), |text, value| text.contrast = value as f32),
            ],
            Some("Thicken makes glyphs bolder, 0 is off. Gamma 1 and contrast 0 leave glyphs as the Mac draws them.".into()),
        ),
        section(
            None,
            vec![row(
                "Reset text",
                Some("Back to weight 500, thicken 4, gamma 1 and contrast 0.".into()),
                {
                    let weight = (usual.weight, usual.thicken, usual.gamma, usual.contrast);
                    button("text-reset", "Reset").on_click(move |_, _, cx| {
                        write(cx, move |settings| {
                            (settings.text.weight, settings.text.thicken, settings.text.gamma, settings.text.contrast) = weight;
                        });
                        tune(cx);
                    })
                },
            )
            .into_any_element()],
            None,
        ),
    ]
}
