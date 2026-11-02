//! Where the caret is in a JSON, TOML or YAML file, as a trail (finch#7skw): `server > ports > 0`.
//! Read from the tree the highlighter already holds (`EditorState::with_current_syntax_tree`),
//! walking from the node at the caret up to the root: O(depth), plus the siblings counted for an
//! array index. It never parses, and it answers only while the tree is current; otherwise the
//! last good trail stays.
//!
//! What the status line calls (see `TrailWatch`): `watch.refresh(&editor, lang, cx)` on a caret
//! move, an edit and a tab switch, never per frame. `watch.trail()` is what to show: `None`
//! hides the field. The field's label is `trail.short_label(n)`; a click calls `copy(&trail, false, cx)` (the dotted path, `server.ports[0]`) and
//! an Option-click `copy(&trail, true, cx)` (a jq filter, `.server.ports[0]`).

use std::{cell::RefCell, rc::Rc, time::Duration};

use gpui_kit::component::input::EditorState;
use gpui_kit::*;
use tree_sitter::{Node, Tree};

use crate::{config_check::{Lang, Text, when_parsed}, language::MAX_HIGHLIGHT_BYTES};

/// How long a caret move or an edit waits before the trail is looked up again.
const LOOK_AFTER: Duration = Duration::from_millis(80);
/// Most siblings counted to number an array element or an array of tables; past this the trail
/// is dropped rather than walk a huge array.
const MAX_SIBLINGS: usize = 200_000;

/// One step of a trail: a key, or an index in an array.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    Key(String),
    Index(usize),
}

/// The caret's place in the file, outermost first. Never empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trail {
    pub segments: Vec<Segment>,
}

impl Trail {
    /// The words of the trail: `server > ports > 0` (the status line shows `short_label`).
    #[cfg(test)]
    pub fn label(&self) -> String {
        let parts: Vec<String> = self.segments.iter().map(Segment::words).collect();
        parts.join(" > ")
    }

    /// `label` for a narrow field: the outer segments give way to "..." first, so the innermost
    /// stay (at most `max` characters when the last segment alone fits).
    pub fn short_label(&self, max: usize) -> String {
        let parts: Vec<String> = self.segments.iter().map(Segment::words).collect();
        let mut start = 0;
        let width = |from: usize| parts[from..].iter().map(|part| part.chars().count()).sum::<usize>() + 3 * (parts.len() - from - 1) + if from > 0 { 4 } else { 0 };
        while start + 1 < parts.len() && width(start) > max {
            start += 1;
        }
        let tail = parts[start..].join(" > ");
        if start > 0 { format!("\u{2026} > {tail}") } else { tail }
    }

    /// The path as code: `server.ports[0]`; a key that isn't plain letters, digits, `_` and `-`
    /// is quoted: `server."my key"`.
    pub fn dotted(&self) -> String {
        let mut out = String::new();
        for segment in &self.segments {
            match segment {
                Segment::Key(key) => {
                    if !out.is_empty() {
                        out.push('.');
                    }
                    if is_plain(key, true) { out.push_str(key) } else { out.push_str(&quoted(key)) }
                }
                Segment::Index(index) => out.push_str(&format!("[{index}]")),
            }
        }
        out
    }

    /// The path as a jq filter: `.server.ports[0]`; a key that isn't letters, digits and `_` is
    /// quoted: `.server."my key"`.
    pub fn jq(&self) -> String {
        let mut out = String::new();
        for segment in &self.segments {
            match segment {
                Segment::Key(key) => {
                    out.push('.');
                    if is_plain(key, false) { out.push_str(key) } else { out.push_str(&quoted(key)) }
                }
                Segment::Index(index) => out.push_str(&format!("[{index}]")),
            }
        }
        out
    }
}

impl Segment {
    fn words(&self) -> String {
        match self {
            Segment::Key(key) => key.clone(),
            Segment::Index(index) => index.to_string(),
        }
    }
}

/// Whether `key` can stand bare: starts with a letter or `_`, then letters, digits, `_` (and `-`
/// where `dash`).
fn is_plain(key: &str, dash: bool) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || (dash && c == '-'))
}

fn quoted(key: &str) -> String {
    let mut out = String::from("\"");
    for c in key.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The trail at byte `offset`, or `None` at the top level of the file (no key or element around
/// it) or past `MAX_SIBLINGS`.
pub fn trail_at<T: Text + ?Sized>(tree: &Tree, text: &T, offset: usize, lang: Lang) -> Option<Trail> {
    let root = tree.root_node();
    let mut node = root.descendant_for_byte_range(offset, offset)?;
    // Between two tokens tree-sitter answers with the node to the right, and after a value (the
    // caret just after `1` in `a = 1`) with the container around it; the value on the left is the
    // one being edited.
    if offset > 0 && (node.start_byte() == offset || is_container(node)) {
        if let Some(left) = root.descendant_for_byte_range(offset - 1, offset - 1) {
            if left.end_byte() == offset && left.is_named() && !is_container(left) {
                node = left;
            }
        }
    }
    // Segments innermost first; reversed at the end.
    let mut up: Vec<Segment> = Vec::new();
    let mut current = Some(node);
    while let Some(n) = current {
        match lang {
            Lang::Json => json_step(n, text, &mut up)?,
            Lang::Toml => toml_step(n, text, &mut up)?,
            Lang::Yaml => yaml_step(n, text, &mut up)?,
        }
        current = n.parent();
    }
    if up.is_empty() {
        return None;
    }
    up.reverse();
    Some(Trail { segments: up })
}

fn is_container(node: Node) -> bool {
    matches!(node.kind(), "object" | "array" | "document" | "table" | "table_array_element" | "inline_table" | "block_mapping" | "block_sequence" | "flow_mapping" | "flow_sequence" | "stream")
}

/// The position of `node` among its parent's named, non-comment children of one of `kinds` (any
/// when empty). `None` past the sibling cap.
fn index_among(node: Node, kinds: &[&str]) -> Option<usize> {
    count_before(node, |sibling| sibling.kind() != "comment" && (kinds.is_empty() || kinds.contains(&sibling.kind())))
}

/// How many of the named siblings before `node` satisfy `counts`, in one pass over the parent's
/// children (`prev_named_sibling` walks from the start each time). `None` past the sibling cap.
fn count_before(node: Node, counts: impl Fn(Node) -> bool) -> Option<usize> {
    let parent = node.parent()?;
    let mut cursor = parent.walk();
    let mut count = 0;
    let mut seen = 0;
    for sibling in parent.named_children(&mut cursor) {
        if sibling.id() == node.id() {
            return Some(count);
        }
        seen += 1;
        if seen > MAX_SIBLINGS {
            return None;
        }
        if counts(sibling) {
            count += 1;
        }
    }
    Some(count)
}

fn json_step<T: Text + ?Sized>(node: Node, text: &T, up: &mut Vec<Segment>) -> Option<()> {
    if node.kind() == "pair" {
        if let Some(key) = node.child_by_field_name("key") {
            up.push(Segment::Key(string_value(&text.slice(key.byte_range()))));
        }
    }
    if node.is_named() && node.kind() != "comment" && node.parent().is_some_and(|parent| parent.kind() == "array") {
        up.push(Segment::Index(index_among(node, &[])?));
    }
    Some(())
}

fn toml_step<T: Text + ?Sized>(node: Node, text: &T, up: &mut Vec<Segment>) -> Option<()> {
    match node.kind() {
        "pair" => {
            let key = node.named_child(0)?;
            push_toml_key(key, text, up);
        }
        "table" | "table_array_element" => {
            let key = node.named_child(0)?;
            if node.kind() == "table_array_element" {
                // Which `[[name]]` of the file this is: the earlier ones with the same name.
                let parts = toml_key_parts(key, text);
                let count = count_before(node, |sibling| {
                    sibling.kind() == "table_array_element" && sibling.named_child(0).is_some_and(|k| toml_key_parts(k, text) == parts)
                })?;
                up.push(Segment::Index(count));
            }
            push_toml_key(key, text, up);
        }
        _ => {}
    }
    if node.is_named() && node.kind() != "comment" && node.parent().is_some_and(|parent| parent.kind() == "array") {
        up.push(Segment::Index(index_among(node, &[])?));
    }
    Some(())
}

/// A TOML key (bare, quoted or dotted) as innermost-first segments, appended to `up`.
fn push_toml_key<T: Text + ?Sized>(key: Node, text: &T, up: &mut Vec<Segment>) {
    for part in toml_key_parts(key, text).into_iter().rev() {
        up.push(Segment::Key(part));
    }
}

fn toml_key_parts<T: Text + ?Sized>(key: Node, text: &T) -> Vec<String> {
    let one = |node: Node| string_value(&text.slice(node.byte_range()));
    // A dotted key nests to the left: `a.b.c` is `((a b) c)`.
    if key.kind() == "dotted_key" {
        let mut cursor = key.walk();
        key.named_children(&mut cursor).filter(|part| part.kind() != "comment").flat_map(|part| toml_key_parts(part, text)).collect()
    } else {
        vec![one(key)]
    }
}

fn yaml_step<T: Text + ?Sized>(node: Node, text: &T, up: &mut Vec<Segment>) -> Option<()> {
    match node.kind() {
        "block_mapping_pair" | "flow_pair" => {
            if let Some(key) = node.child_by_field_name("key") {
                up.push(Segment::Key(string_value(text.slice(key.byte_range()).trim())));
            }
        }
        "block_sequence_item" => up.push(Segment::Index(index_among(node, &["block_sequence_item"])?)),
        _ => {}
    }
    if node.is_named() && node.kind() != "comment" && node.parent().is_some_and(|parent| parent.kind() == "flow_sequence") {
        up.push(Segment::Index(index_among(node, &[])?));
    }
    Some(())
}

/// A key as the file means it: the text inside a double or single quoted string, with the
/// escapes of the common cases read; anything else as written.
fn string_value(raw: &str) -> String {
    let raw = raw.trim();
    let bytes = raw.as_bytes();
    if raw.len() >= 2 && bytes[0] == b'"' && bytes[raw.len() - 1] == b'"' {
        let inner = &raw[1..raw.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        Some(c) => out.push(c),
                        None => {
                            out.push_str("\\u");
                            out.push_str(&hex);
                        }
                    }
                }
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        }
        out
    } else if raw.len() >= 2 && bytes[0] == b'\'' && bytes[raw.len() - 1] == b'\'' {
        raw[1..raw.len() - 1].replace("''", "'")
    } else {
        raw.to_string()
    }
}

/// A click on the trail copies the dotted path; an Option-click (`jq`) the jq filter.
pub fn copy(trail: &Trail, jq: bool, cx: &mut App) {
    let text = if jq { trail.jq() } else { trail.dotted() };
    cx.write_to_clipboard(ClipboardItem::new_string(text));
}

/// One editor's trail: the last good answer, and a look again when the tree is behind.
#[derive(Default)]
pub struct TrailWatch {
    shared: Rc<RefCell<Shared>>,
    task: Option<Task<()>>,
}

#[derive(Default)]
struct Shared {
    trail: Option<Trail>,
    /// The editor, text revision and caret the trail answers for.
    key: Option<(EntityId, u64, usize)>,
    /// The key a look is waiting to run for.
    waiting: Option<(EntityId, u64, usize)>,
}

impl TrailWatch {
    /// What the field shows; `None` hides it.
    pub fn trail(&self) -> Option<Trail> {
        self.shared.borrow().trail.clone()
    }

    /// Call when the caret moves, the text changes or the tab switches (not per frame). `lang` is
    /// `Lang::from_name` of the file's language (`None` hides the field). Returns whether the
    /// trail changed, so the status line can compare before it notifies. While the tree is
    /// behind the text the last good trail stays, and a look again follows (the status line `T`
    /// is notified if that finds a different trail).
    pub fn refresh<T: 'static>(&mut self, editor: &Entity<EditorState>, lang: Option<Lang>, cx: &mut Context<T>) -> bool {
        let Some(lang) = lang.filter(|_| editor.read(cx).text().len() <= MAX_HIGHLIGHT_BYTES) else {
            self.task = None;
            self.shared.borrow_mut().waiting = None;
            let mut shared = self.shared.borrow_mut();
            shared.key = None;
            return shared.trail.take().is_some();
        };
        // Another editor's last good trail is not this one's.
        let switched = self.shared.borrow().key.is_some_and(|(id, _, _)| id != editor.entity_id());
        if switched {
            let mut shared = self.shared.borrow_mut();
            shared.key = None;
            shared.trail = None;
        }
        let key = (editor.entity_id(), editor.read(cx).text_revision(), editor.read(cx).cursor());
        if self.shared.borrow().key == Some(key) {
            self.task = None;
            self.shared.borrow_mut().waiting = None;
            return false;
        }
        // The first look and a tab switch have no trail to keep showing, so they look at once.
        if self.shared.borrow().key.is_none() {
            if let Some(changed) = Self::look(&self.shared, editor, lang, cx) {
                self.task = None;
                self.shared.borrow_mut().waiting = None;
                return changed;
            }
        }
        // The look walks siblings, so it waits for a pause: holding an arrow key in a huge array
        // looks once at the end. The last good trail stays until then.
        if self.shared.borrow().waiting == Some(key) {
            return false;
        }
        self.shared.borrow_mut().waiting = Some(key);
        let shared = self.shared.clone();
        let editor = editor.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LOOK_AFTER).await;
            let changed = when_parsed(&editor, cx, |cx| Self::look(&shared, &editor, lang, cx)).await;
            shared.borrow_mut().waiting = None;
            if changed == Some(true) {
                let _ = this.update(cx, |_, cx| cx.notify());
            }
        }));
        false
    }

    /// One look at the tree: `None` while it is behind the text, else whether the trail changed.
    fn look(shared: &Rc<RefCell<Shared>>, editor: &Entity<EditorState>, lang: Lang, cx: &App) -> Option<bool> {
        let state = editor.read(cx);
        let key = (editor.entity_id(), state.text_revision(), state.cursor());
        if shared.borrow().key == Some(key) {
            return Some(false);
        }
        let trail = state.with_current_syntax_tree(|tree, text| tree.downcast_ref::<Tree>().map(|tree| trail_at(tree, text, key.2, lang)))??;
        let mut shared = shared.borrow_mut();
        shared.key = Some(key);
        let changed = shared.trail != trail;
        shared.trail = trail;
        Some(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::{Lang, Segment, Trail, trail_at};
    use tree_sitter::{Language, Parser};

    fn trail(lang: Lang, text: &str) -> Option<Trail> {
        // `|` marks the caret.
        let caret = text.find('|').expect("a caret");
        let source = text.replacen('|', "", 1);
        let language: Language = match lang {
            Lang::Json => tree_sitter_json::LANGUAGE.into(),
            Lang::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
            Lang::Yaml => tree_sitter_yaml::LANGUAGE.into(),
        };
        let mut parser = Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(&source, None).unwrap();
        trail_at(&tree, source.as_str(), caret, lang)
    }

    fn label(lang: Lang, text: &str) -> (String, String, String) {
        let trail = trail(lang, text).unwrap_or_else(|| panic!("no trail for {text:?}"));
        (trail.label(), trail.dotted(), trail.jq())
    }

    fn expect(lang: Lang, text: &str, label_: &str, dotted: &str, jq: &str) {
        assert_eq!(label(lang, text), (label_.to_string(), dotted.to_string(), jq.to_string()), "{text}");
    }

    #[test]
    fn json_keys_and_indexes() {
        let text = "{\n  \"server\": {\n    \"ports\": [80, 4|43, {\"name\": \"x\"}]\n  }\n}";
        expect(Lang::Json, text, "server > ports > 1", "server.ports[1]", ".server.ports[1]");
        expect(Lang::Json, "{\"a\": {\"b\": [1, {\"c\": \"v|\"}]}}", "a > b > 1 > c", "a.b[1].c", ".a.b[1].c");
        expect(Lang::Json, "{\"a\": 1, \"my |key\": 2}", "my key", "\"my key\"", ".\"my key\"");
        expect(Lang::Json, "{\"a-b\": 1|}", "a-b", "a-b", ".\"a-b\"");
        // A comment is not an element.
        expect(Lang::Json, "[\n  // first\n  1,\n  /* c */ 2|\n]", "1", "[1]", "[1]");
        assert_eq!(trail(Lang::Json, "{\n|  \"a\": 1}"), None);
    }

    #[test]
    fn toml_tables_dotted_keys_and_arrays_of_tables() {
        expect(Lang::Toml, "[server]\nhost = \"x\"\nports = [1, 2|]\n", "server > ports > 1", "server.ports[1]", ".server.ports[1]");
        expect(Lang::Toml, "a.b.c| = 1\n", "a > b > c", "a.b.c", ".a.b.c");
        expect(Lang::Toml, "[x.y]\nk.l = 1|\n", "x > y > k > l", "x.y.k.l", ".x.y.k.l");
        expect(Lang::Toml, "[[w]]\nk = 1\n[[w]]\nk = 2|\n[other]\nz = 1\n", "w > 1 > k", "w[1].k", ".w[1].k");
        expect(Lang::Toml, "[[w]]\nk = 1\n[[w]]\nk = 2\n[[v]]\nq| = 1\n", "v > 0 > q", "v[0].q", ".v[0].q");
        expect(Lang::Toml, "t = { a = 1, b = { c = 2| } }\n", "t > b > c", "t.b.c", ".t.b.c");
        expect(Lang::Toml, "[\"my table\"]\n\"my key\" = 1|\n", "my table > my key", "\"my table\".\"my key\"", ".\"my table\".\"my key\"");
        // On a header.
        expect(Lang::Toml, "[tool.a|]\nx = 1\n", "tool > a", "tool.a", ".tool.a");
        assert_eq!(trail(Lang::Toml, "|# just a comment\n[a]\n"), None);
    }

    #[test]
    fn yaml_block_and_flow() {
        expect(Lang::Yaml, "server:\n  ports:\n    - 80\n    - 4|43\n", "server > ports > 1", "server.ports[1]", ".server.ports[1]");
        expect(Lang::Yaml, "a:\n  - b: 1\n    c: [x, {d: e|}]\n", "a > 0 > c > 1 > d", "a[0].c[1].d", ".a[0].c[1].d");
        expect(Lang::Yaml, "\"my key\": v|\n", "my key", "\"my key\"", ".\"my key\"");
        expect(Lang::Yaml, "a: 1\n---\nb:\n  c: 2|\n", "b > c", "b.c", ".b.c");
        // Comments between items are not items.
        expect(Lang::Yaml, "l:\n  - a\n  # note\n  - b|\n", "l > 1", "l[1]", ".l[1]");
    }

    #[test]
    fn short_label_keeps_the_inner_segments() {
        let t = Trail { segments: ["alpha", "beta", "gamma"].iter().map(|s| Segment::Key(s.to_string())).collect() };
        assert_eq!(t.short_label(100), "alpha > beta > gamma");
        assert_eq!(t.short_label(14), "\u{2026} > gamma");
    }
}
