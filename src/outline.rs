//! The outline of the open file for Go to Symbol (finch#aee9): its functions and types, Markdown
//! headings, TOML tables and top-level YAML keys, each with a kind, a name and a line.
//!
//! Code symbols come from the tree the highlighter already holds (`EditorState::with_current_syntax_tree`,
//! so no second parse), walking named nodes by their grammar's kind. Text formats come from a line
//! scan. Both run on a text snapshot off the main thread; nothing runs until the field opens, and a
//! file past `MAX_BYTES` has no outline.

use std::borrow::Cow;

use gpui_kit::component::input::{Rope, RopeExt};
use tree_sitter::{Node, Tree};

/// A file past this many bytes has no outline (the same ceiling as the rest of the structure work).
pub const MAX_BYTES: usize = 4 * 1024 * 1024;
/// Most symbols listed; a generated file with more is cut here.
const MAX_SYMBOLS: usize = 20_000;
/// Most characters of a name kept.
const NAME_CHARS: usize = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Function,
    Struct,
    Enum,
    Trait,
    Impl,
    Class,
    Heading,
    Table,
    Key,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub kind: Kind,
    pub name: String,
    /// Counted from 1.
    pub line: u32,
}

/// Whether `lang` (the highlighter's language name) has an outline read from its text.
pub fn is_text_format(lang: &str) -> bool {
    matches!(lang, "markdown" | "toml" | "yaml")
}

/// The outline of a text format by a line scan of the rope, each line read in place; `None` for any other language.
pub fn from_text(lang: &str, text: &Rope) -> Option<Vec<Symbol>> {
    if text.len() > MAX_BYTES {
        return Some(Vec::new());
    }
    match lang {
        "markdown" => Some(markdown(text)),
        "toml" => Some(toml(text)),
        "yaml" => Some(yaml(text)),
        _ => None,
    }
}

/// How many symbols to make room for: a symbol to every 16 lines, at most 4096 (a bigger outline grows past it). Counts only lines, never reads text.
fn estimate(text: &Rope) -> usize {
    (text.lines_len() / 16).min(4096)
}

/// The rope's lines (each with its `\r` if it has one). A line inside one chunk (nearly all) is borrowed; one that spans chunks is joined.
fn rope_lines(text: &Rope) -> impl Iterator<Item = Cow<'_, str>> {
    text.iter_lines().map(|line| match line.as_str() {
        Some(line) => Cow::Borrowed(line),
        None => Cow::Owned(line.to_string()),
    })
}

/// The outline of a code file from its parse tree and the rope the tree describes.
pub fn from_tree(tree: &Tree, text: &Rope) -> Vec<Symbol> {
    if text.len() > MAX_BYTES {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(estimate(text));
    let mut cursor = tree.walk();
    'walk: loop {
        let node = cursor.node();
        let mut descend = true;
        if node.is_named()
            && let Some(symbol) = symbol(node, text)
        {
            // A function's body is not listed (a closure or a nested fn is no landmark); a type's is (its methods).
            descend = symbol.kind != Kind::Function;
            out.push(symbol);
            if out.len() >= MAX_SYMBOLS {
                break;
            }
        }
        if descend && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }
    out
}

fn symbol(node: Node, text: &Rope) -> Option<Symbol> {
    let kind = match node.kind() {
        "function_item" | "function_definition" | "function_declaration" | "method_definition" | "method_declaration" | "constructor_declaration" | "generator_function_declaration" | "method"
        | "singleton_method" => Kind::Function,
        "variable_declarator" => {
            let value = node.child_by_field_name("value")?;
            if !matches!(value.kind(), "arrow_function" | "function_expression" | "function") {
                return None;
            }
            Kind::Function
        }
        "struct_item" | "struct_declaration" | "struct_definition" => Kind::Struct,
        "struct_specifier" if node.child_by_field_name("body").is_some() => Kind::Struct,
        "enum_item" | "enum_declaration" => Kind::Enum,
        "enum_specifier" if node.child_by_field_name("body").is_some() => Kind::Enum,
        "trait_item" | "interface_declaration" | "protocol_declaration" | "trait_declaration" => Kind::Trait,
        "impl_item" => Kind::Impl,
        "class_definition" | "class_declaration" | "class" => Kind::Class,
        "class_specifier" if node.child_by_field_name("body").is_some() => Kind::Class,
        "type_spec" => match node.child_by_field_name("type")?.kind() {
            "struct_type" => Kind::Struct,
            "interface_type" => Kind::Trait,
            _ => return None,
        },
        _ => return None,
    };
    let name = if kind == Kind::Impl { impl_name(node, text)? } else { name_of(node, text)? };
    Some(Symbol { kind, name, line: node.start_position().row as u32 + 1 })
}

/// The first line of the node's text, cut at `NAME_CHARS`; read from the rope by byte range, never past the line.
fn words(node: Node, text: &Rope) -> Option<String> {
    let range = node.byte_range();
    if range.end > text.len() {
        return None;
    }
    let slice = text.slice(range);
    // A name inside one chunk (nearly all) is read in place; one that spans chunks has its first line joined.
    let joined;
    let line = match slice.as_str() {
        Some(whole) => whole.lines().next().unwrap_or(""),
        None => {
            let mut line = String::new();
            for chunk in slice.chunks() {
                let end = chunk.find('\n');
                line.push_str(&chunk[..end.unwrap_or(chunk.len())]);
                // A name's first line is short; a line this long is cut at `NAME_CHARS` below whatever follows.
                if end.is_some() || line.len() >= 1024 {
                    break;
                }
            }
            joined = line;
            &joined
        }
    };
    let line = line.trim();
    (!line.is_empty()).then(|| line.chars().take(NAME_CHARS).collect())
}

/// A declaration's name: its `name` field, else the identifier at the bottom of its `declarator` chain (C and C++ functions).
fn name_of(node: Node, text: &Rope) -> Option<String> {
    let mut at = node;
    loop {
        if let Some(name) = at.child_by_field_name("name") {
            return words(name, text);
        }
        at = at.child_by_field_name("declarator")?;
        if at.child_count() == 0 {
            return words(at, text);
        }
    }
}

/// `Type`, or `Trait for Type` for a trait's impl.
fn impl_name(node: Node, text: &Rope) -> Option<String> {
    let ty = words(node.child_by_field_name("type")?, text)?;
    Some(match node.child_by_field_name("trait").and_then(|tr| words(tr, text)) {
        Some(tr) => format!("{tr} for {ty}"),
        None => ty,
    })
}

/// ATX (`# Title`) and setext (`Title` over `===` or `---`) headings outside code fences.
fn markdown(text: &Rope) -> Vec<Symbol> {
    let mut out = Vec::with_capacity(estimate(text));
    let mut fence: Option<(char, usize)> = None;
    // The line above the current one, when it could be a setext heading's text: its row, and its cut text in `held` (one buffer, reused).
    let mut paragraph: Option<u32> = None;
    let mut held = String::new();
    for (ix, raw) in rope_lines(text).enumerate() {
        let line = raw.trim_end();
        let number = ix as u32 + 1;
        let indent = line.len() - line.trim_start_matches(' ').len();
        let body = &line[indent..];
        if let Some((mark, length)) = fence {
            let run = body.chars().take_while(|ch| *ch == mark).count();
            if indent < 4 && run >= length && body[run..].trim().is_empty() {
                fence = None;
            }
            paragraph = None;
            continue;
        }
        if indent < 4 {
            let mark = body.chars().next().filter(|ch| matches!(ch, '`' | '~'));
            if let Some(mark) = mark {
                let run = body.chars().take_while(|ch| *ch == mark).count();
                // A backtick fence's info string can't hold a backtick (that is inline code).
                if run >= 3 && !(mark == '`' && body[run..].contains('`')) {
                    fence = Some((mark, run));
                    paragraph = None;
                    continue;
                }
            }
            let hashes = body.chars().take_while(|ch| *ch == '#').count();
            if (1..=6).contains(&hashes) && (body.len() == hashes || body[hashes..].starts_with([' ', '\t'])) {
                let content = body[hashes..].trim();
                // `# Title #` closes with hashes after a space; `# C#` keeps its own.
                let stripped = content.trim_end_matches('#');
                let title = if stripped.is_empty() || stripped.ends_with([' ', '\t']) { stripped.trim_end() } else { content };
                if !title.is_empty() {
                    out.push(Symbol { kind: Kind::Heading, name: title.chars().take(NAME_CHARS).collect(), line: number });
                }
                paragraph = None;
                continue;
            }
            let underline = !body.is_empty() && (body.chars().all(|ch| ch == '=') || body.chars().all(|ch| ch == '-'));
            if underline && let Some(at) = paragraph.take() {
                out.push(Symbol { kind: Kind::Heading, name: held.clone(), line: at });
                continue;
            }
        }
        paragraph = (!body.is_empty() && indent < 4 && !body.starts_with(['>', '-', '*', '+', '|']) && !is_ordered(body)).then_some(number);
        if paragraph.is_some() {
            held.clear();
            held.extend(body.trim().chars().take(NAME_CHARS));
        }
    }
    out
}

/// `1. item` or `1) item`: a list line, never a setext heading's text.
fn is_ordered(body: &str) -> bool {
    let digits = body.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && body[digits..].starts_with(['.', ')'])
}

/// `[a.b]` and `[[x]]` headers, outside multi-line strings.
fn toml(text: &Rope) -> Vec<Symbol> {
    let mut out = Vec::with_capacity(estimate(text));
    let mut multiline: Option<&str> = None;
    for (ix, raw) in rope_lines(text).enumerate() {
        let line = raw.trim();
        if let Some(quote) = multiline {
            if line.matches(quote).count() % 2 == 1 {
                multiline = None;
            }
            continue;
        }
        if line.starts_with('[') {
            let double = line.starts_with("[[");
            let (open, close) = if double { (2, "]]") } else { (1, "]") };
            if let Some(end) = line.find(close) {
                let rest = line[end + close.len()..].trim();
                let name = line[open..end].trim();
                if (rest.is_empty() || rest.starts_with('#')) && !name.is_empty() {
                    out.push(Symbol { kind: Kind::Table, name: name.chars().take(NAME_CHARS).collect(), line: ix as u32 + 1 });
                }
            }
            continue;
        }
        // A value opening `"""` or `'''` without closing it holds the next lines as text.
        for quote in ["\"\"\"", "'''"] {
            if line.matches(quote).count() % 2 == 1 {
                multiline = Some(quote);
                break;
            }
        }
    }
    out
}

/// The keys at the left edge of a YAML file.
fn yaml(text: &Rope) -> Vec<Symbol> {
    let mut out = Vec::with_capacity(estimate(text));
    for (ix, raw) in rope_lines(text).enumerate() {
        let line = raw.trim_end();
        let Some(first) = line.chars().next() else { continue };
        if matches!(first, ' ' | '\t' | '-' | '#' | '%' | '?' | '[' | '{' | '&' | '*' | '!' | '|' | '>') {
            continue;
        }
        let key = if matches!(first, '"' | '\'') {
            let Some(end) = line[1..].find(first).map(|end| end + 1) else { continue };
            let rest = line[end + 1..].trim_start();
            if !rest.starts_with(':') {
                continue;
            }
            &line[1..end]
        } else {
            // The key ends at the first `:` followed by a space or the line's end.
            let Some(end) = line.char_indices().find(|&(at, ch)| ch == ':' && line[at + 1..].chars().next().is_none_or(|next| next == ' ')).map(|(at, _)| at) else { continue };
            line[..end].trim_end()
        };
        if !key.is_empty() {
            out.push(Symbol { kind: Kind::Key, name: key.chars().take(NAME_CHARS).collect(), line: ix as u32 + 1 });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Kind, Symbol, from_text, from_tree};
    use gpui_kit::component::{highlighter::SyntaxHighlighter, input::Rope};

    fn names(lang: &str, text: &str) -> Vec<(Kind, String, u32)> {
        from_text(lang, &Rope::from(text)).unwrap().into_iter().map(|Symbol { kind, name, line }| (kind, name, line)).collect()
    }

    fn heading(name: &str, line: u32) -> (Kind, String, u32) {
        (Kind::Heading, name.to_string(), line)
    }

    #[test]
    fn markdown_lists_atx_and_setext_headings() {
        let text = "# One\n\nBody\n\n## Two ##\nSetext\n======\nSub\n---\n####### seven\n#hashtag\n# C#\n";
        assert_eq!(names("markdown", text), vec![heading("One", 1), heading("Two", 5), heading("Setext", 6), heading("Sub", 8), heading("C#", 12)]);
    }

    #[test]
    fn markdown_skips_headings_inside_fences() {
        let text = "# Real\n```sh\n# comment\n```\n~~~~\n## also code\n~~~\n~~~~\n## After\n";
        assert_eq!(names("markdown", text), vec![heading("Real", 1), heading("After", 9)]);
    }

    #[test]
    fn markdown_rule_and_list_are_not_setext() {
        let text = "Para\n\n---\n- item\n---\n";
        assert_eq!(names("markdown", text), vec![]);
    }

    #[test]
    fn toml_lists_tables_and_arrays_of_tables() {
        let text = "title = \"x\"\n[package]\nname = \"a\"\n[dependencies.serde] # c\nfeatures = [\n  [1, 2],\n]\n[[bin]]\nnote = \"\"\"\n[not.a.table]\n\"\"\"\n[ spaced . name ]\n";
        let table = |name: &str, line| (Kind::Table, name.to_string(), line);
        assert_eq!(names("toml", text), vec![table("package", 2), table("dependencies.serde", 4), table("bin", 8), table("spaced . name", 12)]);
    }

    #[test]
    fn yaml_lists_top_level_keys_only() {
        let text = "---\n# c\nname: x\nservices:\n  web:\n    image: y\n- item\n\"quoted key\": 1\nurl: http://a:b\nempty:\n";
        let key = |name: &str, line| (Kind::Key, name.to_string(), line);
        assert_eq!(names("yaml", text), vec![key("name", 3), key("services", 4), key("quoted key", 8), key("url", 9), key("empty", 10)]);
    }

    #[test]
    fn code_languages_have_no_text_outline() {
        assert!(from_text("rust", &Rope::from("fn a() {}")).is_none());
    }

    fn rust_outline(rope: &Rope) -> Vec<Symbol> {
        let mut highlighter = SyntaxHighlighter::new("rust");
        highlighter.update(None, rope, None);
        from_tree(highlighter.tree().expect("parsed"), rope)
    }

    #[test]
    fn rust_names_come_from_the_rope() {
        let rope = Rope::from("struct A;\nimpl Display for A {\n    fn fmt(&self) {}\n}\nfn  wide_name_here() {}\n");
        let got: Vec<_> = rust_outline(&rope).into_iter().map(|Symbol { kind, name, line }| (kind, name, line)).collect();
        assert_eq!(got, vec![(Kind::Struct, "A".into(), 1), (Kind::Impl, "Display for A".into(), 2), (Kind::Function, "fmt".into(), 3), (Kind::Function, "wide_name_here".into(), 5)]);
    }

    /// The outline of a 3 MB file no longer copies the text: before, `text.to_string()` asked for 3 MB on every open (and held it while
    /// the scan ran); now the scan asks only for its symbols and a line buffer. The parse is made outside the count, as the editor's is.
    #[test]
    fn the_outline_of_a_big_file_copies_no_text() {
        let markdown: String = (0..60_000).map(|i| if i % 20 == 0 { format!("## Section {i}\n") } else { format!("Paragraph line number {i} with a few more words\n") }).collect();
        let rust: String = (0..19_000).map(|i| format!("fn function_{i}(a: u32) -> u32 {{\n    let first = a + {i};\n    let second = first * 3;\n    let third = second - 1;\n    let fourth = third + first;\n    fourth\n}}\n")).collect();
        let mut highlighter = SyntaxHighlighter::new("rust");
        for (name, text, symbols) in [("markdown", markdown, 3_000), ("rust", rust, 19_000)] {
            assert!(text.len() > 2_500_000 && text.len() < super::MAX_BYTES, "{name}: {} bytes", text.len());
            let rope = Rope::from(text.as_str());
            let (copy, copied) = crate::test_alloc::bytes(|| rope.to_string());
            assert_eq!(copy.len(), text.len());
            drop(copy);
            let (found, asked) = if name == "markdown" {
                crate::test_alloc::bytes(|| from_text("markdown", &rope).unwrap())
            } else {
                highlighter.update(None, &rope, None);
                let tree = highlighter.tree().expect("parsed");
                crate::test_alloc::bytes(|| from_tree(tree, &rope))
            };
            eprintln!("{name}: {} text bytes; before the outline asked {} bytes (the copy {copied} and the same symbols), now {asked}, {} symbols", text.len(), copied + asked, found.len());
            assert_eq!(found.len(), symbols, "{name}");
            assert!(asked < text.len(), "{name}: {asked} bytes asked for {} bytes of text (a copy asks {copied})", text.len());
        }
    }
}
