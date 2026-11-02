//! Light or dark: which one the app is in, how it follows the Mac, and the fade between them.
//!
//! The `theme` setting is `system` (follow the Mac live), `light` or `dark`. Everything that
//! needs to know whether the colors are light asks `is_light`; text and glyph inks go through
//! `ink`, which applies the theme's gamma (`tokens::ink`).

use std::{rc::Rc, sync::Arc, time::Duration};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use crate::{
    settings::{self, ThemeSetting},
    tokens::{self, ALPHA_DIVIDER_LIT, ALPHA_HAIRLINE, ALPHA_PRIMARY, ALPHA_SECONDARY, THEME_FADE},
    window_look,
};

/// Whether the colors in use are the light ones: the one answer every light-or-dark check reads.
pub fn is_light(cx: &App) -> bool {
    !cx.theme().is_dark()
}

/// The ink for text and glyphs at `alpha`: the sidebar foreground through the theme's gamma.
pub fn ink(alpha: f32, cx: &App) -> Hsla {
    tokens::ink(is_light(cx), cx.theme().sidebar_foreground, alpha)
}

/// The inks of the git statuses a name can carry: roles of the theme, one per status.
#[derive(Clone, Copy, Default)]
pub struct GitInks {
    pub conflicted: Hsla,
    pub deleted: Hsla,
    pub modified: Hsla,
    pub added: Hsla,
}

/// The git inks of the current appearance, from the theme's magenta, red, yellow and green: calmed
/// by alpha in the dark, deepened toward black in the light so each reads at 4.5:1 or more.
pub fn git_inks(cx: &App) -> GitInks {
    let theme = cx.theme();
    let tone = |base: Hsla| {
        if is_light(cx) {
            crate::tab_bar::mix(base, hsla(0., 0., 0., 1.), tokens::GIT_DEEPEN_LIGHT)
        } else {
            base.opacity(tokens::ALPHA_GIT_DARK)
        }
    };
    GitInks { conflicted: tone(theme.magenta), deleted: tone(theme.red), modified: tone(theme.yellow), added: tone(theme.green) }
}

/// A hairline in `color` across its box's middle: `vertical` runs it top to bottom. It is one device
/// pixel, `1 / scale factor` points, laid on the device pixel grid so it never blurs across two.
pub fn hairline(color: Hsla, vertical: bool) -> impl IntoElement + Styled {
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let scale = window.scale_factor();
            let thick = px(1. / scale);
            let on_grid = |middle: Pixels| px(((f32::from(middle) * scale) - 0.5).round() / scale);
            let line = if vertical {
                Bounds::new(point(on_grid(bounds.center().x), bounds.origin.y), size(thick, bounds.size.height))
            } else {
                Bounds::new(point(bounds.origin.x, on_grid(bounds.center().y)), size(bounds.size.width, thick))
            };
            window.paint_quad(fill(line, color));
        },
    )
}

/// The appearance a window is pinned to: `None` while the setting follows the Mac.
pub fn pin(cx: &App) -> Option<bool> {
    match settings::get(cx).window.theme {
        ThemeSetting::System => None,
        ThemeSetting::Light => Some(true),
        ThemeSetting::Dark => Some(false),
    }
}

fn appearance_is_light(appearance: WindowAppearance) -> bool {
    matches!(appearance, WindowAppearance::Light | WindowAppearance::VibrantLight)
}

/// Whether the setting means light right now: the pinned one, or the Mac's.
fn wanted_light(cx: &App) -> bool {
    pin(cx).unwrap_or_else(|| appearance_is_light(cx.window_appearance()))
}

/// Sets the colors at once, ground included, and refreshes every window.
fn set(light: bool, cx: &mut App) {
    component::Theme::change(if light { component::ThemeMode::Light } else { component::ThemeMode::Dark }, None, cx);
    // The editor's ground is the chrome's, so the text area and the bars around it are one surface.
    let ground = tokens::chrome_ground(light);
    let styled = settings::get(cx).editor.styled_markdown;
    let colors = settings::get(cx).code_colors.clone();
    component::Theme::update(cx, |theme| {
        theme.background = ground;
        // Every rule and divider is the theme's own ink, not the kit's cool grey and ring blue: the
        // foreground at `ALPHA_HAIRLINE` at rest, and the lit ink while the pointer holds a handle.
        let ink = theme.sidebar_foreground;
        theme.border = tokens::ink(light, ink, ALPHA_HAIRLINE);
        theme.sidebar_border = theme.border;
        theme.drag_border = tokens::ink(light, ink, ALPHA_DIVIDER_LIT);
        // Plain code is the app's own text ink (Xcode's plain text at `ALPHA_PRIMARY`), the editor's foreground
        // (the kit hands the editor `theme.foreground`) and the ink fields and variables keep. On a light theme
        // the primary ink is the neutral dark grey `TEXT_LIGHT` itself.
        let plain = tokens::ink(light, ink, ALPHA_PRIMARY);
        theme.foreground = plain;
        let mut highlight = (*theme.highlight_theme).clone();
        highlight.style.editor_background = Some(ground);
        highlight.style.editor_foreground = Some(plain);
        // The current-line band: the foreground over the ground at the hover wash, not the kit's fixed grey.
        let band = if light { tokens::ALPHA_HOVER_LIGHT } else { tokens::ALPHA_HOVER };
        highlight.style.editor_active_line = Some(tokens::ink(light, ink, band));
        highlight.style.syntax = syntax(light, plain, tokens::ink(light, ink, ALPHA_SECONDARY), marks_ink(light, ink), styled, &colors);
        theme.highlight_theme = Arc::new(highlight);
    });
    store_palettes(cx);
}

fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// A role's color: the hex `name` is overridden to in `[code_colors.dark|light]`, else `hex` (the scheme's), lifted
/// when vivid. Run once when the theme is set, never per frame; the Settings window's preview reads it too.
pub fn tuned(colors: &settings::CodeColors, light: bool, name: &str, hex: u32) -> Hsla {
    let overrides = if light { &colors.light } else { &colors.dark };
    if let Some(over) = overrides.get(name).and_then(|text| settings::parse_hex(text)) {
        return color(over);
    }
    let color = color(hex);
    if colors.vivid { tokens::vivid(color, light, tokens::chrome_ground(light)) } else { color }
}

/// The code palette of the current appearance and scheme (`colors`): the scheme's hues (`tokens::palette`), lifted when
/// vivid, `plain` for what takes the text ink (variables, operators, and fields while `colored_roles` is off) and `quiet`
/// for punctuation, then the overrides of `[code_colors.dark|light]` by role name. It replaces the kit theme's
/// `syntax` block whole, so the JSON decides no role, and sets no weight but Markdown's bold, so no
/// role is bolder than the code font. `SyntaxColors` has no setters, so it is built the way the JSON builds it: from the
/// same keys, through serde; a role it has no field for (`variable.parameter`, `type.builtin`, `function.method`,
/// `markup.quote`) goes in its open map, by the capture's name.
/// With `colored_roles` on, fields, parameters, constants, enum variants, `self`, built-in types, methods, lifetimes
/// and labels, string escapes, Markdown list markers and quote marks each have a hue of their own; off, each takes the
/// role it had before (constants the number's, variants and built-in types the type's, `self` the keyword's, methods the
/// function's, escapes the string's, the markers the quiet ink) and fields, parameters, labels stay plain.
/// `marks` is the faded ink and `styled` the View > Styled Markdown option: on, Markdown's headings are semibold and its marks
/// (`#`, `**`, `_`, backticks, link brackets and addresses) take `marks`; off, they look as plain Markdown always did. Only
/// the `markup.*` roles change, and those are named only by the Markdown captures, so no other language moves.
fn syntax(light: bool, plain: Hsla, quiet: Hsla, marks: Hsla, styled: bool, colors: &settings::CodeColors) -> component::highlighter::SyntaxColors {
    use gpui_kit::private::serde_json::{from_value, json, to_value, Map, Value};
    let p = tokens::palette(colors.scheme, light);
    let hue = |name: &str, hex: u32| to_value(tuned(colors, light, name, hex)).expect("a color serializes");
    let style = |name: &str, hex: u32| json!({ "color": hue(name, hex) });
    let ink = |c: Hsla| json!({ "color": to_value(c).expect("a color serializes") });
    let keyword = style("keyword", p.keyword);
    let string = style("string", p.string);
    let number = style("number", p.number);
    let type_ = style("type", p.type_);
    let function = style("function", p.function);
    // Comments stay as the scheme gives them: quiet by design, never lifted.
    let comment = {
        let over = (if light { &colors.light } else { &colors.dark }).get("comment").and_then(|text| settings::parse_hex(text));
        json!({ "color": to_value(color(over.unwrap_or(p.comment))).expect("a color serializes") })
    };
    let link = hue("link_uri", p.link);
    let marks_value = to_value(marks).expect("a color serializes");
    // Headings stay body size (rows are one height, the wrapper measures one font); styled, they are semibold in the heading ink.
    let heading = if styled { json!({ "color": hue("keyword", p.keyword), "font_weight": 600 }) } else { keyword.clone() };
    let roles = colors.colored_roles;
    let or = |on: Option<Value>, off: &Value| on.filter(|_| roles).unwrap_or_else(|| off.clone());
    let mut map = Map::new();
    let mut put = |name: &str, value: Value| {
        map.insert(name.to_owned(), value);
    };
    put("attribute", style("attribute", p.attribute));
    put("boolean", keyword.clone());
    put("comment", comment.clone());
    put("comment.doc", comment);
    put("constant", or(Some(style("constant", p.constant)), &number));
    put("constructor", type_.clone());
    put("embedded", ink(plain));
    put("emphasis", json!({ "font_style": "italic" }));
    // Markdown's own bold keeps its weight: it carries the writer's meaning, not a role's look.
    put("emphasis.strong", json!({ "font_weight": 700 }));
    put("function", function.clone());
    put("function.definition", function.clone());
    put("function.special", style("function.special", p.macro_));
    put("keyword", keyword.clone());
    put("link_text", json!({ "color": link, "font_style": "normal" }));
    put("link_uri", json!({ "color": link, "font_style": "italic" }));
    put("number", number.clone());
    put("operator", ink(plain));
    put("property", or(Some(style("property", p.field)), &ink(plain)));
    put("punctuation", ink(quiet));
    put("punctuation.bracket", ink(quiet));
    put("punctuation.delimiter", ink(quiet));
    put("punctuation.list_marker", or(Some(style("punctuation.list_marker", p.list_marker)), &ink(quiet)));
    put("punctuation.special", ink(quiet));
    put("string", string.clone());
    put("string.escape", or(Some(style("string.escape", p.escape)), &string));
    put("string.regex", string.clone());
    put("string.special", string.clone());
    put("string.special.symbol", string.clone());
    put("tag", keyword.clone());
    put("text.literal", string.clone());
    put("text.code.span", string.clone());
    put("title", keyword.clone());
    put("type", type_.clone());
    put("type.interface", style("type.interface", p.declared_type));
    put("variable", ink(plain));
    put("variable.special", or(Some(style("variable.special", p.self_)), &keyword));
    put("variant", or(Some(style("variant", p.variant)), &type_));
    // Roles with no field of their own: the grammar's finer captures. Off, they are left out and fall back to the field role above.
    if roles {
        put("label", style("label", p.label));
        put("type.builtin", style("type.builtin", p.builtin_type));
        put("function.method", style("function.method", p.method));
        put("variable.parameter", style("variable.parameter", p.parameter));
        put("variable.member", style("variable.member", p.field));
        put("variable.builtin", style("variable.builtin", p.self_));
    }
    // Markdown's own roles.
    put("markup.heading", heading);
    put("markup.heading.marker", ink(if styled { marks } else { quiet }));
    put("markup.emphasis", json!({ "font_style": "italic" }));
    put("markup.strong", json!({ "font_weight": 700 }));
    put("markup.mark", ink(if styled { marks } else { quiet }));
    // Off, these three set no style (null), so the text under them keeps its own ink as before.
    put("markup.code.mark", if styled { ink(marks) } else { json!(null) });
    put("markup.bracket", if styled { ink(marks) } else { json!(null) });
    put("markup.paren", if styled { ink(marks) } else { json!(null) });
    put("markup.link.uri", json!({ "color": if styled { marks_value } else { link.clone() }, "font_style": "italic" }));
    put("markup.quote", or(Some(style("markup.quote", p.quote)), &ink(quiet)));
    // The overrides come last: a role named in the file takes that color and keeps its other style.
    for (name, text) in if light { &colors.light } else { &colors.dark } {
        let Some(hex) = settings::parse_hex(text) else { continue };
        let value = to_value(color(hex)).expect("a color serializes");
        match map.get_mut(name) {
            Some(Value::Object(object)) => {
                object.insert("color".to_owned(), value);
            }
            _ => {
                map.insert(name.clone(), json!({ "color": value }));
            }
        }
    }
    from_value(Value::Object(map)).expect("the palette keys are the highlight theme's own")
}

/// Called when View > Styled Markdown flips or the `[code_colors]` palette settings change: takes the roles they mean. A new highlight theme is a new
/// cache key, so every editor re-queries once on its next frame.
pub fn apply_syntax(cx: &mut App) {
    let light = is_light(cx);
    let styled = settings::get(cx).editor.styled_markdown;
    let colors = settings::get(cx).code_colors.clone();
    component::Theme::update(cx, |theme| {
        let plain = theme.foreground;
        let ink = theme.sidebar_foreground;
        let mut highlight = (*theme.highlight_theme).clone();
        highlight.style.syntax = syntax(light, plain, tokens::ink(light, ink, ALPHA_SECONDARY), marks_ink(light, ink), styled, &colors);
        theme.highlight_theme = Arc::new(highlight);
    });
    store_palettes(cx);
    cx.refresh_windows();
}

/// The ink of a faded mark: the tertiary ink laid over the ground, so it is one opaque color. A translucent text color
/// draws darker than its nominal value through the glyph contrast, and a mark must read as quiet.
fn marks_ink(light: bool, ink: Hsla) -> Hsla {
    tokens::chrome_ground(light).blend(tokens::ink(light, ink, tokens::ALPHA_TERTIARY))
}

/// The inks Markdown's marks have with Styled Markdown off, which the caret line shows them in: the quiet punctuation ink
/// (heading markers, emphasis), the code span's ink (backticks), the plain text ink (link and image brackets, parentheses
/// and `!`, which have no style of their own) and the link ink (addresses).
pub fn markdown_mark_inks(cx: &App) -> (Hsla, Hsla, Hsla, Hsla) {
    let light = is_light(cx);
    let colors = &settings::get(cx).code_colors;
    let p = tokens::palette(colors.scheme, light);
    (ink(ALPHA_SECONDARY, cx), tuned(colors, light, "string", p.string), cx.theme().foreground, tuned(colors, light, "link_uri", p.link))
}

/// The hues rainbow brackets take by depth: five from the current scheme and appearance (number, keyword, function,
/// string and declared type), lifted when vivid, so a bracket's color belongs to the page's own palette. Built when
/// asked; the editor keeps it until the theme or the setting changes.
pub fn bracket_palette(cx: &App) -> Rc<[Hsla]> {
    let light = is_light(cx);
    let colors = &settings::get(cx).code_colors;
    let p = tokens::palette(colors.scheme, light);
    [p.number, p.keyword, p.function, p.string, p.declared_type].iter().map(|hex| tuned(colors, light, "", *hex)).collect()
}

/// The hues semantic colors choose a name's color from: `SEMANTIC_HUES` evenly spaced hues, ordered so neighbors in
/// the list are far apart on the wheel, at a saturation and lightness for the current appearance, each moved until it
/// reads at `VIVID_CONTRAST` on the ground (vivid adds saturation). Built when asked; the editor keeps it until the
/// theme or the setting changes.
pub fn semantic_palette(cx: &App) -> Rc<[Hsla]> {
    let light = is_light(cx);
    let vivid = settings::get(cx).code_colors.vivid;
    let ground = tokens::chrome_ground(light);
    let (saturation, lightness) = if light { (0.62, 0.36) } else { (0.55, 0.72) };
    let saturation = if vivid { saturation + tokens::SEMANTIC_VIVID_SATURATION } else { saturation };
    (0..tokens::SEMANTIC_HUES)
        .map(|index| {
            let slot = (index * tokens::SEMANTIC_HUE_STRIDE) % tokens::SEMANTIC_HUES;
            let hue = (tokens::SEMANTIC_HUE_START + slot as f32 / tokens::SEMANTIC_HUES as f32).fract();
            tokens::readable(hsla(hue, saturation, lightness, 1.), light, ground)
        })
        .collect()
}

/// The rainbow-bracket and semantic palettes the `[code_colors]` settings ask for (`None`: that option is off), built
/// once per theme or setting change so every editor shares one `Rc` and a frame builds nothing.
#[derive(Default, Clone, PartialEq)]
pub struct CodePalettes {
    pub brackets: Option<Rc<[Hsla]>>,
    pub names: Option<Rc<[Hsla]>>,
}

impl Global for CodePalettes {}

/// The palettes in effect, for an editor being built.
pub fn code_palettes(cx: &App) -> CodePalettes {
    cx.try_global::<CodePalettes>().cloned().unwrap_or_default()
}

/// Builds the palettes the settings and appearance mean and, when they changed, hands them to every built editor
/// (deferred, so no editor is updated while the caller holds one).
pub fn store_palettes(cx: &mut App) {
    let colors = &settings::get(cx).code_colors;
    let (rainbow, semantic) = (colors.rainbow_brackets, colors.semantic);
    let palettes = CodePalettes {
        brackets: rainbow.then(|| bracket_palette(cx)),
        names: semantic.then(|| semantic_palette(cx)),
    };
    if cx.try_global::<CodePalettes>() == Some(&palettes) {
        return;
    }
    cx.set_global(palettes);
    cx.defer(crate::document::apply_code_palettes);
}

/// Called once at launch, before any window opens.
pub fn init(cx: &mut App) {
    window_look::follow_mac(pin(cx).is_none());
    let light = wanted_light(cx);
    set(light, cx);
}

/// Called when the setting changes: takes the colors it means, with the fade.
pub fn apply_setting(cx: &mut App) {
    window_look::follow_mac(pin(cx).is_none());
    let light = wanted_light(cx);
    switch(light, cx);
}

/// Called once when a window opens: while the setting follows the Mac, a change of the window's
/// appearance (the Mac's light or dark mode, the only thing that changes it then) switches the
/// colors.
pub fn watch(window: &mut Window) {
    window_look::warm_capture(window);
    window_look::watch_appearance();
    window
        .observe_window_appearance(|window, cx| {
            if settings::get(cx).window.theme != ThemeSetting::System {
                return;
            }
            let light = appearance_is_light(window.appearance());
            // The window is being updated now; the switch updates every window.
            cx.defer(move |cx| switch(light, cx));
        })
        .detach();
}

/// Takes the `light` or dark colors. Every window fades over `THEME_FADE`: its old picture goes
/// over it, the new colors are taken at once and drawn once underneath, and the picture fades out.
fn switch(light: bool, cx: &mut App) {
    if is_light(cx) == light {
        return;
    }
    let old = tokens::chrome_ground(!light).to_rgb();
    // Each window's picture, by its serial: the timer below takes off only its own.
    let windows: Vec<_> = cx
        .windows()
        .into_iter()
        .map(|handle| {
            let serial = cx
                .update_window(handle, |_, window, _| window_look::fade_begin(window, (old.r, old.g, old.b)))
                .ok()
                .flatten();
            (handle, serial)
        })
        .collect();
    set(light, cx);
    // A hidden or minimised window took no picture (no serial) and has nothing to fade.
    for (handle, serial) in &windows {
        let (handle, serial) = (*handle, *serial);
        cx.update_window(handle, |_, window, _| {
            window.refresh();
            let Some(serial) = serial else { return };
            window.on_next_frame(move |window, cx| {
                window_look::fade_run(window, THEME_FADE);
                // The picture goes when its fade ends, one frame after the length it runs over.
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(Duration::from_secs_f32(THEME_FADE + FADE_END_SLACK)).await;
                    cx.update_window(handle, |_, window, _| window_look::fade_end(window, serial)).ok();
                })
                .detach();
            });
        })
        .ok();
    }
    // A window that never draws again (covered, closed) still gets its picture taken off.
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_secs_f32(THEME_FADE + 0.5)).await;
        for (handle, serial) in windows {
            if let Some(serial) = serial {
                cx.update_window(handle, |_, window, _| window_look::fade_end(window, serial)).ok();
            }
        }
    })
    .detach();
}

/// How long past `THEME_FADE` the picture stays before it is taken off: one frame at 60 Hz.
const FADE_END_SLACK: f32 = 1. / 60.;
