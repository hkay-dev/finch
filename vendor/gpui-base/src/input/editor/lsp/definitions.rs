use anyhow::Result;
use gpui::{
    App, Context, HighlightStyle, Hitbox, MouseDownEvent, Task, UnderlineStyle, Window, px,
};
use ropey::Rope;
use std::{ops::Range, rc::Rc};

use crate::input::{EditorMode, GoToDefinition, InputBaseState, RopeExt};

/// Definition provider
///
/// https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#textDocument_definition
pub trait DefinitionProvider {
    /// textDocument/definition
    ///
    /// https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#textDocument_definition
    fn definitions(
        &self,
        _text: &Rope,
        _offset: usize,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Task<Result<Vec<lsp_types::LocationLink>>>;
}

#[derive(Clone, Default)]
pub(crate) struct HoverDefinition {
    /// The range of the symbol that triggered the hover.
    symbol_range: Range<usize>,
    pub(crate) locations: Rc<Vec<lsp_types::LocationLink>>,
    /// Where the Cmd-hovered link goes (a URL or a Markdown link's target), when the pointer is
    /// on one instead of on a symbol. It shares the range, the underline and the hand with
    /// `locations`.
    link: Option<Rc<str>>,
    /// The last compiler or stack-trace location the app said leads nowhere, by range and text:
    /// the same token under the pointer again is not asked about twice. Kept across `clear`.
    miss: Option<(Range<usize>, Rc<str>)>,
    /// The range of a location the app is being asked about; an answer for any other is stale.
    pending: Option<Range<usize>>,
}

impl HoverDefinition {
    pub(crate) fn update(
        &mut self,
        symbol_range: Range<usize>,
        locations: Vec<lsp_types::LocationLink>,
    ) {
        self.clear();
        self.symbol_range = symbol_range;
        self.locations = Rc::new(locations);
    }

    fn update_link(&mut self, range: Range<usize>, target: &str) {
        self.clear();
        self.symbol_range = range;
        self.link = Some(target.into());
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.locations.is_empty() && self.link.is_none()
    }

    pub(crate) fn clear(&mut self) {
        self.symbol_range = 0..0;
        self.locations = Rc::new(vec![]);
        self.link = None;
        self.pending = None;
    }

    pub(crate) fn is_same(&self, offset: usize) -> bool {
        self.symbol_range.contains(&offset)
    }
}

/// How the host opens a link that is not a web address: a path (relative to the open file, or
/// absolute) or a `file:` URL. The app sets it once with [`EditorState::set_link_opener`].
struct LinkOpener(Rc<dyn Fn(&str, &mut Window, &mut App)>);

impl gpui::Global for LinkOpener {}

/// How the host checks and opens a compiler or stack-trace location (`path:line[:col]`): the
/// check runs once per token Cmd-hovered, and a location it refuses never underlines. The app
/// sets both with [`EditorState::set_location_handlers`].
struct LocationHandlers {
    check: Rc<dyn Fn(&str, &mut Window, &mut App) -> bool>,
    open: Rc<dyn Fn(&str, &mut Window, &mut App) -> bool>,
}

impl gpui::Global for LocationHandlers {}

/// What the pointer is on: a link whose target the host opens, or a `path:line[:col]` location.
enum Link {
    Target(String),
    Location(String),
}

/// The longest line searched for a link: a minified file's one huge line is not text to scan on
/// every mouse move.
const LINK_LINE_LIMIT: usize = 20_000;

/// The link on `line` that byte `col` touches: its byte range and the byte range of its target.
/// A Markdown link `[text](target)` counts from `[` to `)`, a bare `http`, `https`, `mailto` or
/// `file` address from its scheme to its last character. A column at the very end of the line
/// still touches a link that ends there, so a click on its last letter's right half opens it.
fn link_in_line(line: &str, col: usize) -> Option<(Range<usize>, Range<usize>)> {
    let touches = |range: &Range<usize>| {
        range.contains(&col) || (col == range.end && range.end == line.len())
    };
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(open) = line[from..].find('[').map(|i| i + from) {
        if open > col {
            break;
        }
        from = open + 1;
        if let Some((end, target)) = markdown_link(line, open) {
            let range = open..end;
            if touches(&range) {
                return Some((range, target));
            }
        }
    }
    for (start, _) in line.char_indices() {
        if start > col {
            break;
        }
        let Some(scheme) = ["https://", "http://", "mailto:", "file://"]
            .into_iter()
            .find(|scheme| {
                bytes
                    .get(start..start + scheme.len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(scheme.as_bytes()))
            })
        else {
            continue;
        };
        let before = line[..start].chars().next_back();
        if before.is_some_and(|c| c.is_alphanumeric()) {
            continue;
        }
        let end = bare_link_end(line, start, start + scheme.len());
        if end > start + scheme.len() && touches(&(start..end)) {
            return Some((start..end, start..end));
        }
    }
    None
}

/// Where a bare address that starts at `start` ends: at whitespace or a quote or angle bracket,
/// less trailing punctuation and a closing bracket nothing in the address opened.
fn bare_link_end(line: &str, start: usize, body: usize) -> usize {
    let mut end = line.len();
    for (i, c) in line[body..].char_indices() {
        if c.is_whitespace() || matches!(c, '<' | '>' | '"' | '`') {
            end = body + i;
            break;
        }
    }
    loop {
        let address = &line[start..end];
        let Some(last) = address.chars().next_back() else { break };
        let opens = |open: char| address.matches(open).count();
        let closes = |close: char| address.matches(close).count();
        let trim = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*' | '_' => true,
            ')' => closes(')') > opens('('),
            ']' => closes(']') > opens('['),
            _ => false,
        };
        if !trim {
            break;
        }
        end -= last.len_utf8();
    }
    end
}

/// The Markdown link whose `[` sits at byte `open`: where its `)` ends and the byte range of its
/// target, or `None` when the text there is not `[text](target)` (an optional `"title"` after
/// the target is allowed; parentheses inside the target must balance).
fn markdown_link(line: &str, open: usize) -> Option<(usize, Range<usize>)> {
    let bytes = line.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    let close = loop {
        match *bytes.get(i)? {
            b'\\' => i += 1,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    break i;
                }
            }
            _ => {}
        }
        i += 1;
    };
    if bytes.get(close + 1) != Some(&b'(') {
        return None;
    }
    let mut i = close + 2;
    while bytes.get(i).is_some_and(|b| b.is_ascii_whitespace()) {
        i += 1;
    }
    let (target, mut i) = if bytes.get(i) == Some(&b'<') {
        let end = line[i + 1..].find('>')? + i + 1;
        (i + 1..end, end + 1)
    } else {
        let start = i;
        let mut depth = 0usize;
        while let Some(&b) = bytes.get(i) {
            match b {
                b'\\' => i += 1,
                b'(' => depth += 1,
                b')' if depth == 0 => break,
                b')' => depth -= 1,
                _ if b.is_ascii_whitespace() => break,
                _ => {}
            }
            i += 1;
        }
        (start..i.min(line.len()), i)
    };
    while bytes.get(i).is_some_and(|b| b.is_ascii_whitespace()) {
        i += 1;
    }
    if matches!(bytes.get(i), Some(b'"' | b'\'')) {
        let quote = bytes[i];
        i += 1 + line[i + 1..].bytes().position(|b| b == quote)? + 1;
        while bytes.get(i).is_some_and(|b| b.is_ascii_whitespace()) {
            i += 1;
        }
    }
    (bytes.get(i) == Some(&b')') && !target.is_empty()).then(|| (i + 1, target))
}

/// Characters that end a path in compiler and stack-trace output.
fn is_location_break(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | ',' | ';')
}

/// A line or column number: digits only, at least 1.
fn location_number(text: &str) -> Option<u32> {
    if text.is_empty() || text.len() > 9 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().filter(|&n| n > 0)
}

/// Whether `path` could be a file path: it has a `/` or an extension dot, and a letter or digit.
fn plausible_path(path: &str) -> bool {
    (path.contains('/') || path.contains('.'))
        && !path.ends_with('/')
        && path.chars().any(char::is_alphanumeric)
}

fn location_text(path: &str, line: u32, column: Option<u32>) -> String {
    match column {
        Some(column) => format!("{path}:{line}:{column}"),
        None => format!("{path}:{line}"),
    }
}

/// The location in the token `start..end` of `line`: `path:line`, `path:line:col` (rustc, Go,
/// Node, gcc; a trailing `:` or `.` stays out) or `path(line)` and `path(line,col)`. Its byte
/// range and the normalized `path:line[:col]`.
fn token_location(line: &str, start: usize, end: usize) -> Option<(Range<usize>, String)> {
    let token = &line[start..end];
    if line[end..].starts_with('(') {
        let inner = &line[end + 1..];
        let close = inner.find(')')?;
        let (number, column) = match inner[..close].split_once(',') {
            Some((number, column)) => (number, Some(column)),
            None => (&inner[..close], None),
        };
        let number = location_number(number.trim())?;
        let column = match column {
            Some(column) => Some(location_number(column.trim())?),
            None => None,
        };
        return plausible_path(token).then(|| (start..end + 1 + close + 1, location_text(token, number, column)));
    }
    let trimmed = token.trim_end_matches([':', '.']);
    let (rest, last) = trimmed.rsplit_once(':')?;
    let last = location_number(last)?;
    let (path, number, column) = rest
        .rsplit_once(':')
        .and_then(|(path, mid)| Some((path, location_number(mid)?)))
        .map_or((rest, last, None), |(path, mid)| (path, mid, Some(last)));
    plausible_path(path).then(|| (start..start + trimmed.len(), location_text(path, number, column)))
}

/// Python's `File "x.py", line 9`: the path through the line number, and `x.py:9`.
fn python_location(line: &str, col: usize) -> Option<(Range<usize>, String)> {
    const HEAD: &str = "File \"";
    const MID: &str = ", line ";
    let mut from = 0;
    while let Some(at) = line[from..].find(HEAD) {
        let path_start = from + at + HEAD.len();
        from = path_start;
        let Some(path_end) = line[path_start..].find('"').map(|i| path_start + i) else { break };
        let Some(after) = line[path_end + 1..].strip_prefix(MID) else { continue };
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        let Some(number) = location_number(&after[..digits]) else { continue };
        let end = path_end + 1 + MID.len() + digits;
        if path_start < path_end && (path_start..end).contains(&col) || (col == end && end == line.len()) {
            return Some((path_start..end, location_text(&line[path_start..path_end], number, None)));
        }
    }
    None
}

/// The compiler or stack-trace location on `line` that byte `col` touches: its byte range and
/// `path:line[:col]`. Whether the file exists is for the host to say.
fn location_in_line(line: &str, col: usize) -> Option<(Range<usize>, String)> {
    if let Some(found) = python_location(line, col) {
        return Some(found);
    }
    let mut from = 0;
    while let Some(skip) = line[from..].find(|c| !is_location_break(c)) {
        let start = from + skip;
        if start > col {
            break;
        }
        let end = line[start..].find(is_location_break).map_or(line.len(), |i| start + i);
        from = end;
        if let Some((range, text)) = token_location(line, start, end)
            && (range.contains(&col) || (col == range.end && range.end == line.len()))
        {
            return Some((range, text));
        }
    }
    None
}

impl InputBaseState<EditorMode> {
    /// Sets how a Cmd-click on a compiler or stack-trace location (`src/main.rs:42:7`) works: the
    /// first closure says whether the location leads to a file (asked once per token under the
    /// pointer), the second opens it and says whether it did. Without them such text is no link.
    pub fn set_location_handlers(
        cx: &mut App,
        check: impl Fn(&str, &mut Window, &mut App) -> bool + 'static,
        open: impl Fn(&str, &mut Window, &mut App) -> bool + 'static,
    ) {
        cx.set_global(LocationHandlers { check: Rc::new(check), open: Rc::new(open) });
    }

    /// Sets how a Cmd-click on a link that is not a web address opens it (a path or `file:`
    /// URL; the app resolves a relative path against the open file). Without one, only web
    /// links open.
    pub fn set_link_opener(
        cx: &mut App,
        opener: impl Fn(&str, &mut Window, &mut App) + 'static,
    ) {
        cx.set_global(LinkOpener(Rc::new(opener)));
    }

    /// The link at `offset`: its range in the text and what it is. The line is borrowed from the
    /// rope when it sits in one chunk; the target is copied out only here.
    fn link_at(&self, offset: usize, cx: &App) -> Option<(Range<usize>, Link)> {
        let row = self.text.offset_to_point(offset).row;
        let start = self.text.line_start_offset(row);
        let end = self.text.line_end_offset(row);
        if end - start > LINK_LINE_LIMIT {
            return None;
        }
        let slice = self.text.slice(start..end);
        let one: Option<&str> = (&slice).into();
        let owned;
        let line = match one {
            Some(line) => line,
            None => {
                owned = slice.to_string();
                &owned
            }
        };
        let line = line.trim_end_matches(['\r', '\n']);
        let col = offset.checked_sub(start)?;
        if let Some((range, target)) = link_in_line(line, col) {
            return Some((start + range.start..start + range.end, Link::Target(line[target].to_string())));
        }
        if !cx.has_global::<LocationHandlers>() {
            return None;
        }
        let (range, location) = location_in_line(line, col)?;
        Some((start + range.start..start + range.end, Link::Location(location)))
    }

    /// Opens a link: web addresses in the system's browser or mail app, anything else through
    /// the app's opener.
    fn open_link(target: &str, window: &mut Window, cx: &mut App) {
        if let Some(opener) = cx.try_global::<LinkOpener>().map(|opener| opener.0.clone()) {
            opener(target, window, cx);
            return;
        }
        let web = ["http://", "https://", "mailto:"]
            .iter()
            .any(|scheme| target.get(..scheme.len()).is_some_and(|head| head.eq_ignore_ascii_case(scheme)));
        if web {
            cx.open_url(target);
        }
    }

    /// Cmd-hover on a link: marks it for the underline and the pointing hand. True when the
    /// pointer is on one. A location is asked about once (the answer comes a moment later, off
    /// the mouse event, because the host reads the window), and a refused one is remembered.
    pub(crate) fn hover_link(&mut self, offset: usize, cx: &mut Context<Self>) -> bool {
        let hover = &self.extras.hover_definition;
        if hover.link.is_some() && hover.is_same(offset) {
            return true;
        }
        let found = self.link_at(offset, cx);
        let (range, target) = match found {
            Some((range, Link::Target(target))) => (range, target),
            Some((range, Link::Location(location))) => {
                let hover = &mut self.extras.hover_definition;
                if hover.link.is_some() && hover.symbol_range == range {
                    return true;
                }
                if hover.link.is_some() {
                    hover.clear();
                    cx.notify();
                }
                let refused = hover.miss.as_ref().is_some_and(|(seen, text)| *seen == range && **text == *location);
                if !refused && hover.pending.as_ref() != Some(&range) {
                    hover.pending = Some(range.clone());
                    self.check_location(range, location, cx);
                }
                return false;
            }
            None => {
                self.extras.hover_definition.pending = None;
                if self.extras.hover_definition.link.is_some() {
                    self.extras.hover_definition.clear();
                    cx.notify();
                }
                return false;
            }
        };
        let hover = &mut self.extras.hover_definition;
        if hover.link.is_none() || hover.symbol_range != range {
            hover.update_link(range, &target);
            cx.notify();
        }
        true
    }

    /// Asks the host whether `location` leads to a file, after the current event, and underlines
    /// it if so; a refusal goes in `miss`. An answer for a range no longer asked about is dropped.
    fn check_location(&mut self, range: Range<usize>, location: String, cx: &mut Context<Self>) {
        let Some(check) = cx.try_global::<LocationHandlers>().map(|handlers| handlers.check.clone()) else {
            return;
        };
        let window = self.auto_scroll_window;
        cx.spawn(async move |this, cx| {
            let exists = window
                .update(cx, |_, window, cx| window.modifiers().secondary(cx).then(|| check(&location, window, cx)))
                .ok()
                .flatten();
            this.update(cx, |this, cx| {
                let hover = &mut this.extras.hover_definition;
                if hover.pending.as_ref() != Some(&range) {
                    return;
                }
                hover.pending = None;
                // Cmd let go before the answer: nothing to show and nothing to remember.
                let Some(exists) = exists else { return };
                if exists {
                    hover.update_link(range, &location);
                } else {
                    hover.miss = Some((range, location.into()));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn handle_hover_definition(
        &mut self,
        offset: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(provider) = self.extras.lsp.definition_provider.clone() else {
            return;
        };

        if self.extras.hover_definition.is_same(offset) {
            return;
        }

        // Currently not implemented.
        let task = provider.definitions(&self.text, offset, window, cx);
        let mut symbol_range = self.text.word_range(offset).unwrap_or(offset..offset);
        let editor = cx.entity();
        self.extras.lsp._hover_task = cx.spawn_in(window, async move |_, cx| {
            let locations = task.await?;

            _ = editor.update(cx, |editor, cx| {
                if locations.is_empty() {
                    editor.extras.hover_definition.clear();
                } else {
                    if let Some(location) = locations.first() {
                        if let Some(range) = location.origin_selection_range {
                            let start = editor.text.position_to_offset(&range.start);
                            let end = editor.text.position_to_offset(&range.end);
                            symbol_range = start..end;
                        }
                    }

                    editor
                        .extras
                        .hover_definition
                        .update(symbol_range.clone(), locations.clone());
                }
                cx.notify();
            });

            Ok(())
        });
    }

    pub(crate) fn on_action_go_to_definition(
        &mut self,
        _: &GoToDefinition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let offset = self.cursor();
        // A keyboard action must also work before the symbol has been hovered.
        let Some(provider) = self.extras.lsp.definition_provider.clone() else {
            return;
        };
        let text = self.text.clone();
        let response = provider.definitions(&text, offset, window, cx);
        let blur_subscription = cx.on_blur(&self.focus_handle, window, |editor, _, _| {
            editor.extras.lsp._definition_task = Task::ready(Ok(()));
        });
        self.extras.lsp._definition_task = cx.spawn_in(window, async move |editor, cx| {
            let _blur_subscription = blur_subscription;
            let locations = response.await?;
            editor.update_in(cx, |editor, window, cx| {
                if editor.cursor() != offset
                    || editor.text != text
                    || !editor.focus_handle.is_focused(window)
                {
                    return;
                }
                if let Some(location) = locations.first() {
                    editor.go_to_definition(location, window, cx);
                }
            })?;
            Ok(())
        });
    }

    /// Return true if handled.
    pub(crate) fn handle_click_hover_definition(
        &mut self,
        event: &MouseDownEvent,
        offset: usize,
        window: &mut Window,
        cx: &mut Context<InputBaseState<EditorMode>>,
    ) -> bool {
        if !event.modifiers.secondary(cx) || event.modifiers.alt {
            return false;
        }

        // A link under the pointer wins over a symbol; the right button keeps its menu.
        if event.button == gpui::MouseButton::Left && event.click_count == 1 {
            match self.link_at(offset, cx) {
                Some((_, Link::Target(target))) => {
                    Self::open_link(&target, window, cx);
                    return true;
                }
                Some((_, Link::Location(location))) => {
                    let open = cx.try_global::<LocationHandlers>().map(|handlers| handlers.open.clone());
                    if open.is_some_and(|open| open(&location, window, cx)) {
                        return true;
                    }
                }
                None => {}
            }
        }

        if self.extras.hover_definition.is_empty() {
            return false;
        };
        if !self.extras.hover_definition.is_same(offset) {
            return false;
        }

        let Some(location) = self.extras.hover_definition.locations.first().cloned() else {
            return false;
        };

        self.go_to_definition(&location, window, cx);

        true
    }

    pub(crate) fn go_to_definition(
        &mut self,
        location: &lsp_types::LocationLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let external = location
            .target_uri
            .scheme()
            .map(|s| s.as_str() == "https" || s.as_str() == "http")
            == Some(true);

        // Give the host a chance to show the document first (window/showDocument),
        // e.g. to open virtual/external documents (stdlib docs) in an app window.
        if let Some(handler) = self.extras.lsp.show_document.clone() {
            let params = lsp_types::ShowDocumentParams {
                uri: location.target_uri.clone(),
                external: Some(external),
                take_focus: Some(true),
                selection: Some(location.target_selection_range),
            };
            if handler(&params, window, cx) {
                return;
            }
        }

        if external {
            cx.open_url(&location.target_uri.to_string());
        } else {
            // Move to the location.
            let target_range = location.target_selection_range;
            let start = self.text.position_to_offset(&target_range.start);
            let end = self.text.position_to_offset(&target_range.end);

            self.move_to(start, None, cx);
            self.select_to(end, cx);
        }
    }
}

/// These read the editor's own state; they sit here rather than on the element
/// because the element has nothing to do with the answer.
impl InputBaseState<EditorMode> {
    /// The range highlighted while Cmd-hovering a symbol, with its style.
    pub(crate) fn hover_definition_style(&self) -> Option<(Range<usize>, HighlightStyle)> {
        let editor = self;
        if editor.extras.hover_definition.is_empty() {
            return None;
        };

        let mut highlight_style = editor.editor_style.highlight_styles.style("link_text")?;

        highlight_style.underline = Some(UnderlineStyle {
            thickness: px(1.),
            ..UnderlineStyle::default()
        });

        Some((
            editor.extras.hover_definition.symbol_range.clone(),
            highlight_style,
        ))
    }

    /// The hitbox that makes a Cmd-hovered symbol clickable.
    pub(crate) fn hover_definition_hitbox(&self, window: &mut Window) -> Option<Hitbox> {
        let editor = self;
        if editor.extras.hover_definition.is_empty() {
            return None;
        };

        let Some(bounds) = editor.range_to_bounds(&editor.extras.hover_definition.symbol_range)
        else {
            return None;
        };

        Some(window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal))
    }
}

#[cfg(test)]
mod tests {
    use super::link_in_line as ranges;

    /// The link and its target text, as `link_at` reads them.
    fn link_in_line(line: &str, col: usize) -> Option<(std::ops::Range<usize>, String)> {
        let (range, target) = ranges(line, col)?;
        Some((range, line[target].to_string()))
    }

    fn at(line: &str, needle: &str, nth: usize) -> Option<(std::ops::Range<usize>, String)> {
        let col = line.find(needle).unwrap() + nth;
        link_in_line(line, col)
    }

    #[test]
    fn bare_url_under_an_offset() {
        let line = "see 中文 https://example.com/a?b=1, then";
        let (range, target) = at(line, "example", 2).unwrap();
        assert_eq!(target, "https://example.com/a?b=1");
        assert_eq!(&line[range], "https://example.com/a?b=1");
        assert_eq!(link_in_line(line, 0), None);
        assert_eq!(at(line, "then", 0), None);
        // The comma after the address is prose.
        assert_eq!(at(line, ",", 0), None);
    }

    #[test]
    fn mail_and_file_addresses() {
        assert_eq!(at("write mailto:a@b.co now", "a@b", 0).unwrap().1, "mailto:a@b.co");
        assert_eq!(at("<file:///tmp/x.md>", "tmp", 0).unwrap().1, "file:///tmp/x.md");
        assert_eq!(at("xhttps://nope.test", "nope", 0), None);
        assert_eq!(at("HTTP://UP.TEST/", "UP", 0).unwrap().1, "HTTP://UP.TEST/");
    }

    #[test]
    fn markdown_link_text_and_target_both_count() {
        let line = "a [the docs](docs/guide.md#top) b";
        let (range, target) = at(line, "docs]", 0).unwrap();
        assert_eq!(target, "docs/guide.md#top");
        assert_eq!(&line[range.clone()], "[the docs](docs/guide.md#top)");
        assert_eq!(at(line, "guide", 1).unwrap().0, range);
        assert_eq!(at(line, " b", 0), None);
        assert_eq!(link_in_line(line, 0), None);
    }

    #[test]
    fn nested_parentheses_stay_in_the_target() {
        let line = "[Rust](https://en.wikipedia.org/wiki/Rust_(language)) and (https://x.test/a_(b)).";
        let (range, target) = at(line, "Rust]", 0).unwrap();
        assert_eq!(target, "https://en.wikipedia.org/wiki/Rust_(language)");
        assert_eq!(&line[range].len(), &53);
        // A bare address in parentheses leaves the closing one out, but keeps a balanced pair.
        assert_eq!(at(line, "x.test", 0).unwrap().1, "https://x.test/a_(b)");
    }

    #[test]
    fn title_angle_brackets_and_images() {
        assert_eq!(at("[a](b.md \"The title\")", "b.md", 0).unwrap().1, "b.md");
        assert_eq!(at("[a](<my notes.md>)", "notes", 0).unwrap().1, "my notes.md");
        assert_eq!(at("![alt](pic.png)", "alt", 0).unwrap().1, "pic.png");
        assert_eq!(at("[not a link] (b)", "not", 0), None);
        assert_eq!(at("[a]()", "a", 0), None);
    }

    #[test]
    fn a_link_at_the_end_of_the_line() {
        let line = "end [go](x.md)";
        // The column after the last character still touches a link that ends the line.
        assert_eq!(link_in_line(line, line.len()).unwrap().1, "x.md");
        let bare = "see https://a.test";
        assert_eq!(link_in_line(bare, bare.len()).unwrap().1, "https://a.test");
        // ...but not one that is followed by more text.
        let inner = "[go](x.md) tail";
        assert_eq!(link_in_line(inner, "[go](x.md)".len()), None);
    }

    /// The location under the first character of `needle` and the text it underlines.
    fn place(line: &str, needle: &str) -> Option<(String, String)> {
        let (range, text) = super::location_in_line(line, line.find(needle).unwrap())?;
        Some((line[range].to_string(), text))
    }

    #[test]
    fn compiler_locations() {
        let rustc = "  --> src/main.rs:42:7";
        assert_eq!(place(rustc, "src"), Some(("src/main.rs:42:7".into(), "src/main.rs:42:7".into())));
        // The arrow is not part of it.
        assert_eq!(place(rustc, "-->"), None);
        assert_eq!(place("src/main.rs:42", "main"), Some(("src/main.rs:42".into(), "src/main.rs:42".into())));
        // A colon or full stop after it is prose.
        assert_eq!(place("main.c:3:9: error: x", "main").unwrap().0, "main.c:3:9");
        assert_eq!(place("see x.go:12:3.", "x.go").unwrap().1, "x.go:12:3");
        assert_eq!(place("(x.go:12:3)", "x.go").unwrap().1, "x.go:12:3");
    }

    #[test]
    fn stack_trace_locations() {
        let python = "  File \"app/run.py\", line 9, in <module>";
        assert_eq!(place(python, "app").unwrap(), ("app/run.py\", line 9".into(), "app/run.py:9".into()));
        let node = "    at foo (/abs/path/x.js:10:5)";
        assert_eq!(place(node, "/abs").unwrap(), ("/abs/path/x.js:10:5".into(), "/abs/path/x.js:10:5".into()));
        assert_eq!(place(node, "foo"), None);
        let msvc = "Program.cs(42,7): error CS0103";
        assert_eq!(place(msvc, "Program").unwrap(), ("Program.cs(42,7)".into(), "Program.cs:42:7".into()));
        assert_eq!(place("a/b.rs(8)", "a/b").unwrap().1, "a/b.rs:8");
    }

    #[test]
    fn not_locations() {
        // No path-looking name, no line, a zero line, a bare time.
        assert_eq!(place("foo:12", "foo"), None);
        assert_eq!(place("at 12:30:45 today", "12"), None);
        assert_eq!(place("main.rs:0", "main"), None);
        assert_eq!(place("main.rs: here", "main"), None);
        assert_eq!(place("call(3) and f(1,2)", "call"), None);
    }

    #[test]
    fn a_location_at_the_end_of_the_line() {
        let line = "see src/a.rs:5";
        assert_eq!(super::location_in_line(line, line.len()).unwrap().1, "src/a.rs:5");
        assert_eq!(super::location_in_line("src/a.rs:5 tail", "src/a.rs:5".len()), None);
    }
}
