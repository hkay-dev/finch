use std::{collections::BTreeMap, ops::Range, sync::Arc};

use gpui::SharedString;
use markdown::{
    ParseOptions,
    mdast::{self, Node},
    unist::Point,
};

use crate::text::{
    document::ParsedDocument,
    markdown_ext::{EXTERNAL_LINK_ARROW, MarkdownParseContext},
    node::{
        self, BlockNode, Callout, CalloutKind, CodeBlock, ImageNode, InlineNode, LinkMark,
        NodeContext, Paragraph, SourceSegment, Span, Table, TableRow, TextMark,
    },
};

/// Whether `url` is an `http` or `https` address, which opens outside the app.
fn is_web_url(url: &str) -> bool {
    let scheme_end = url.find("://").unwrap_or(0);
    matches!(url[..scheme_end].to_ascii_lowercase().as_str(), "http" | "https")
}

/// Parse Markdown into a tree of nodes.
pub(crate) fn parse(source: &str, cx: &mut NodeContext) -> Result<ParsedDocument, SharedString> {
    let options = cx.markdown_extensions.parse_options();
    let mut root =
        markdown::to_mdast(source, &options).map_err(|e| SharedString::from(e.to_string()))?;
    let mut prose = options;
    prose.constructs.math_text = false;
    prose.constructs.math_flow = false;
    flatten_unclaimed_math(&mut root, source, &prose, cx);
    Ok(ast_to_document(source, root, cx))
}

/// Math parsing is on by default, so prose that merely contains two dollar
/// signs ("$5 and $10") parses as an inline math node. When no plugin claims
/// such a node, the text between the dollars must stay ordinary Markdown:
/// emphasis, inline HTML and links inside it render as they would anywhere
/// else, and inline HTML tags may pair with tags outside the span. Re-parse
/// the node's source without the math constructs and, when it holds any
/// markup, splice the result into its parent with positions shifted back into
/// the document. Plain prose keeps the literal node, whose atomic source
/// mapping the selection tests rely on.
fn flatten_unclaimed_math(node: &mut Node, source: &str, options: &ParseOptions, cx: &NodeContext) {
    let Some(children) = node.children_mut() else {
        return;
    };
    let mut ix = 0;
    while ix < children.len() {
        let prose = match &children[ix] {
            Node::InlineMath(_) => {
                let parse_cx = MarkdownParseContext::new(source, cx.offset);
                if cx
                    .markdown_extensions
                    .parse_inline(&children[ix], &parse_cx)
                    .is_some()
                {
                    None
                } else {
                    reparse_as_prose(&children[ix], source, options)
                }
            }
            _ => None,
        };
        if let Some(prose) = prose {
            let count = prose.len();
            children.splice(ix..=ix, prose);
            ix += count;
            continue;
        }
        flatten_unclaimed_math(&mut children[ix], source, options, cx);
        ix += 1;
    }
}

fn reparse_as_prose(node: &Node, source: &str, options: &ParseOptions) -> Option<Vec<Node>> {
    let position = node.position()?.clone();
    let literal = source.get(position.start.offset..position.end.offset)?;
    if !may_hold_inline_markup(literal) {
        return None;
    }
    let Ok(Node::Root(mut root)) = markdown::to_mdast(literal, options) else {
        return None;
    };
    let [Node::Paragraph(paragraph)] = root.children.as_mut_slice() else {
        return None;
    };
    if paragraph
        .children
        .iter()
        .all(|child| matches!(child, Node::Text(_)))
    {
        return None;
    }
    let mut children = std::mem::take(&mut paragraph.children);
    for child in &mut children {
        shift_positions(child, &position.start);
    }
    Some(children)
}

/// A cheap gate before re-parsing: every inline construct starts with one of
/// these bytes (tags, emphasis, code, links, images, escapes, entities,
/// strikethrough) or is a GFM autolink literal.
fn may_hold_inline_markup(literal: &str) -> bool {
    literal.bytes().any(|byte| {
        matches!(
            byte,
            b'<' | b'*' | b'_' | b'[' | b'`' | b'~' | b'\\' | b'!' | b'&'
        )
    }) || literal.contains("://")
        || literal.contains("www.")
}

fn shift_positions(node: &mut Node, origin: &Point) {
    if let Some(position) = node.position_mut() {
        for point in [&mut position.start, &mut position.end] {
            let first_line = point.line == 1;
            point.offset += origin.offset;
            point.line += origin.line - 1;
            if first_line {
                point.column += origin.column - 1;
            }
        }
    }
    if let Some(children) = node.children_mut() {
        for child in children {
            shift_positions(child, origin);
        }
    }
}

enum InlineGroup<'a> {
    Node(&'a Node),
    Marked(TextMark, &'a [Node]),
}

/// CommonMark hands each raw inline tag over as its own `Html` node, so
/// `<strong>x</strong>` arrives as three siblings and the tags alone carry no
/// text. Pair the formatting tags this renderer knows with their closing tag
/// among the siblings and treat what lies between as a marked run, the same
/// way `**x**` is handled.
fn inline_groups(children: &[Node]) -> Vec<InlineGroup<'_>> {
    let mut groups = Vec::with_capacity(children.len());
    let mut ix = 0;
    while ix < children.len() {
        if let Some((false, name, attrs)) = inline_html_tag(&children[ix])
            && let Some(mark) = inline_html_mark(&name, attrs)
            && let Some(close) = matching_close_tag(children, ix, &name)
        {
            groups.push(InlineGroup::Marked(mark, &children[ix + 1..close]));
            ix = close + 1;
            continue;
        }
        groups.push(InlineGroup::Node(&children[ix]));
        ix += 1;
    }
    groups
}

fn inline_html_tag(node: &Node) -> Option<(bool, String, &str)> {
    let Node::Html(html) = node else {
        return None;
    };
    let inner = html.value.trim().strip_prefix('<')?.strip_suffix('>')?;
    if inner.ends_with('/') {
        return None;
    }
    let (closing, rest) = match inner.strip_prefix('/') {
        Some(rest) => (true, rest),
        None => (false, inner),
    };
    let name_len = rest.chars().take_while(char::is_ascii_alphanumeric).count();
    let name = rest[..name_len].to_ascii_lowercase();
    let attrs = rest[name_len..].trim();
    (!name.is_empty()).then_some((closing, name, attrs))
}

fn inline_html_attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = attrs;
    while !rest.is_empty() {
        rest = rest.trim_start();
        let key_len = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '=' | '/' | '>'))
            .unwrap_or(rest.len());
        if key_len == 0 {
            break;
        }
        let key = &rest[..key_len];
        rest = rest[key_len..].trim_start();
        let value = if let Some(after_eq) = rest.strip_prefix('=') {
            rest = after_eq.trim_start();
            if let Some(quote) = rest
                .as_bytes()
                .first()
                .copied()
                .filter(|b| *b == b'"' || *b == b'\'')
            {
                rest = &rest[1..];
                let end = rest.find(quote as char)?;
                let value = &rest[..end];
                rest = &rest[end + 1..];
                value
            } else {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                let value = &rest[..end];
                rest = &rest[end..];
                value
            }
        } else {
            ""
        };
        if key.eq_ignore_ascii_case(name) {
            return Some(value);
        }
    }
    None
}

fn inline_html_mark(name: &str, attrs: &str) -> Option<TextMark> {
    Some(match name {
        "strong" | "b" => TextMark::default().bold(),
        "em" | "i" => TextMark::default().italic(),
        "u" => TextMark::default().underline(),
        "s" | "del" | "strike" => TextMark::default().strikethrough(),
        "mark" => {
            match super::html::mark_color_from_values(
                inline_html_attr(attrs, "color"),
                inline_html_attr(attrs, "style"),
            ) {
                Some(color) => TextMark::default().highlight(color),
                None => TextMark::default().themed_highlight(),
            }
        }
        _ => return None,
    })
}

fn matching_close_tag(children: &[Node], open: usize, name: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (ix, child) in children.iter().enumerate().skip(open + 1) {
        match inline_html_tag(child) {
            Some((false, tag, _)) if tag == name => depth += 1,
            Some((true, tag, _)) if tag == name => {
                if depth == 0 {
                    return Some(ix);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

fn parse_inline_children(
    source: &str,
    paragraph: &mut Paragraph,
    children: &[Node],
    cx: &mut NodeContext,
) -> String {
    let first = paragraph.children.len();
    let mut text = String::new();
    for group in inline_groups(children) {
        match group {
            InlineGroup::Node(child) => {
                text.push_str(&parse_paragraph(source, paragraph, child, cx));
            }
            InlineGroup::Marked(mark, nodes) => {
                text.push_str(&merge_children_with_mark(
                    source, paragraph, nodes, mark, cx,
                ));
            }
        }
    }
    smarten_punctuation(paragraph, first);
    text
}

/// Draws prose in typographic punctuation: curly quotes and apostrophes, `--` as an en
/// dash, `---` as an em dash and `...` as an ellipsis. It runs on the rendered runs of
/// `paragraph.children[first..]`, so a quote after an emphasis or link closes it, and the
/// source is never touched. Code, autolink labels, objects and runs without a source
/// mapping (inline HTML) stay as written.
fn smarten_punctuation(paragraph: &mut Paragraph, first: usize) {
    let mut previous: Option<char> = None;
    for node in &mut paragraph.children[first..] {
        if node.custom.is_some() || node.image.is_some() {
            previous = Some('\u{fffc}');
            continue;
        }
        let bytes = node.text.as_bytes();
        let candidate = !node.source_segments.is_empty()
            && (bytes.iter().any(|&byte| matches!(byte, b'"' | b'\''))
                || bytes.windows(2).any(|pair| pair == b".." || pair == b"--"));
        if !candidate {
            previous = node.text.chars().next_back().or(previous);
            continue;
        }
        let replacements = punctuation_replacements(
            &node.text,
            &node.marks,
            &node.source_segments,
            &mut previous,
        );
        if !replacements.is_empty() {
            apply_replacements(node, &replacements);
        }
    }
}

fn opens_quote(previous: Option<char>) -> bool {
    previous.is_none_or(|previous| {
        previous.is_whitespace()
            || matches!(previous, '(' | '[' | '{' | '<' | '“' | '‘' | '–' | '—')
    })
}

/// Whether a `'` that opens by position is really a dropped letter or digit: `'n'`, `'90s`,
/// `'til`.
fn elided_start(rest: &str) -> bool {
    let word_end = rest
        .find(|c: char| !c.is_alphanumeric())
        .unwrap_or(rest.len());
    let word = &rest[..word_end];
    let after = &rest[word_end..];
    if word == "n" {
        return after.starts_with('\'');
    }
    if word.len() >= 3
        && word.ends_with('s')
        && word[..word.len() - 1].bytes().all(|byte| byte.is_ascii_digit())
    {
        return true;
    }
    matches!(word, "tis" | "twas" | "til" | "cause" | "em")
}

/// Whether a link's label is its own address (a GFM autolink), which must read as typed.
fn is_autolink_label(label: &str, url: &str) -> bool {
    let label = label.strip_suffix(EXTERNAL_LINK_ARROW).unwrap_or(label);
    let address = ["https://", "http://", "mailto:"]
        .iter()
        .find_map(|scheme| url.strip_prefix(scheme))
        .unwrap_or(url);
    label == url || label == address
}

/// The byte ranges of `text` to replace and what each becomes. `previous` is the character
/// before the run, as drawn, and is the last one drawn when this returns. A character that
/// came from an escape or an entity (`\"`, `&quot;`) has a non-linear source segment and
/// stays as written.
fn punctuation_replacements(
    text: &str,
    marks: &[(Range<usize>, TextMark)],
    segments: &[SourceSegment],
    previous: &mut Option<char>,
) -> Vec<(Range<usize>, &'static str)> {
    let protected = |at: usize| {
        let ix = segments.partition_point(|segment| segment.rendered.start <= at);
        if let Some(segment) = ix.checked_sub(1).map(|ix| &segments[ix])
            && segment.rendered.contains(&at)
            && !segment.linear
        {
            return true;
        }
        marks.iter().any(|(range, mark)| {
            range.contains(&at)
                && (mark.code
                    || mark
                        .link
                        .as_ref()
                        .is_some_and(|link| is_autolink_label(&text[range.clone()], &link.url)))
        })
    };
    let mut replacements = Vec::new();
    let mut at = 0;
    while let Some(c) = text[at..].chars().next() {
        if protected(at) {
            *previous = Some(c);
            at += c.len_utf8();
            continue;
        }
        match c {
            '"' | '\'' => {
                // A quote opens by position, unless a space, the end of the line or the
                // same quote follows it (`""`, `" `): then it closes. At the end of a run
                // the next run is unknown, so the quote opens.
                let next = text[at + 1..].chars().next();
                let mut opens = opens_quote(*previous)
                    && next.is_none_or(|next| !next.is_whitespace() && next != c);
                if c == '\'' {
                    opens = opens && !elided_start(&text[at + 1..]);
                }
                let curly = match (c, opens) {
                    ('"', true) => "“",
                    ('"', false) => "”",
                    (_, true) => "‘",
                    (_, false) => "’",
                };
                replacements.push((at..at + 1, curly));
                *previous = curly.chars().next();
                at += 1;
            }
            '.' | '-' => {
                let run = text.as_bytes()[at..]
                    .iter()
                    .enumerate()
                    .take_while(|&(ix, &byte)| byte == c as u8 && !protected(at + ix))
                    .count();
                let after_run = text[at + run..].chars().next();
                let replacement = match (c, run) {
                    ('.', 3) => Some("…"),
                    ('-', 3) => Some("—"),
                    // `--flag` after a space is a command line option, not a dash.
                    ('-', 2)
                        if !(opens_quote(*previous)
                            && after_run.is_some_and(char::is_alphanumeric)) =>
                    {
                        Some("–")
                    }
                    _ => None,
                };
                match replacement {
                    Some(curly) => {
                        replacements.push((at..at + run, curly));
                        *previous = curly.chars().next();
                    }
                    None => *previous = Some(c),
                }
                at += run;
            }
            _ => {
                *previous = Some(c);
                at += c.len_utf8();
            }
        }
    }
    replacements
}

/// Swaps `replacements` (sorted, disjoint byte ranges of the run's text) into the run and
/// moves its marks and source segments with them. A replaced stretch maps to the whole
/// source it stood for, so a selection or a reveal never lands inside it; the stretches
/// between stay linear. Both lists are sorted, so every lookup is a binary search.
fn apply_replacements(node: &mut InlineNode, replacements: &[(Range<usize>, &'static str)]) {
    let old = node.text.clone();
    let mut new = String::with_capacity(old.len());
    let mut cursor = 0;
    let mut placed = Vec::with_capacity(replacements.len());
    for (range, with) in replacements {
        new.push_str(&old[cursor..range.start]);
        let start = new.len();
        new.push_str(with);
        placed.push(start..new.len());
        cursor = range.end;
    }
    new.push_str(&old[cursor..]);

    let map = |offset: usize, is_end: bool| -> usize {
        let before = replacements.partition_point(|(range, _)| range.end <= offset);
        if let Some((range, _)) = replacements.get(before)
            && offset > range.start
        {
            let placed = &placed[before];
            return if is_end { placed.end } else { placed.start };
        }
        match before.checked_sub(1) {
            Some(last) => offset + placed[last].end - replacements[last].0.end,
            None => offset,
        }
    };

    for (range, _) in &mut node.marks {
        range.start = map(range.start, false);
        range.end = map(range.end, true);
    }

    let segments = std::mem::take(&mut node.source_segments);
    let source_start = |at: usize| {
        let ix = segments
            .partition_point(|segment| segment.rendered.start <= at)
            .checked_sub(1)?;
        let segment = &segments[ix];
        segment.rendered.contains(&at).then(|| {
            if segment.linear {
                segment.source.start + at - segment.rendered.start
            } else {
                segment.source.start
            }
        })
    };
    let source_end = |at: usize| {
        let ix = segments
            .partition_point(|segment| segment.rendered.start < at)
            .checked_sub(1)?;
        let segment = &segments[ix];
        (at <= segment.rendered.end).then(|| {
            if segment.linear {
                segment.source.start + at - segment.rendered.start
            } else {
                segment.source.end
            }
        })
    };

    let mut pieces = Vec::new();
    for segment in &segments {
        // The replacements that overlap this segment, in order.
        let first = replacements.partition_point(|(range, _)| range.end <= segment.rendered.start);
        let touching = replacements[first..]
            .iter()
            .take_while(|(range, _)| range.start < segment.rendered.end);
        if !segment.linear {
            if touching.clone().next().is_none() {
                pieces.push(SourceSegment {
                    rendered: map(segment.rendered.start, false)..map(segment.rendered.end, true),
                    source: segment.source.clone(),
                    linear: false,
                });
            }
            continue;
        }
        let stretch = |from: usize, to: usize| SourceSegment {
            rendered: map(from, false)..map(to, true),
            source: segment.source.start + from - segment.rendered.start
                ..segment.source.start + to - segment.rendered.start,
            linear: true,
        };
        let mut at = segment.rendered.start;
        for (range, _) in touching {
            if at < range.start {
                pieces.push(stretch(at, range.start));
            }
            at = range.end.min(segment.rendered.end);
        }
        if at < segment.rendered.end {
            pieces.push(stretch(at, segment.rendered.end));
        }
    }
    for ((range, _), placed) in replacements.iter().zip(&placed) {
        if let (Some(start), Some(end)) = (source_start(range.start), source_end(range.end)) {
            pieces.push(SourceSegment {
                rendered: placed.clone(),
                source: start..end,
                linear: false,
            });
        }
    }
    pieces.sort_by_key(|piece| piece.rendered.start);
    for piece in pieces {
        push_source_segment(&mut node.source_segments, piece);
    }
    node.text = new.into();
}

fn parse_table_row(source: &str, table: &mut Table, node: &mdast::TableRow, cx: &mut NodeContext) {
    let mut row = TableRow::default();
    node.children.iter().for_each(|c| {
        match c {
            Node::TableCell(cell) => {
                parse_table_cell(source, &mut row, cell, cx);
            }
            _ => {}
        };
    });
    // GFM fixes the column count with the header/delimiter. Leave missing
    // cells implicit; the renderer places them on the shared column grid.
    row.children.truncate(table.column_aligns.len());
    table.children.push(row);
}

fn parse_table_cell(
    source: &str,
    row: &mut node::TableRow,
    node: &mdast::TableCell,
    cx: &mut NodeContext,
) {
    let mut paragraph = Paragraph::default();
    parse_inline_children(source, &mut paragraph, &node.children, cx);
    let table_cell = node::TableCell {
        children: paragraph,
        ..Default::default()
    };
    row.children.push(table_cell);
}

/// Push a text run with its existing `marks` plus `new_mark` across the full
/// run.
///
/// If the last mark already covers the full run, merge into it. Otherwise add a
/// new full-run mark. Empty runs are skipped so callers can flush freely.
fn push_merged(
    paragraph: &mut Paragraph,
    text: String,
    marks: Vec<(Range<usize>, TextMark)>,
    source_segments: Vec<SourceSegment>,
    new_mark: TextMark,
) {
    if text.is_empty() {
        return;
    }

    let mut node = InlineNode::new(text)
        .marks(marks)
        .source_segments(source_segments);
    let len = node.text.len();
    if let Some(last) = node.marks.last_mut()
        && last.0.start == 0
        && last.0.end == len
    {
        last.1.merge(new_mark);
    } else {
        node.marks.push((0..len, new_mark));
    }
    paragraph.push(node);
}

/// Parse `children` and apply `mark` across each emitted text run.
///
/// Nested child marks are kept and shifted to match the combined text for the
/// current run, which lets nested emphasis like `**_x_**` render as both bold
/// and italic. Inline images split the run and are emitted as sibling image
/// nodes. The return value is the plain text from all children, for callers that
/// need to pass text back to their parent node.
fn merge_children_with_mark(
    source: &str,
    paragraph: &mut Paragraph,
    children: &[mdast::Node],
    mark: TextMark,
    cx: &mut NodeContext,
) -> String {
    let mut text = String::new();
    let mut merged_text = String::new();
    let mut merged_marks = Vec::new();
    let mut merged_source_segments = Vec::new();

    for group in inline_groups(children) {
        let mut child_paragraph = Paragraph::default();
        let child_text = match group {
            InlineGroup::Node(child) => parse_paragraph(source, &mut child_paragraph, child, cx),
            InlineGroup::Marked(child_mark, nodes) => {
                merge_children_with_mark(source, &mut child_paragraph, nodes, child_mark, cx)
            }
        };
        text.push_str(&child_text);

        for mut node in child_paragraph.children {
            if node.custom.is_some() {
                push_merged(
                    paragraph,
                    std::mem::take(&mut merged_text),
                    std::mem::take(&mut merged_marks),
                    std::mem::take(&mut merged_source_segments),
                    mark.clone(),
                );
                if let Some((_, existing)) = node.marks.first_mut() {
                    existing.merge(mark.clone());
                } else {
                    node.marks.push((0..node.text.len(), mark.clone()));
                }
                paragraph.push(node);
                continue;
            }
            let merged_offset = merged_text.len();
            merged_text.push_str(&node.text);
            merged_source_segments.extend(node.source_segments.drain(..).map(|mut segment| {
                segment.rendered.start += merged_offset;
                segment.rendered.end += merged_offset;
                segment
            }));

            for (range, child_mark) in node.marks {
                merged_marks.push((
                    range.start + merged_offset..range.end + merged_offset,
                    child_mark,
                ));
            }

            if let Some(mut image) = node.image {
                if let Some(link_mark) = mark.link.clone() {
                    image.link = Some(link_mark);
                }

                // GPUI InteractiveText does not support inline images, so
                // flush the accumulated text run and emit the image as its
                // own sibling InlineNode.
                push_merged(
                    paragraph,
                    std::mem::take(&mut merged_text),
                    std::mem::take(&mut merged_marks),
                    std::mem::take(&mut merged_source_segments),
                    mark.clone(),
                );
                paragraph.push(InlineNode::image(image));
            }
        }
    }

    push_merged(
        paragraph,
        merged_text,
        merged_marks,
        merged_source_segments,
        mark,
    );
    text
}

fn source_segments(
    source: &str,
    rendered: &str,
    span: Option<Span>,
    source_offset: usize,
    include_preceding_escape: bool,
) -> Vec<SourceSegment> {
    let Some(span) = span else {
        return Vec::new();
    };
    let local_start = span.start.saturating_sub(source_offset);
    let local_end = span.end.saturating_sub(source_offset);
    let Some(raw) = source.get(local_start..local_end) else {
        return Vec::new();
    };

    let mut segments = aligned_source_segments(raw, rendered, span.start, true, true);

    // The text node of an escaped character starts after its backslash. The
    // backslash and the character map as a whole; the rest of the run stays
    // linear.
    if include_preceding_escape
        && let Some(previous) = local_start.checked_sub(1)
        && source.as_bytes().get(previous) == Some(&b'\\')
        && let Some(first) = segments.first_mut()
    {
        let first_char_len = rendered[first.rendered.start..]
            .chars()
            .next()
            .map_or(0, char::len_utf8);
        if first.linear && first.rendered.len() > first_char_len {
            let rest = SourceSegment {
                rendered: first.rendered.start + first_char_len..first.rendered.end,
                source: first.source.start + first_char_len..first.source.end,
                linear: true,
            };
            first.rendered.end = rest.rendered.start;
            first.source.end = rest.source.start;
            segments.insert(1, rest);
        }
        let first = &mut segments[0];
        first.source.start -= 1;
        first.linear = false;
    }
    segments
}

fn aligned_source_segments(
    raw: &str,
    rendered: &str,
    source_offset: usize,
    decode_entities: bool,
    decode_escapes: bool,
) -> Vec<SourceSegment> {
    let mut segments = Vec::new();
    let mut raw_cursor = 0;
    let mut rendered_start = 0;
    let mut whitespace_end = 0;
    let mut source_positions = None;
    let mut entity_cursor = None;
    let mut entity = None;
    // Only the final LF may absorb source-only text before it. Find the
    // preceding line boundary once instead of recounting the suffix per LF.
    let final_line_start = raw
        .strip_suffix('\n')
        .and_then(|prefix| prefix.rfind('\n'))
        .map_or(0, |newline| newline + 1);

    while rendered_start < rendered.len() {
        if decode_entities && entity_cursor != Some(raw_cursor) {
            entity = decoded_entity(&raw[raw_cursor..]);
            entity_cursor = Some(raw_cursor);
        }
        if let Some((decoded, source_len)) = &entity
            && rendered[rendered_start..].starts_with(decoded.as_str())
        {
            let rendered_end = rendered_start + decoded.len();
            push_source_segment(
                &mut segments,
                SourceSegment {
                    rendered: rendered_start..rendered_end,
                    source: (source_offset + raw_cursor)
                        ..(source_offset + raw_cursor + *source_len),
                    // Even when the decoded characters take as many bytes as the
                    // entity, as `∾̳` and `&acE;` do, they are not one for one.
                    linear: false,
                },
            );
            rendered_start = rendered_end;
            raw_cursor += *source_len;
            continue;
        }

        let rendered_char = rendered[rendered_start..]
            .chars()
            .next()
            .expect("rendered cursor must be on a character boundary");
        let rendered_end = rendered_start + rendered_char.len_utf8();
        let remainder = &raw[raw_cursor..];
        if rendered_char == ' ' && raw_cursor >= whitespace_end {
            // Reuse this boundary while consuming a run of literal spaces.
            // Otherwise a long horizontal-whitespace run also costs O(n^2).
            whitespace_end = raw_cursor
                + remainder
                    .bytes()
                    .position(|byte| !matches!(byte, b' ' | b'\t'))
                    .unwrap_or(remainder.len());
        }
        let (relative_start, source_len) = if rendered_char == ' '
            && matches!(raw.as_bytes().get(whitespace_end), Some(b'\n' | b'\r'))
        {
            let newline_len = if raw[whitespace_end..].starts_with("\r\n") {
                2
            } else {
                1
            };
            (whitespace_end - raw_cursor, newline_len)
        } else if rendered_char == '\n'
            && remainder.ends_with('\n')
            && raw_cursor >= final_line_start
        {
            (0, remainder.len())
        } else if decode_escapes
            && let Some(escaped) = remainder.strip_prefix('\\')
            && escaped.starts_with(rendered_char)
        {
            (0, 1 + rendered_char.len_utf8())
        } else if remainder.starts_with(rendered_char) {
            (0, rendered_char.len_utf8())
        } else if let Some(source_start) =
            source_char_offset(raw, raw_cursor, rendered_char, &mut source_positions)
        {
            (source_start - raw_cursor, rendered_char.len_utf8())
        } else {
            // Decoded entities and other source-only syntax have no exact
            // rendered-byte mapping. Leave a rendered gap for this
            // character, but keep aligning later characters in the node.
            rendered_start = rendered_end;
            continue;
        };
        let source_start = raw_cursor + relative_start;
        let source_end = source_start + source_len;
        push_source_segment(
            &mut segments,
            SourceSegment {
                rendered: rendered_start..rendered_end,
                source: (source_offset + source_start)..(source_offset + source_end),
                linear: source_len == rendered_char.len_utf8(),
            },
        );
        raw_cursor = source_end;
        rendered_start = rendered_end;
    }
    segments
}

fn source_char_offset(
    raw: &str,
    raw_cursor: usize,
    character: char,
    positions: &mut Option<BTreeMap<char, Vec<usize>>>,
) -> Option<usize> {
    if let Some(positions) = positions.as_ref() {
        let offsets = positions.get(&character)?;
        return offsets
            .get(offsets.partition_point(|&offset| offset < raw_cursor))
            .copied();
    }
    if let Some(offset) = raw[raw_cursor..].find(character) {
        return Some(raw_cursor + offset);
    }

    // Successful searches advance raw_cursor, so their scans do not overlap.
    // A missing character does not advance it. Index the remaining source only
    // after the first miss so later gaps cannot repeatedly scan that suffix.
    let mut indexed = BTreeMap::<char, Vec<usize>>::new();
    for (offset, character) in raw[raw_cursor..].char_indices() {
        indexed
            .entry(character)
            .or_default()
            .push(raw_cursor + offset);
    }
    *positions = Some(indexed);
    None
}

/// Pushes `segment`, merging it into the previous one when both are linear and
/// adjacent, so that a merged run still maps byte for byte. Any other segment
/// is kept whole on its own.
fn push_source_segment(segments: &mut Vec<SourceSegment>, segment: SourceSegment) {
    if let Some(previous) = segments.last_mut()
        && previous.linear
        && segment.linear
        && previous.rendered.end == segment.rendered.start
        && previous.source.end == segment.source.start
    {
        previous.rendered.end = segment.rendered.end;
        previous.source.end = segment.source.end;
    } else {
        segments.push(segment);
    }
}

fn decoded_entity(source: &str) -> Option<(String, usize)> {
    let candidate = source.strip_prefix('&')?;
    let candidate_end = candidate
        .bytes()
        .position(|byte| byte == b';' || !(byte.is_ascii_alphanumeric() || byte == b'#'))?;
    if candidate.as_bytes()[candidate_end] != b';' {
        return None;
    }
    let semicolon = candidate_end + 1;
    let name = &source[1..=semicolon];
    let decoded = if let Some(number) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X"))
    {
        char::from_u32(u32::from_str_radix(number.strip_suffix(';')?, 16).ok()?)?.to_string()
    } else if let Some(number) = name.strip_prefix('#') {
        char::from_u32(number.strip_suffix(';')?.parse().ok()?)?.to_string()
    } else {
        let &(first, second) = html5ever::data::NAMED_ENTITIES.get(name)?;
        let mut decoded = char::from_u32(first)?.to_string();
        if second != 0 {
            decoded.push(char::from_u32(second)?);
        }
        decoded
    };
    Some((decoded, semicolon + 1))
}

fn code_source_segments(
    source: &str,
    code: &str,
    span: Option<Span>,
    source_offset: usize,
) -> Vec<SourceSegment> {
    let Some(span) = span else {
        return Vec::new();
    };
    let Some(raw) = source
        .get(span.start.saturating_sub(source_offset)..span.end.saturating_sub(source_offset))
    else {
        return Vec::new();
    };
    let trimmed = raw.trim_start();
    let fence = trimmed
        .chars()
        .next()
        .filter(|character| matches!(character, '`' | '~'))
        .map(|character| {
            let len = trimmed
                .chars()
                .take_while(|candidate| candidate == &character)
                .count();
            (character, len)
        })
        .filter(|(_, len)| *len >= 3);
    let (body_start, body_end) = if let Some((fence, fence_len)) = fence {
        let start = raw.find('\n').map_or(raw.len(), |newline| newline + 1);
        let last_line = raw.rfind('\n').map_or(start, |newline| newline + 1);
        let closing = raw[last_line..].trim();
        let is_closing = closing.chars().count() >= fence_len
            && closing.chars().all(|character| character == fence);
        (start, if is_closing { last_line } else { raw.len() })
    } else {
        (0, raw.len())
    };

    aligned_source_segments(
        &raw[body_start..body_end],
        code,
        span.start + body_start,
        false,
        false,
    )
}

fn mapped_inline(
    source: &str,
    text: impl Into<SharedString>,
    node: &mdast::Node,
    cx: &NodeContext,
) -> InlineNode {
    let text = text.into();
    let span = node.position().map(|position| Span {
        start: cx.offset + position.start.offset,
        end: cx.offset + position.end.offset,
    });
    let segments = source_segments(
        source,
        &text,
        span,
        cx.offset,
        matches!(node, Node::Text(_)),
    );
    InlineNode::new(text).source_segments(segments)
}

fn append_inline_html_blocks(paragraph: &mut Paragraph, blocks: Vec<BlockNode>) -> Option<String> {
    let mut text = String::new();

    for block in blocks {
        match block {
            BlockNode::Root { children, .. } => {
                text.push_str(&append_inline_html_blocks(paragraph, children)?);
            }
            BlockNode::Paragraph(html_paragraph) => {
                text.push_str(&html_paragraph.text());
                for child in html_paragraph.children {
                    paragraph.push(child);
                }
            }
            BlockNode::Break { .. } => {
                text.push('\n');
                paragraph.push(InlineNode::new("\n"));
            }
            _ => return None,
        }
    }

    Some(text)
}

fn parse_paragraph(
    source: &str,
    paragraph: &mut Paragraph,
    node: &mdast::Node,
    cx: &mut NodeContext,
) -> String {
    let span = node.position().map(|pos| Span {
        start: cx.offset + pos.start.offset,
        end: cx.offset + pos.end.offset,
    });
    if let Some(span) = span {
        paragraph.set_span(span);
    }

    let parse_cx = MarkdownParseContext::new(source, cx.offset);
    if let Some(mut custom) = cx.markdown_extensions.parse_inline(node, &parse_cx) {
        custom.set_span(span);
        let custom = custom.with_inline_source(parse_cx.node_source(node).unwrap_or_default());
        let text = custom.as_text().to_string();
        paragraph.push(InlineNode::custom(custom));
        return text;
    }

    let mut text = String::new();

    match node {
        Node::Paragraph(val) => {
            text.push_str(&parse_inline_children(source, paragraph, &val.children, cx));
        }
        Node::Text(val) => {
            // A CommonMark *soft* break lives inside this value as a plain
            // line ending. The renderer treats a newline in a text run as a
            // line break, so a paragraph hard-wrapped in the source would
            // render one visual line per source line instead of reflowing to
            // the available width. Collapse soft breaks to spaces; *hard*
            // breaks never reach here, they arrive as their own Node::Break.
            //
            // mdast hands the line ending over exactly as the source wrote it,
            // so a CRLF document still carries its carriage return here. Take
            // the CR with the newline: dropping only the newline would strand
            // the CR in the middle of the reflowed line.
            text = val.value.replace("\r\n", " ").replace(['\n', '\r'], " ");
            paragraph.push(mapped_inline(source, text.clone(), node, cx))
        }
        Node::Emphasis(val) => {
            text = merge_children_with_mark(
                source,
                paragraph,
                &val.children,
                TextMark::default().italic(),
                cx,
            );
        }
        Node::Strong(val) => {
            text = merge_children_with_mark(
                source,
                paragraph,
                &val.children,
                TextMark::default().bold(),
                cx,
            );
        }
        Node::Delete(val) => {
            text = merge_children_with_mark(
                source,
                paragraph,
                &val.children,
                TextMark::default().strikethrough(),
                cx,
            );
        }
        Node::InlineCode(val) => {
            text = val.value.clone();
            let span = node.position().map(|position| Span {
                start: cx.offset + position.start.offset,
                end: cx.offset + position.end.offset,
            });
            paragraph.push(
                InlineNode::new(text.clone())
                    .source_segments(code_source_segments(source, &text, span, cx.offset))
                    .marks(vec![(0..text.len(), TextMark::default().code())]),
            );
        }
        Node::Link(val) => {
            let link_mark = Some(LinkMark {
                url: val.url.clone().into(),
                title: val.title.clone().map(|s| s.into()),
                ..Default::default()
            });
            let leaves_app = is_web_url(&val.url) && cx.markdown_extensions.external_link_arrow_on();

            text = merge_children_with_mark(
                source,
                paragraph,
                &val.children,
                TextMark {
                    link: link_mark,
                    ..Default::default()
                },
                cx,
            );
            // The arrow is text in the link's own run, so the paragraph stays plain text (cached
            // layout) and the arrow wraps, dims and clicks with the label. A link whose label is
            // only images has no text to end in it.
            if leaves_app
                && !text.trim().is_empty()
                && let Some(last) = paragraph.children.last_mut()
                && last.custom.is_none()
                && last.image.is_none()
                && let Some((range, mark)) = last.marks.last_mut()
                && mark.link.is_some()
                && range.end == last.text.len()
            {
                let mut with_arrow = last.text.to_string();
                with_arrow.push_str(EXTERNAL_LINK_ARROW);
                range.end = with_arrow.len();
                text.push_str(EXTERNAL_LINK_ARROW);
                last.text = with_arrow.into();
            }
        }
        Node::Image(raw) => {
            paragraph.push_image(ImageNode {
                url: raw.url.clone().into(),
                title: raw.title.clone().map(|t| t.into()),
                alt: Some(raw.alt.clone().into()),
                span: raw.position.as_ref().map(|position| Span {
                    start: cx.offset + position.start.offset,
                    end: cx.offset + position.end.offset,
                }),
                ..Default::default()
            });
        }
        Node::Break(_) => {
            // Hard line break (trailing two spaces / backslash). Mirror the
            // inline-HTML <br> path: emit a newline inline node so the break
            // renders instead of silently concatenating adjacent lines.
            text.push('\n');
            paragraph.push(mapped_inline(source, "\n", node, cx));
        }
        Node::InlineMath(raw) => {
            // Math parsing is on by default, so ordinary prose that merely
            // contains dollar signs ("spent $5 and $10") arrives here as a
            // math node whenever no inline plugin claimed it. Emitting
            // `raw.value` would drop the delimiters and restyle the run as
            // code, silently rewriting what the author typed. Fall back to the
            // original source so unclaimed math stays literal.
            text = parse_cx
                .node_source(node)
                .map(str::to_string)
                .unwrap_or_else(|| raw.value.clone());
            paragraph.push(mapped_inline(source, text.clone(), node, cx));
        }
        Node::MdxTextExpression(raw) => {
            text = raw.value.clone();
            let span = node.position().map(|position| Span {
                start: cx.offset + position.start.offset,
                end: cx.offset + position.end.offset,
            });
            paragraph.push(
                InlineNode::new(text.clone())
                    .source_segments(code_source_segments(source, &text, span, cx.offset))
                    .marks(vec![(0..text.len(), TextMark::default())]),
            );
        }
        Node::Html(val) => match super::html::parse(&val.value, cx) {
            Ok(el) => {
                if let Some(inline_text) =
                    append_inline_html_blocks(paragraph, Arc::unwrap_or_clone(el.blocks))
                {
                    text = inline_text;
                } else {
                    if cfg!(debug_assertions) {
                        tracing::warn!("unsupported inline html tag: {:#?}", val.value);
                    }
                }
            }
            Err(err) => {
                if cfg!(debug_assertions) {
                    tracing::warn!("failed parsing html: {:#?}", err);
                }

                text.push_str(&val.value);
            }
        },
        Node::FootnoteReference(foot) => {
            let numbered = cx.footnote_numbers.get(&foot.identifier).map(|(number, _)| *number);
            match numbered {
                // The reference is its note's number, raised and linked to the note. It
                // stands for the whole `[^id]` of the source.
                Some(number) => {
                    let label = number.to_string();
                    let span = node.position().map(|position| Span {
                        start: cx.offset + position.start.offset,
                        end: cx.offset + position.end.offset,
                    });
                    let segments = span
                        .map(|span| {
                            vec![SourceSegment {
                                rendered: 0..label.len(),
                                source: span.start..span.end,
                                linear: false,
                            }]
                        })
                        .unwrap_or_default();
                    paragraph.push(InlineNode::new(label.clone()).source_segments(segments).marks(vec![(
                        0..label.len(),
                        TextMark {
                            raised: true,
                            link: Some(LinkMark {
                                url: format!("#fn-{number}").into(),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )]));
                    text = label;
                }
                None => {
                    let prefix = format!("[{}]", foot.identifier);
                    paragraph.push(mapped_inline(source, prefix.clone(), node, cx).marks(vec![(
                        0..prefix.len(),
                        TextMark {
                            italic: true,
                            ..Default::default()
                        },
                    )]));
                }
            }
        }
        Node::LinkReference(link) => {
            let link_mark = LinkMark {
                url: "".into(),
                title: link.label.clone().map(Into::into),
                identifier: Some(link.identifier.clone().into()),
            };

            text = merge_children_with_mark(
                source,
                paragraph,
                &link.children,
                TextMark {
                    link: Some(link_mark),
                    ..Default::default()
                },
                cx,
            );
        }
        _ => {
            if cfg!(debug_assertions) {
                tracing::warn!("unsupported inline node: {:#?}", node);
            }
        }
    }

    text
}

fn ast_to_document(source: &str, root: mdast::Node, cx: &mut NodeContext) -> ParsedDocument {
    let root = match root {
        Node::Root(r) => r,
        _ => panic!("expected root node"),
    };

    number_footnotes(&root.children, cx);
    let mut blocks: Vec<BlockNode> = Vec::with_capacity(root.children.len());
    // The notes in source order, which the section at the end sorts by number.
    let mut notes: Vec<(usize, Vec<BlockNode>)> = Vec::new();
    for child in root.children {
        let Node::FootnoteDefinition(def) = child else {
            blocks.push(ast_to_node(source, child, cx));
            continue;
        };
        // The definition stays where it was written, drawing nothing, with its own span, so
        // scroll sync keeps mapping the editor's lines to blocks in source order; the note
        // itself is drawn in the section at the end.
        let span = new_span(def.position.clone(), cx);
        if let Some(&(number, _)) = cx.footnote_numbers.get(&def.identifier)
            && !notes.iter().any(|(taken, _)| *taken == number)
        {
            let children = def
                .children
                .into_iter()
                .map(|c| ast_to_node(source, c, cx))
                .collect();
            notes.push((number, children));
        }
        blocks.push(BlockNode::Definition {
            identifier: format!("^{}", def.identifier).into(),
            url: SharedString::default(),
            title: None,
            span,
        });
    }
    notes.sort_by_key(|(number, _)| *number);
    let last = notes.last().map_or(0, |(number, _)| *number);
    for (number, children) in notes {
        let ref_start = cx
            .footnote_numbers
            .values()
            .find(|(taken, _)| *taken == number)
            .and_then(|(_, start)| *start);
        blocks.push(BlockNode::Footnote {
            number,
            ref_start,
            children,
            last,
        });
    }
    ParsedDocument {
        source: source.to_string().into(),
        blocks: Arc::new(blocks),
    }
}

/// Gives each footnote that has a top-level definition a number, in order of first
/// reference; a definition nobody references is numbered after those, in source order.
fn number_footnotes(top: &[Node], cx: &mut NodeContext) {
    fn references(node: &Node, offset: usize, found: &mut Vec<(String, usize)>) {
        if let Node::FootnoteReference(foot) = node {
            let at = node.position().map_or(0, |position| position.start.offset);
            found.push((foot.identifier.clone(), offset + at));
        }
        for child in node.children().into_iter().flatten() {
            references(child, offset, found);
        }
    }

    cx.footnote_numbers.clear();
    let defined: Vec<&str> = top
        .iter()
        .filter_map(|node| match node {
            Node::FootnoteDefinition(def) => Some(def.identifier.as_str()),
            _ => None,
        })
        .collect();
    if defined.is_empty() {
        return;
    }
    let mut found = Vec::new();
    for node in top {
        references(node, cx.offset, &mut found);
    }
    for (identifier, start) in found {
        if defined.contains(&identifier.as_str()) && !cx.footnote_numbers.contains_key(&identifier) {
            let number = cx.footnote_numbers.len() + 1;
            cx.footnote_numbers.insert(identifier, (number, Some(start)));
        }
    }
    for identifier in defined {
        if !cx.footnote_numbers.contains_key(identifier) {
            let number = cx.footnote_numbers.len() + 1;
            cx.footnote_numbers.insert(identifier.to_string(), (number, None));
        }
    }
}

/// When the quote's first line is `[!TYPE]` (any case, with an optional title after it on the
/// same line), takes that line out of `children` and says what kind of callout it makes.
fn take_callout(source: &str, children: &mut Vec<Node>) -> Option<Callout> {
    let Some(Node::Paragraph(paragraph)) = children.first_mut() else {
        return None;
    };
    let ends_line = paragraph.children.len() == 1
        || matches!(paragraph.children.get(1), Some(Node::Break(_)));
    let Some(Node::Text(text)) = paragraph.children.first_mut() else {
        return None;
    };
    // An escaped `\[!NOTE]` is plain text.
    if let Some(start) = text.position.as_ref().map(|position| position.start.offset)
        && (source.as_bytes().get(start) == Some(&b'\\')
            || start.checked_sub(1).and_then(|before| source.as_bytes().get(before)) == Some(&b'\\'))
    {
        return None;
    }
    let rest = text.value.strip_prefix("[!")?;
    let close = rest.find(']')?;
    let kind = CalloutKind::from_marker(&rest[..close])?;
    let after = &rest[close + 1..];
    let (line, body) = match after.find('\n') {
        Some(ix) => (&after[..ix], Some(&after[ix + 1..])),
        None => (after, None),
    };
    if !(line.is_empty() || line.starts_with([' ', '\t', '\r'])) {
        return None;
    }
    // A title is the rest of the marker's own line, as plain text; more inline nodes after
    // it (emphasis, a link) are the body's start, not a title.
    let titled = body.is_some() || ends_line;
    let title = line.trim();
    let title = (titled && !title.is_empty()).then(|| SharedString::from(title.to_string()));
    let keep_line = !titled && !line.trim().is_empty();

    match body.filter(|body| !body.is_empty()) {
        Some(body) => {
            let consumed = text.value.len() - body.len();
            let start = text.position.as_ref().map(|position| position.start.offset);
            text.value = body.to_string();
            // The text now starts on the quote's next line, after its `> `.
            if let (Some(position), Some(start)) = (text.position.as_mut(), start) {
                let mut at = start + consumed;
                if let Some(tail) = source.get(at..) {
                    let blanks = tail.len() - tail.trim_start_matches(' ').len();
                    let mut skip = blanks;
                    if tail[blanks..].starts_with('>') {
                        skip += 1;
                        if tail[skip..].starts_with(' ') {
                            skip += 1;
                        }
                    }
                    at += skip;
                }
                position.start.offset = at;
            }
        }
        None if keep_line => {
            let marker_len = 3 + close;
            text.value = text.value[marker_len..].trim_start().to_string();
        }
        None => {
            paragraph.children.remove(0);
            // A hard break closing the marker's line goes with it.
            if matches!(paragraph.children.first(), Some(Node::Break(_))) {
                paragraph.children.remove(0);
            }
            if paragraph.children.is_empty() {
                children.remove(0);
            }
        }
    }
    Some(Callout { kind, title })
}

/// The range of `old` an edit replaced, as `(start, old_end)`: `old` and `new`
/// share their first `start` bytes and the bytes after `old_end` in `old` (and
/// the same count of bytes at the end of `new`). Both sit on character
/// boundaries of both texts.
fn changed_range(old: &str, new: &str) -> (usize, usize) {
    let (old_bytes, new_bytes) = (old.as_bytes(), new.as_bytes());
    let mut start = old_bytes
        .iter()
        .zip(new_bytes)
        .take_while(|(a, b)| a == b)
        .count();
    while !(old.is_char_boundary(start) && new.is_char_boundary(start)) {
        start -= 1;
    }
    let max_tail = old_bytes.len().min(new_bytes.len()) - start;
    let mut tail = old_bytes
        .iter()
        .rev()
        .zip(new_bytes.iter().rev())
        .take(max_tail)
        .take_while(|(a, b)| a == b)
        .count();
    while !(old.is_char_boundary(old.len() - tail) && new.is_char_boundary(new.len() - tail)) {
        tail -= 1;
    }
    (start, old.len() - tail)
}

/// Whether `text` holds a construct that can swallow the blocks after it
/// (an unclosed fence, an HTML block or comment, a math fence), or that
/// depends on the whole document (front matter).
fn may_reach_past_its_block(text: &str, at_document_start: bool) -> bool {
    if text.contains("```") || text.contains("~~~") || text.contains("$$") {
        return true;
    }
    if at_document_start && (text.starts_with("---") || text.starts_with("+++")) {
        return true;
    }
    // A line of dashes or equals signs underlines the paragraph above it (a
    // setext heading) or is a rule.
    let underline = |line: &str| {
        let line = line.trim();
        !line.is_empty() && (line.bytes().all(|b| b == b'-') || line.bytes().all(|b| b == b'='))
    };
    if text.lines().any(underline) {
        return true;
    }
    text.match_indices('<').any(|(ix, _)| {
        text[ix + 1..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, '!' | '?' | '/'))
    })
}

/// Whether an edit of `old_slice` into `new_slice` can change which references there are or
/// what they point at: either slice holds a link reference definition or a footnote label
/// (`]:`, `[^`). A reference use alone cannot: the slice is parsed with the document's other
/// definitions in scope (see [`definitions_outside`]).
fn decides_references(old_slice: &str, new_slice: &str) -> bool {
    let marks = |slice: &str| slice.contains("]:") || slice.contains("[^");
    marks(old_slice) || marks(new_slice)
}

/// The link definitions of the blocks outside `lo..hi`, one per line, to parse after a slice
/// so the references in it resolve as they do in the whole document. `None` when the
/// definitions cannot be handed over as they stand: a `]:` is somewhere that is not a
/// top-level definition (a definition in a list or quote, or `]:` in prose or code, which
/// may be one), or two definitions share a label, where which one is first decides.
fn definitions_outside(
    old_text: &str,
    blocks: &[BlockNode],
    lo: usize,
    hi: usize,
) -> Option<String> {
    for (at, _) in old_text.match_indices("]:") {
        let ix = blocks.partition_point(|block| block.span().is_some_and(|span| span.end <= at));
        let block = blocks.get(ix)?;
        let BlockNode::Definition { identifier, span, .. } = block else {
            return None;
        };
        let start = span.as_ref()?.start;
        if start > at {
            return None;
        }
        // A footnote definition holds blocks of its own, which may hold a definition; only
        // the `]:` that ends its label is its own.
        if identifier.starts_with('^') {
            let label = old_text.get(start..at)?;
            if !label.starts_with("[^") || label.contains([']', '\n']) {
                return None;
            }
        }
    }
    let mut labels = std::collections::HashSet::new();
    let mut definitions = String::new();
    for (ix, block) in blocks.iter().enumerate() {
        let BlockNode::Definition { identifier, span, .. } = block else {
            continue;
        };
        if identifier.starts_with('^') {
            continue;
        }
        if !labels.insert(identifier) {
            return None;
        }
        if (lo..hi).contains(&ix) {
            continue;
        }
        let span = (*span)?;
        definitions.push_str(old_text.get(span.start..span.end)?);
        definitions.push('\n');
    }
    Some(definitions)
}

/// Parse `text`, an edit of the document `old` was parsed from, by parsing
/// again only the blocks the edit touches, each with the block next to it.
///
/// The result equals `parse(text, ..)`: the blocks before the edit are kept,
/// those after it are kept with their source positions moved by the change
/// in length, and the blocks between are parsed on their own with positions
/// offset to where they sit in `text`.
///
/// Returns `None` whenever block boundaries could move beyond that, so the
/// caller parses the whole text: the edit or the slice parsed again holds
/// something that reaches past its block (see [`may_reach_past_its_block`]),
/// a block has no span or a custom node, the blocks around the edit are not
/// separated by blank space, or the blocks parsed again do not line up with
/// the old ones in number and edges (a split, a join, a block added or
/// removed). A document's notes section has no source of its own: it is kept as it was,
/// moved with the text, when the edit touches no footnote label.
pub(crate) fn parse_edit(
    old: &ParsedDocument,
    text: &str,
    cx: &mut NodeContext,
) -> Option<ParsedDocument> {
    let old_text: &str = &old.source;
    // The notes come last, after every block that has a source.
    let notes_at = old
        .blocks
        .iter()
        .position(|block| matches!(block, BlockNode::Footnote { .. }))
        .unwrap_or(old.blocks.len());
    let (blocks, notes) = old.blocks.split_at(notes_at);
    if notes.iter().any(|block| !matches!(block, BlockNode::Footnote { .. })) {
        return None;
    }
    if blocks.is_empty() || cx.markdown_extensions.mdx_enabled() {
        return None;
    }

    // Top-level blocks of a Markdown parse are in order and cover the source
    // without overlap; anything else cannot be spliced.
    let mut previous_end = 0;
    for block in blocks {
        let span = block.span()?;
        if span.start < previous_end || span.end < span.start || span.end > old_text.len() {
            return None;
        }
        previous_end = span.end;
    }
    let span_of = |block: &BlockNode| block.span().unwrap_or_default();

    // The blocks from the one before the first touched by the edit to the one
    // after the last touched. A block the edit only touches the edge of, or a
    // blank line between blocks it changes, counts as touched.
    let (start, old_end) = changed_range(old_text, text);
    let first = blocks.partition_point(|block| span_of(block).end < start);
    let past = blocks.partition_point(|block| span_of(block).start <= old_end);
    let lo = first.saturating_sub(1);
    let hi = (past + 1).min(blocks.len());
    if lo >= hi {
        return None;
    }

    // The slice starts on a line of its own, after blank space, so its first
    // block parses as it did in context.
    let slice_start = if lo == 0 {
        0
    } else {
        let first_start = span_of(&blocks[lo]).start;
        let line_start = old_text.get(..first_start)?.rfind('\n').map_or(0, |ix| ix + 1);
        let before = old_text.get(span_of(&blocks[lo - 1]).end..line_start)?;
        if !before.trim().is_empty() || !old_text[line_start..first_start].trim().is_empty() {
            return None;
        }
        line_start
    };
    // It ends with the blank space after its last block, up to the line the
    // next block starts on, so that block ends as it did in context: a list
    // ends after its last line break only when something follows it.
    let old_slice_end = if hi == blocks.len() {
        old_text.len()
    } else {
        let last_end = span_of(&blocks[hi - 1]).end;
        let between = old_text.get(last_end..span_of(&blocks[hi]).start)?;
        if !between.trim().is_empty() {
            return None;
        }
        match between.rfind('\n') {
            Some(ix) => last_end + ix + 1,
            None if old_text[..last_end].ends_with('\n') => last_end,
            None => return None,
        }
    };
    if slice_start > start || old_end > old_slice_end {
        return None;
    }

    let delta = text.len() as isize - old_text.len() as isize;
    let new_slice_end = old_slice_end.checked_add_signed(delta)?;
    let old_slice = old_text.get(slice_start..old_slice_end)?;
    let new_slice = text.get(slice_start..new_slice_end)?;
    // A block the slice only holds as its neighbour is parsed again as a
    // check and must come out equal (below), and cannot reach past itself
    // unless the edit gives it something to reach with. So the constructs
    // that reach past a block are looked for only in the lines of the blocks
    // the edit touches, in both texts, not in the neighbours.
    let touched_start = if first < past { span_of(&blocks[first]).start.min(start) } else { start };
    let touched_end = if first < past { span_of(&blocks[past - 1]).end.max(old_end) } else { old_end };
    let region_start = old_text
        .get(slice_start..touched_start)?
        .rfind('\n')
        .map_or(slice_start, |ix| slice_start + ix + 1);
    let line_end = |text: &str, from: usize, limit: usize| {
        text.get(from..limit)
            .map(|rest| rest.find('\n').map_or(limit, |ix| from + ix))
    };
    let old_region = old_text.get(region_start..line_end(old_text, touched_end, old_slice_end)?)?;
    let new_touched_end = touched_end.checked_add_signed(delta)?;
    let new_region = text.get(region_start..line_end(text, new_touched_end, new_slice_end)?)?;
    // A slice that starts on a rule is parsed as if it opened the document,
    // where `---` can open front matter.
    let opens_like_front_matter =
        slice_start > 0 && (old_slice.starts_with("---") || old_slice.starts_with("+++"));
    // A pipe ending a blockquote line leaves the parser's table state set,
    // and lines that skip the table attempt (a fence, `$$`, a rule, a
    // heading, HTML and the like) keep it set, so a later pipe-led table is
    // read as a paragraph in a full parse. A pipe in the slice with such a
    // line in it, or a pipe above the slice when the touched lines change
    // which lines skip, falls back.
    fn skips_line(line: &str) -> bool {
        let columns = line
            .chars()
            .take_while(|c| c.is_whitespace())
            .map(|c| if c == '\t' { 4 } else { 1 })
            .sum::<usize>();
        if columns >= 4 {
            return true;
        }
        let rule_line = line.trim_start_matches(|c: char| c == '>' || c.is_whitespace());
        let mut line = rule_line;
        if let Some(rest) = line
            .strip_prefix(['-', '*', '+'])
            .filter(|rest| rest.starts_with(' '))
        {
            line = rest.trim_start();
        } else {
            let digits = line.bytes().take_while(u8::is_ascii_digit).count();
            if digits > 0
                && let Some(rest) = line[digits..].strip_prefix(['.', ')'])
                && rest.starts_with(' ')
            {
                line = rest.trim_start();
            }
        }
        if line.starts_with(['`', '~', '$', '#', '*', '_', 'e', 'i', '{', '<']) {
            return true;
        }
        let mut marks = rule_line.bytes().filter(|b| !matches!(b, b' ' | b'\t'));
        marks.next().is_some_and(|first| {
            matches!(first, b'-' | b'*' | b'_')
                && marks.clone().count() >= 2
                && marks.all(|b| b == first)
        })
    }
    let skips_table = |text: &str| text.lines().any(skips_line);
    let leak_above = old_text[..slice_start].contains('|')
        && !old_region
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(skips_line)
            .eq(new_region.lines().filter(|line| !line.trim().is_empty()).map(skips_line));
    let pipe_by_fence = leak_above
        || ((old_slice.contains('|') || new_slice.contains('|'))
            && (skips_table(old_slice) || skips_table(new_slice)));
    if opens_like_front_matter
        || pipe_by_fence
        || may_reach_past_its_block(old_region, region_start == 0)
        || may_reach_past_its_block(new_region, region_start == 0)
        || decides_references(old_region, new_region)
    {
        return None;
    }

    // A reference use parses as a link only where its definition is in scope, so a slice
    // with a `[` parses with the document's other definitions after it, and the blocks
    // they make are dropped again below.
    let definitions = if (old_slice.contains('[') || new_slice.contains('['))
        && old_text.contains("]:")
    {
        definitions_outside(old_text, blocks, lo, hi)?
    } else {
        String::new()
    };
    let offset = cx.offset;
    cx.offset = slice_start;
    let parsed = if definitions.is_empty() {
        parse(new_slice, cx)
    } else {
        parse(&format!("{new_slice}\n\n{definitions}"), cx)
    };
    cx.offset = offset;
    let mut parsed = Arc::unwrap_or_clone(parsed.ok()?.blocks);
    // The notes section of the slice and the definitions added to it are not the slice's.
    while parsed.last().is_some_and(|block| match block.span() {
        None => matches!(block, BlockNode::Footnote { .. }),
        Some(span) => !definitions.is_empty() && span.start >= new_slice_end,
    }) {
        parsed.pop();
    }

    // The blocks the edit touched are the ones that change; the blocks next
    // to them, which were only parsed again as a check, must come out as
    // before, so there are as many as there were and the slice ends where its
    // last old block did. An edit that splits, joins, adds or removes a block
    // moves block boundaries, and parses in full.
    let (Some(first_new), Some(last_new)) = (parsed.first(), parsed.last()) else {
        return None;
    };
    let (first_new, last_new) = (first_new.span()?, last_new.span()?);
    if parsed.len() != hi - lo
        || first_new.start != span_of(&blocks[lo]).start
        || Some(last_new.end) != span_of(&blocks[hi - 1]).end.checked_add_signed(delta)
    {
        return None;
    }
    // Matching edges are not enough: a block the edit did not touch can turn
    // into another kind there (a paragraph under a heading that becomes a
    // lazy list continuation). The block before the touched ones (`lo <
    // first`) and the block after them (`hi - 1 == past`, unless the slice
    // ends the text) must come out equal to the old ones, moved by `delta`.
    if lo < first && parsed.first() != Some(&blocks[lo]) {
        return None;
    }
    if hi < blocks.len() {
        let mut expected = blocks[hi - 1].clone();
        if (delta != 0 && !expected.shift_source(delta)) || parsed.last() != Some(&expected) {
            return None;
        }
    }
    let mut previous_end = slice_start;
    for block in &parsed {
        let span = block.span()?;
        if span.start < previous_end || span.end > new_slice_end {
            return None;
        }
        previous_end = span.end;
    }

    let mut spliced = Vec::with_capacity(lo + parsed.len() + blocks.len() - hi + notes.len());
    spliced.extend_from_slice(&blocks[..lo]);
    spliced.extend(parsed);
    for block in &blocks[hi..] {
        let mut block = block.clone();
        if delta != 0 && !block.shift_source(delta) {
            return None;
        }
        spliced.push(block);
    }
    // The notes section is the old one: no slice holds a footnote label in the lines the
    // edit touches, so no note, number or first reference changes, and each only moves.
    for note in notes {
        spliced.push(moved_note(note, delta, start, old_end)?);
    }
    Some(ParsedDocument {
        source: text.to_string().into(),
        blocks: Arc::new(spliced),
    })
}

/// The footnote `note`, moved for an edit that replaced `start..old_end` of the source and
/// changed its length by `delta`: the source positions it holds (its first reference and its
/// blocks, which sit where the definition was written) that lie after the edit shift with
/// it. A position the edit covers cannot be moved, so the caller parses in full.
fn moved_note(note: &BlockNode, delta: isize, start: usize, old_end: usize) -> Option<BlockNode> {
    let BlockNode::Footnote {
        number,
        ref_start,
        children,
        last,
    } = note
    else {
        return None;
    };
    let ref_start = match *ref_start {
        Some(at) if at >= old_end => Some(at.checked_add_signed(delta)?),
        Some(at) if at < start => Some(at),
        Some(_) => return None,
        None => None,
    };
    let mut moved = Vec::with_capacity(children.len());
    for child in children {
        let mut child = child.clone();
        let span = child.span()?;
        if span.start >= old_end {
            if !child.shift_source(delta) {
                return None;
            }
        } else if span.end > start {
            return None;
        }
        moved.push(child);
    }
    Some(BlockNode::Footnote {
        number: *number,
        ref_start,
        children: moved,
        last: *last,
    })
}

fn new_span(pos: Option<markdown::unist::Position>, cx: &NodeContext) -> Option<Span> {
    let pos = pos?;

    Some(Span {
        start: cx.offset + pos.start.offset,
        end: cx.offset + pos.end.offset,
    })
}

fn ast_to_node(source: &str, value: mdast::Node, cx: &mut NodeContext) -> BlockNode {
    let span = new_span(value.position().cloned(), cx);
    let parse_cx = MarkdownParseContext::new(source, cx.offset);
    if let Some(mut node) = cx.markdown_extensions.parse_block(&value, &parse_cx) {
        node.set_span(span);
        return BlockNode::Custom {
            node,
            state: Default::default(),
        };
    }

    match value {
        Node::Root(_) => unreachable!("node::Root should be handled separately"),
        Node::Paragraph(val) => {
            let mut paragraph = Paragraph::default();
            parse_inline_children(source, &mut paragraph, &val.children, cx);
            paragraph.span = new_span(val.position, cx);
            BlockNode::Paragraph(paragraph)
        }
        Node::Blockquote(mut val) => {
            let callout = take_callout(source, &mut val.children);
            let children = val
                .children
                .into_iter()
                .map(|c| ast_to_node(source, c, cx))
                .collect();
            BlockNode::Blockquote {
                children,
                callout,
                span: new_span(val.position, cx),
            }
        }
        Node::List(list) => {
            let children = list
                .children
                .into_iter()
                .map(|c| ast_to_node(source, c, cx))
                .collect();
            BlockNode::List {
                ordered: list.ordered,
                start: list.start,
                children,
                span: new_span(list.position, cx),
            }
        }
        Node::ListItem(val) => {
            let children = val
                .children
                .into_iter()
                .map(|c| ast_to_node(source, c, cx))
                .collect();
            BlockNode::ListItem {
                children,
                spread: val.spread,
                checked: val.checked,
                span: new_span(val.position, cx),
            }
        }
        Node::Break(val) => BlockNode::Break {
            html: false,
            span: new_span(val.position, cx),
        },
        Node::Code(raw) => {
            let span = new_span(raw.position, cx);
            let segments = code_source_segments(source, &raw.value, span, cx.offset);
            BlockNode::CodeBlock(
                CodeBlock::new(raw.value.into(), raw.lang.map(Into::into), span)
                    .with_meta(raw.meta.map(Into::into))
                    .source_segments(segments),
            )
        }
        Node::Heading(val) => {
            let mut paragraph = Paragraph::default();
            parse_inline_children(source, &mut paragraph, &val.children, cx);

            BlockNode::Heading {
                level: val.depth,
                children: paragraph,
                span: new_span(val.position, cx),
            }
        }
        Node::Math(val) => {
            let span = new_span(val.position, cx);
            let segments = code_source_segments(source, &val.value, span, cx.offset);
            BlockNode::CodeBlock(
                CodeBlock::new(val.value.into(), None, span).source_segments(segments),
            )
        }
        Node::Html(val) => match super::html::parse(&val.value, cx) {
            Ok(el) => BlockNode::Root {
                children: Arc::unwrap_or_clone(el.blocks),
                span: new_span(val.position, cx),
            },
            Err(err) => {
                if cfg!(debug_assertions) {
                    tracing::warn!("error parsing html: {:#?}", err);
                }

                BlockNode::Paragraph(Paragraph::new(val.value))
            }
        },
        Node::MdxFlowExpression(val) => {
            let span = new_span(val.position, cx);
            let segments = code_source_segments(source, &val.value, span, cx.offset);
            BlockNode::CodeBlock(
                CodeBlock::new(val.value.into(), Some("mdx".into()), span)
                    .source_segments(segments),
            )
        }
        Node::Yaml(val) => BlockNode::CodeBlock(CodeBlock::new(
            val.value.into(),
            Some("yml".into()),
            new_span(val.position, cx),
        )),
        Node::Toml(val) => BlockNode::CodeBlock(CodeBlock::new(
            val.value.into(),
            Some("toml".into()),
            new_span(val.position, cx),
        )),
        Node::MdxJsxTextElement(val) => {
            let mut paragraph = Paragraph::default();
            parse_inline_children(source, &mut paragraph, &val.children, cx);
            paragraph.span = new_span(val.position, cx);
            BlockNode::Paragraph(paragraph)
        }
        Node::MdxJsxFlowElement(val) => {
            let mut paragraph = Paragraph::default();
            parse_inline_children(source, &mut paragraph, &val.children, cx);
            paragraph.span = new_span(val.position, cx);
            BlockNode::Paragraph(paragraph)
        }
        Node::ThematicBreak(val) => BlockNode::HorizontalRule {
            span: new_span(val.position, cx),
        },
        Node::Table(val) => {
            let mut table = Table::default();
            table.column_aligns = val
                .align
                .clone()
                .into_iter()
                .map(|align| align.into())
                .collect();
            val.children.iter().for_each(|c| {
                if let Node::TableRow(row) = c {
                    parse_table_row(source, &mut table, row, cx);
                }
            });
            table.span = new_span(val.position, cx);

            BlockNode::Table(table)
        }
        Node::FootnoteDefinition(def) => {
            let mut paragraph = Paragraph::default();
            let prefix = format!("[{}]: ", def.identifier);
            paragraph.push(InlineNode::new(&prefix).marks(vec![(
                0..prefix.len(),
                TextMark {
                    italic: true,
                    ..Default::default()
                },
            )]));

            parse_inline_children(source, &mut paragraph, &def.children, cx);
            paragraph.span = new_span(def.position, cx);
            BlockNode::Paragraph(paragraph)
        }
        Node::Definition(def) => {
            cx.add_ref(
                def.identifier.clone().into(),
                LinkMark {
                    url: def.url.clone().into(),
                    identifier: Some(def.identifier.clone().into()),
                    title: def.title.clone().map(Into::into),
                },
            );

            BlockNode::Definition {
                identifier: def.identifier.clone().into(),
                url: def.url.clone().into(),
                title: def.title.clone().map(|s| s.into()),
                span: new_span(def.position, cx),
            }
        }
        _ => {
            if cfg!(debug_assertions) {
                tracing::warn!("unsupported node: {:#?}", value);
            }
            BlockNode::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::ParentElement;

    use crate::text::{MarkdownExtensions, MarkdownNode, MarkdownPlugin};

    fn first_paragraph(block: &BlockNode) -> Option<&Paragraph> {
        match block {
            BlockNode::Paragraph(paragraph)
            | BlockNode::Heading {
                children: paragraph,
                ..
            } => Some(paragraph),
            BlockNode::Root { children, .. }
            | BlockNode::Blockquote { children, .. }
            | BlockNode::List { children, .. }
            | BlockNode::ListItem { children, .. } => children.iter().find_map(first_paragraph),
            _ => None,
        }
    }

    fn first_code_block(block: &BlockNode) -> Option<&CodeBlock> {
        match block {
            BlockNode::CodeBlock(code) => Some(code),
            BlockNode::Root { children, .. }
            | BlockNode::Blockquote { children, .. }
            | BlockNode::List { children, .. }
            | BlockNode::ListItem { children, .. } => children.iter().find_map(first_code_block),
            _ => None,
        }
    }

    fn selected_rendered_range(source: &str, selection: Range<usize>) -> Option<Range<usize>> {
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        let paragraph = document
            .blocks
            .iter()
            .find_map(first_paragraph)
            .expect("expected paragraph");
        let rendered = paragraph.text();
        let mut state = paragraph.state.lock().unwrap();
        state.set_text(rendered.into());
        state.selection = Some(selection.into());
        drop(state);
        document.selected_source_range()
    }

    fn select_rendered_range(source: &str, selection: Range<usize>) -> Range<usize> {
        selected_rendered_range(source, selection).expect("source range")
    }

    fn selected_code_range(source: &str, selection: Range<usize>) -> Option<Range<usize>> {
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        let code = document
            .blocks
            .iter()
            .find_map(first_code_block)
            .expect("expected code block");
        code.set_selection(selection);
        document.selected_source_range()
    }

    fn selected_mdx_rendered_range(source: &str, selection: Range<usize>) -> Option<Range<usize>> {
        let mut cx = NodeContext {
            markdown_extensions: Arc::new(MarkdownExtensions::default().mdx()),
            ..Default::default()
        };
        let document = parse(source, &mut cx).unwrap();
        let paragraph = document
            .blocks
            .iter()
            .find_map(first_paragraph)
            .expect("expected MDX paragraph");
        let rendered = paragraph.text();
        let mut state = paragraph.state.lock().unwrap();
        state.set_text(rendered.into());
        state.selection = Some(selection.into());
        drop(state);
        document.selected_source_range()
    }

    fn selected_mdx_code(source: &str, selected_text: &str) -> Option<Range<usize>> {
        let mut cx = NodeContext {
            markdown_extensions: Arc::new(MarkdownExtensions::default().mdx()),
            ..Default::default()
        };
        let document = parse(source, &mut cx).unwrap();
        let code = document
            .blocks
            .iter()
            .find_map(first_code_block)
            .expect("expected MDX code block");
        let code_text = code.code();
        let start = code_text.find(selected_text).expect("selected MDX code");
        code.set_selection(start..start + selected_text.len());
        document.selected_source_range()
    }

    #[test]
    fn table_rows_keep_missing_cells_implicit() {
        let mut cx = NodeContext::default();
        let document = parse(
            "| A | B | C |\n| --- | --- | --- |\n| one |\n| two | three | four | ignored |\n| five | | six |\n",
            &mut cx,
        )
        .unwrap();
        let BlockNode::Table(table) = &document.blocks[0] else {
            panic!("expected table");
        };
        assert_eq!(table.column_aligns.len(), 3);
        assert_eq!(
            table
                .children
                .iter()
                .map(|row| row.children.len())
                .collect::<Vec<_>>(),
            [3, 1, 3, 3]
        );
        assert_eq!(table.children[1].children[0].children.text(), "one");
        assert_eq!(table.children[2].children[2].children.text(), "four");
        assert!(table.children[3].children[1].children.text().is_empty());
        assert_eq!(table.children[3].children[2].children.text(), "six");
    }

    #[test]
    fn sparse_table_cell_count_tracks_source_cells() {
        for (columns, rows) in [(32, 64), (64, 128)] {
            let source = format!(
                "{}|\n{}|\n{}",
                "|h".repeat(columns),
                "|---".repeat(columns),
                "|x|\n".repeat(rows),
            );
            let mut cx = NodeContext::default();
            let document = parse(&source, &mut cx).unwrap();
            let BlockNode::Table(table) = &document.blocks[0] else {
                panic!("expected table");
            };
            assert_eq!(table.column_aligns.len(), columns);
            assert_eq!(table.children.len(), rows + 1);
            assert_eq!(table.children[0].children.len(), columns);
            assert!(
                table.children[1..]
                    .iter()
                    .all(|row| row.children.len() == 1)
            );
            assert_eq!(
                table
                    .children
                    .iter()
                    .map(|row| row.children.len())
                    .sum::<usize>(),
                columns + rows
            );
        }
    }

    #[test]
    fn selected_source_range_uses_the_selected_identical_styled_occurrence() {
        let source = "**same** then **same**";
        assert_eq!(select_rendered_range(source, 10..14), 16..20);
    }

    #[test]
    fn selected_source_range_maps_partial_styled_text() {
        let source = "**same** then **same**";
        assert_eq!(select_rendered_range(source, 11..13), 17..19);
    }

    #[test]
    fn source_segments_compact_contiguous_one_to_one_mappings() {
        let source = "plain text";
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        let paragraph = first_paragraph(&document.blocks[0]).unwrap();
        assert_eq!(
            paragraph.children[0].source_segments,
            vec![SourceSegment {
                rendered: 0..source.len(),
                source: 0..source.len(),
                linear: true,
            }]
        );

        assert_eq!(select_rendered_range(source, 2..7), 2..7);
    }

    #[test]
    fn source_alignment_compacts_long_whitespace_runs() {
        for raw in [
            "a ".repeat(16_384),
            format!("a{}b", " ".repeat(32_768)),
            format!("a{}b\n", " \t".repeat(16_384)),
        ] {
            let segments = aligned_source_segments(&raw, &raw, 7, true, true);
            assert_eq!(
                segments,
                vec![SourceSegment {
                    rendered: 0..raw.len(),
                    source: 7..7 + raw.len(),
                    linear: true,
                }]
            );
            assert!(
                segments.capacity() < 64,
                "compaction must not retain a per-character allocation"
            );
        }
    }

    #[test]
    fn source_alignment_preserves_multiline_code_and_final_newline() {
        let raw = "x\n".repeat(16_384);
        let rendered = &raw[..raw.len() - 1];
        assert_eq!(
            aligned_source_segments(&raw, rendered, 4, false, false),
            vec![SourceSegment {
                rendered: 0..rendered.len(),
                source: 4..4 + rendered.len(),
                linear: true,
            }]
        );
        assert_eq!(
            aligned_source_segments("a\n> \n", "a\n\n", 0, false, false),
            vec![
                SourceSegment {
                    rendered: 0..2,
                    source: 0..2,
                    linear: true,
                },
                SourceSegment {
                    rendered: 2..3,
                    source: 2..5,
                    linear: false,
                },
            ]
        );
    }

    #[test]
    fn source_alignment_keeps_soft_breaks_and_entities_atomic() {
        assert_eq!(
            aligned_source_segments("a \r\nb", "a b", 9, true, true),
            vec![
                SourceSegment {
                    rendered: 0..1,
                    source: 9..10,
                    linear: true,
                },
                SourceSegment {
                    rendered: 1..2,
                    source: 11..13,
                    linear: false,
                },
                SourceSegment {
                    rendered: 2..3,
                    source: 13..14,
                    linear: true,
                },
            ]
        );
        let entity = "&NotEqualTilde;";
        let decoded = "\u{2242}\u{338}";
        assert_eq!(
            aligned_source_segments(entity, decoded, 3, true, true),
            vec![SourceSegment {
                rendered: 0..decoded.len(),
                source: 3..3 + entity.len(),
                linear: false,
            }]
        );
    }

    #[test]
    fn source_alignment_resumes_after_unmapped_characters() {
        let raw = "abc".repeat(4_096);
        let mut positions = None;
        assert_eq!(source_char_offset(&raw, 0, 'b', &mut positions), Some(1));
        assert!(positions.is_none(), "successful scans need no index");
        assert_eq!(
            source_char_offset(&raw, 0, '\u{fffd}', &mut positions),
            None
        );
        assert!(positions.is_some(), "failed scans must not be repeated");
        assert_eq!(source_char_offset(&raw, 2, 'b', &mut positions), Some(4));
        assert_eq!(
            source_char_offset(&raw, raw.len(), 'a', &mut positions),
            None
        );

        let missing = "\u{fffd}".repeat(4_096);
        assert_eq!(
            aligned_source_segments(&raw, &format!("{missing}abc"), 5, true, true),
            vec![SourceSegment {
                rendered: missing.len()..missing.len() + 3,
                source: 5..8,
                linear: true,
            }]
        );
        assert_eq!(
            aligned_source_segments("&amp;z", &format!("{missing}&z"), 0, true, true),
            vec![
                SourceSegment {
                    rendered: missing.len()..missing.len() + 1,
                    source: 0..5,
                    linear: false,
                },
                SourceSegment {
                    rendered: missing.len() + 1..missing.len() + 2,
                    source: 5..6,
                    linear: true,
                },
            ]
        );
    }

    #[test]
    fn source_alignment_parses_long_text_and_code_without_selection() {
        let source = format!("{}end", "a ".repeat(4_096));
        let mut cx = NodeContext::default();
        let document = parse(&source, &mut cx).unwrap();
        let paragraph = first_paragraph(&document.blocks[0]).unwrap();
        assert_eq!(paragraph.text(), source);
        let segments = &paragraph.children[0].source_segments;
        assert_eq!(segments.len(), 1);
        assert!(segments.capacity() < 64);
        assert_eq!(segments[0].source, 0..source.len());
        assert_eq!(
            selected_rendered_range(&source, source.len() - 3..source.len()),
            Some(source.len() - 3..source.len())
        );

        let body = "x\n".repeat(4_096);
        let source = format!("```\n{body}```");
        let document = parse(&source, &mut cx).unwrap();
        assert!(first_code_block(&document.blocks[0]).is_some());
        assert_eq!(
            selected_code_range(&source, body.len() - 2..body.len() - 1),
            Some(body.len() + 2..body.len() + 3)
        );
    }

    #[test]
    fn source_segments_keep_non_linear_mappings_atomic() {
        let source = r"a\* &amp; b";
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        let paragraph = first_paragraph(&document.blocks[0]).unwrap();
        let segments = &paragraph.children[0].source_segments;

        assert!(
            segments.len() < paragraph.children[0].text.chars().count(),
            "ordinary characters should be compacted into runs"
        );
        assert!(segments.contains(&SourceSegment {
            rendered: 1..2,
            source: 1..3,
            linear: false,
        }));
        assert!(segments.contains(&SourceSegment {
            rendered: 3..4,
            source: 4..9,
            linear: false,
        }));

        assert_eq!(select_rendered_range(source, 1..2), 1..3);
        assert_eq!(select_rendered_range(source, 3..4), 4..9);
    }

    #[test]
    fn source_segments_keep_an_entity_as_long_as_its_characters_atomic() {
        // `&acE;` decodes to `∾̳`: five bytes each, but not one for one.
        let source = "a &acE; b";
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        let paragraph = first_paragraph(&document.blocks[0]).unwrap();
        assert_eq!(paragraph.children[0].text.as_ref(), "a ∾̳ b");
        assert_eq!(
            paragraph.children[0].source_segments,
            vec![
                SourceSegment {
                    rendered: 0..2,
                    source: 0..2,
                    linear: true,
                },
                SourceSegment {
                    rendered: 2..7,
                    source: 2..7,
                    linear: false,
                },
                SourceSegment {
                    rendered: 7..9,
                    source: 7..9,
                    linear: true,
                },
            ]
        );
        // Selecting the combining mark alone reports the whole entity.
        assert_eq!(select_rendered_range(source, 5..7), 2..7);
    }

    #[test]
    fn selected_source_range_crosses_style_boundaries() {
        let source = "left **bold** right";
        assert_eq!(select_rendered_range(source, 2..12), 2..16);
    }

    #[test]
    fn selected_source_range_maps_inline_code_in_merged_styled_node() {
        let source = "**left `code` right**";
        assert_eq!(selected_rendered_range(source, 5..9), Some(8..12));
    }

    #[test]
    fn selected_source_range_maps_inline_code_delimiters_and_boundaries() {
        let source = "`code` x";
        assert_eq!(selected_rendered_range(source, 0..4), Some(1..5));
        assert_eq!(selected_rendered_range(source, 5..6), Some(7..8));
        assert_eq!(selected_rendered_range(source, 3..6), Some(4..8));

        let padded = "`` code ` value ``";
        assert_eq!(selected_rendered_range(padded, 0..4), Some(3..7));
        assert_eq!(selected_rendered_range(padded, 5..6), Some(8..9));

        let literal_entity = "`&amp;`";
        assert_eq!(selected_rendered_range(literal_entity, 0..5), Some(1..6));
    }

    #[test]
    fn selected_source_range_keeps_inline_code_escapes_literal() {
        assert_eq!(select_rendered_range("`a\\\\b`", 1..2), 2..3);
        assert_eq!(select_rendered_range("`a\\\\b`", 2..3), 3..4);
        assert_eq!(select_rendered_range(r"`a\*b`", 1..2), 2..3);
        assert_eq!(select_rendered_range(r"`a\*b`", 2..3), 3..4);
        // The same punctuation is still an escape outside code.
        assert_eq!(select_rendered_range(r"a\*b", 1..2), 1..3);
    }

    #[test]
    fn selected_source_range_maps_footnote_reference_syntax() {
        // The reference renders as its number, which stands for all of `[^note]`.
        let source = "before[^note] after\n\n[^note]: body";
        assert_eq!(selected_rendered_range(source, 0..6), Some(0..6));
        assert_eq!(selected_rendered_range(source, 6..7), Some(6..13));
        assert_eq!(selected_rendered_range(source, 8..13), Some(14..19));
        assert_eq!(selected_rendered_range(source, 4..10), Some(4..16));
    }

    #[test]
    fn footnotes_number_by_first_reference_and_end_the_document() {
        let source = "one[^b] two[^a] again[^b]\n\n[^a]: note a\n\n[^b]: note b\n\n[^c]: unused";
        let document = parse(source, &mut NodeContext::default()).unwrap();
        let blocks = &document.blocks;
        // The paragraph, three definitions where they were written, then the notes.
        assert_eq!(blocks.len(), 7);
        assert_eq!(first_paragraph(&blocks[0]).unwrap().text(), "one1 two2 again1");
        let spans: Vec<_> = blocks[1..4].iter().map(|block| block.span().unwrap()).collect();
        assert_eq!(source[spans[0].start..spans[0].end].trim_end(), "[^a]: note a");
        assert_eq!(source[spans[1].start..spans[1].end].trim_end(), "[^b]: note b");
        let notes: Vec<_> = blocks[4..]
            .iter()
            .map(|block| match block {
                BlockNode::Footnote {
                    number,
                    ref_start,
                    children,
                    ..
                } => (
                    *number,
                    *ref_start,
                    first_paragraph(&children[0]).unwrap().text(),
                ),
                other => panic!("expected a note, got {other:?}"),
            })
            .collect();
        assert_eq!(
            notes,
            vec![
                (1, Some(3), "note b".to_string()),
                (2, Some(11), "note a".to_string()),
                (3, None, "unused".to_string()),
            ]
        );
    }

    #[test]
    fn a_marker_line_makes_a_callout_and_a_plain_quote_stays_plain() {
        let parse_quote = |source: &str| {
            let document = parse(source, &mut NodeContext::default()).unwrap();
            match &document.blocks[0] {
                BlockNode::Blockquote {
                    children, callout, ..
                } => (
                    callout.clone(),
                    children
                        .iter()
                        .filter_map(first_paragraph)
                        .map(Paragraph::text)
                        .collect::<Vec<_>>(),
                ),
                other => panic!("expected a quote, got {other:?}"),
            }
        };
        let (callout, body) = parse_quote("> [!note]\n> Mind the gap.");
        assert_eq!(callout.unwrap().kind, CalloutKind::Note);
        assert_eq!(body, vec!["Mind the gap."]);
        let (callout, body) = parse_quote("> [!WARNING] Hot surface\n> Do not touch.");
        let callout = callout.unwrap();
        assert_eq!(callout.kind, CalloutKind::Warning);
        assert_eq!(callout.title.as_deref(), Some("Hot surface"));
        assert_eq!(body, vec!["Do not touch."]);
        let (callout, body) = parse_quote("> [!HARSHA]");
        assert_eq!(callout.unwrap().kind, CalloutKind::Harsha);
        assert!(body.is_empty());
        let (callout, body) = parse_quote("> [!UNKNOWN]\n> Text.");
        assert!(callout.is_none());
        assert_eq!(body, vec!["[!UNKNOWN] Text."]);
        let (callout, _) = parse_quote("> just a quote");
        assert!(callout.is_none());
        let (callout, _) = parse_quote("> \\[!NOTE]\n> Text.");
        assert!(callout.is_none());
        let (callout, _) = parse_quote("> [!WARNING] Hot surface  \n> Do not touch.");
        assert_eq!(callout.unwrap().title.as_deref(), Some("Hot surface"));
    }

    #[test]
    fn selected_source_range_maps_mdx_text_expression_body() {
        let source = "before {value + 1} after";
        assert_eq!(selected_mdx_rendered_range(source, 7..16), Some(8..17));
        assert_eq!(selected_mdx_rendered_range(source, 4..19), Some(4..21));
    }

    #[test]
    fn selected_source_range_maps_mdx_flow_expression_body() {
        let source = "{\n  value + 1\n}";
        assert_eq!(selected_mdx_code(source, "value + 1"), Some(4..13));
        assert_eq!(selected_mdx_code(source, "lue +"), Some(6..11));
    }

    #[test]
    fn selected_source_range_maps_math_block_body() {
        let source = "$$\nx + y\n$$";
        assert_eq!(selected_code_range(source, 0..5), Some(3..8));
        assert_eq!(selected_code_range(source, 2..3), Some(5..6));
    }

    #[test]
    fn selected_source_range_rejects_mapped_block_plus_unmappable_entity() {
        let source = "mapped\n\nA &amp; B";
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        let [BlockNode::Paragraph(mapped), BlockNode::Paragraph(entity)] =
            document.blocks.as_slice()
        else {
            panic!("expected two paragraphs");
        };
        mapped.state.lock().unwrap().selection = Some((0..6).into());

        entity.state.lock().unwrap().selection = Some((2..2).into());
        assert_eq!(document.selected_source_range(), Some(0..6));

        entity.state.lock().unwrap().selection = Some((2..3).into());
        assert_eq!(document.selected_source_range(), Some(0..15));
    }

    #[test]
    fn selected_source_range_maps_fenced_code_body_after_matching_info_string() {
        let source = "```rust\nrust\n```";
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        let BlockNode::CodeBlock(code) = &document.blocks[0] else {
            panic!("expected code block");
        };
        code.set_selection(0..4);

        assert_eq!(document.selected_source_range(), Some(8..12));
    }

    #[test]
    fn selected_source_range_maps_each_literal_backslash_in_fenced_code() {
        let source = "```\na\\\\b\n```";
        assert_eq!(selected_code_range(source, 1..2), Some(5..6));
        assert_eq!(selected_code_range(source, 2..3), Some(6..7));
        assert_eq!(selected_code_range(source, 1..3), Some(5..7));
        let punctuation = "```\na\\*b\n```";
        assert_eq!(selected_code_range(punctuation, 1..2), Some(5..6));
        assert_eq!(selected_code_range(punctuation, 2..3), Some(6..7));
        assert_eq!(selected_code_range("```\na\\\\b\r\n```", 1..3), Some(5..7));
        let multiline = "```\na\\\\\nb\n```";
        assert_eq!(selected_code_range(multiline, 1..2), Some(5..6));
        assert_eq!(selected_code_range(multiline, 2..3), Some(6..7));
        assert_eq!(selected_code_range(multiline, 3..4), Some(7..8));
        assert_eq!(selected_code_range(multiline, 4..5), Some(8..9));
    }

    #[test]
    fn selected_source_range_excludes_closing_fence_candidate() {
        let source = "````text\n```\n````";
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        let BlockNode::CodeBlock(code) = &document.blocks[0] else {
            panic!("expected code block");
        };
        code.set_selection(0..3);

        assert_eq!(document.selected_source_range(), Some(9..12));
    }

    #[test]
    fn selected_source_range_maps_indented_code_content() {
        let source = "    rust";
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        let BlockNode::CodeBlock(code) = &document.blocks[0] else {
            panic!("expected code block");
        };
        code.set_selection(0..4);

        assert_eq!(document.selected_source_range(), Some(4..8));
    }

    #[test]
    fn selected_source_range_maps_multiline_indented_code() {
        let source = "    one\n    two\n    three";
        assert_eq!(selected_code_range(source, 0..3), Some(4..7));
        assert_eq!(selected_code_range(source, 4..7), Some(12..15));
        assert_eq!(selected_code_range(source, 0..13), Some(4..25));
    }

    #[test]
    fn selected_source_range_maps_literal_backslashes_in_indented_code() {
        let source = "    one\n    a\\\\b";
        assert_eq!(selected_code_range(source, 5..6), Some(13..14));
        assert_eq!(selected_code_range(source, 6..7), Some(14..15));
        assert_eq!(selected_code_range(source, 5..7), Some(13..15));
    }

    #[test]
    fn selected_source_range_maps_fenced_code_nested_in_a_list() {
        let source = "- ```rust\n  one\n  two\n  ```";
        assert_eq!(selected_code_range(source, 0..3), Some(12..15));
        assert_eq!(selected_code_range(source, 4..7), Some(18..21));
        assert_eq!(selected_code_range(source, 0..7), Some(12..21));
    }

    #[test]
    fn selected_source_range_maps_literal_backslashes_in_nested_fences() {
        for source in ["- ```\n  a\\\\b\n  ```", "> ```\n> a\\\\b\n> ```"] {
            assert_eq!(selected_code_range(source, 1..2), Some(9..10));
            assert_eq!(selected_code_range(source, 2..3), Some(10..11));
        }
    }

    #[test]
    fn selected_source_range_maps_fenced_code_nested_in_a_blockquote() {
        let source = "> ```\n> one\n> two\n> ```";
        assert_eq!(selected_code_range(source, 0..3), Some(8..11));
        assert_eq!(selected_code_range(source, 4..7), Some(14..17));
        assert_eq!(selected_code_range(source, 0..7), Some(8..17));
    }

    #[test]
    fn selected_source_range_maps_fenced_code_with_blank_lines_and_repeated_text() {
        let source = "```\nsame\n\nsame\n```";
        assert_eq!(selected_code_range(source, 0..4), Some(4..8));
        assert_eq!(selected_code_range(source, 6..10), Some(10..14));
        assert_eq!(selected_code_range(source, 0..10), Some(4..14));
    }

    #[test]
    fn selected_source_range_maps_the_whole_markdown_escape() {
        assert_eq!(selected_rendered_range(r"\*", 0..1), Some(0..2));
    }

    #[test]
    fn selected_source_range_maps_text_after_an_escape_character_for_character() {
        let source = r"\*abc";
        assert_eq!(select_rendered_range(source, 0..1), 0..2);
        assert_eq!(select_rendered_range(source, 2..3), 3..4);
        assert_eq!(select_rendered_range(source, 0..2), 0..3);
        // Only punctuation escapes, so a backslash before a letter is text.
        let source = r"\été";
        assert_eq!(select_rendered_range(source, 0..1), 0..1);
        assert_eq!(select_rendered_range(source, 1..3), 1..3);
    }

    #[test]
    fn selected_source_range_maps_after_an_escaped_backslash() {
        let source = r"a\\b";
        assert_eq!(select_rendered_range(source, 1..2), 1..3);
        assert_eq!(select_rendered_range(source, 2..3), 3..4);
        assert_eq!(select_rendered_range(source, 1..3), 1..4);

        let repeated = r"\\\\b";
        assert_eq!(select_rendered_range(repeated, 2..3), 4..5);
    }

    #[test]
    fn selected_source_range_does_not_borrow_an_escape_from_the_previous_node() {
        let source = r"a\\$x$";
        assert_eq!(select_rendered_range(source, 2..3), 3..4);
        assert_eq!(select_rendered_range(source, 1..3), 1..4);
    }

    #[test]
    fn selected_source_range_does_not_shift_a_hard_break_after_an_escape() {
        let source = "a\\\\  \nb";
        assert_eq!(select_rendered_range(source, 2..3), 3..6);
        assert_eq!(select_rendered_range(source, 1..3), 1..6);
    }

    #[test]
    fn selected_source_range_includes_a_trailing_inline_image() {
        let source = "before ![alt](image.png)";
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        let BlockNode::Paragraph(paragraph) = &document.blocks[0] else {
            panic!("expected paragraph");
        };
        let image = paragraph
            .children
            .iter()
            .find(|child| child.image.is_some())
            .expect("expected image");
        let mut state = image.state.lock().unwrap();
        state.set_text("before ".into());
        state.selection = Some((0..7).into());
        drop(state);

        assert_eq!(document.selected_source_range(), Some(0..source.len()));
    }

    #[test]
    fn selected_source_range_includes_a_leading_inline_image() {
        let source = "![alt](image.png) after";
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        let BlockNode::Paragraph(paragraph) = &document.blocks[0] else {
            panic!("expected paragraph");
        };
        let mut state = paragraph.state.lock().unwrap();
        state.set_text(" after".into());
        state.selection = Some((0..6).into());
        drop(state);

        assert_eq!(document.selected_source_range(), Some(0..source.len()));
    }

    #[test]
    fn selected_source_range_includes_an_enclosed_inline_image() {
        let source = "before ![alt](image.png) after";
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        let BlockNode::Paragraph(paragraph) = &document.blocks[0] else {
            panic!("expected paragraph");
        };
        let image = paragraph
            .children
            .iter()
            .find(|child| child.image.is_some())
            .expect("expected image");
        let mut before = image.state.lock().unwrap();
        before.set_text("before ".into());
        before.selection = Some((0..7).into());
        drop(before);
        let mut after = paragraph.state.lock().unwrap();
        after.set_text(" after".into());
        after.selection = Some((0..6).into());
        drop(after);

        assert_eq!(document.selected_source_range(), Some(0..source.len()));
    }

    #[test]
    fn selected_source_range_excludes_an_unreached_inline_image() {
        let source = "before ![alt](image.png) after";
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        let BlockNode::Paragraph(paragraph) = &document.blocks[0] else {
            panic!("expected paragraph");
        };
        let image = paragraph
            .children
            .iter()
            .find(|child| child.image.is_some())
            .expect("expected image");
        let mut before = image.state.lock().unwrap();
        before.set_text("before ".into());
        before.selection = Some((0..3).into());
        drop(before);

        assert_eq!(document.selected_source_range(), Some(0..3));

        image.state.lock().unwrap().selection = None;
        let after_start = source.find("after").unwrap();
        let mut after = paragraph.state.lock().unwrap();
        after.set_text(" after".into());
        after.selection = Some((2..6).into());
        drop(after);

        assert_eq!(
            document.selected_source_range(),
            Some(after_start + 1..source.len())
        );
    }

    #[test]
    fn selected_source_range_includes_consecutive_inline_images() {
        let source = "![first](one.png)![second](two.png) after";
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        let BlockNode::Paragraph(paragraph) = &document.blocks[0] else {
            panic!("expected paragraph");
        };
        let mut state = paragraph.state.lock().unwrap();
        state.set_text(" after".into());
        state.selection = Some((0..6).into());
        drop(state);

        assert_eq!(document.selected_source_range(), Some(0..source.len()));
    }

    #[test]
    fn selected_source_range_maps_decoded_entity_to_its_source_syntax() {
        let source = "A &amp; B";
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        let BlockNode::Paragraph(paragraph) = &document.blocks[0] else {
            panic!("expected paragraph");
        };
        assert_eq!(paragraph.text(), "A & B");

        assert_eq!(selected_rendered_range(source, 2..3), Some(2..7));
    }

    #[test]
    fn selected_source_range_maps_around_named_and_numeric_entities() {
        let source = "Copyright &copy; &#x1F600; &#169; 2024";
        assert_eq!(selected_rendered_range(source, 0..9), Some(0..9));
        assert_eq!(selected_rendered_range(source, 10..12), Some(10..16));
        assert_eq!(selected_rendered_range(source, 13..17), Some(17..26));
        assert_eq!(selected_rendered_range(source, 18..20), Some(27..33));
        assert_eq!(selected_rendered_range(source, 21..25), Some(34..38));
        assert_eq!(selected_rendered_range(source, 8..22), Some(8..35));
    }

    #[test]
    fn selected_source_range_maps_soft_breaks_with_source_prefixes() {
        assert_eq!(selected_rendered_range("a\n   b", 0..1), Some(0..1));
        assert_eq!(selected_rendered_range("a\n   b", 2..3), Some(5..6));
        assert_eq!(selected_rendered_range("a\n   b", 0..3), Some(0..6));

        assert_eq!(selected_rendered_range("> a\n> b", 0..1), Some(2..3));
        assert_eq!(selected_rendered_range("> a\n> b", 2..3), Some(6..7));
        assert_eq!(selected_rendered_range("> a\n> b", 0..3), Some(2..7));

        assert_eq!(selected_rendered_range("- a\n  b", 0..1), Some(2..3));
        assert_eq!(selected_rendered_range("- a\n  b", 2..3), Some(6..7));
        assert_eq!(selected_rendered_range("- a\n  b", 0..3), Some(2..7));
    }

    #[test]
    fn selected_source_range_maps_soft_breaks_with_trailing_spaces_and_crlf() {
        assert_eq!(selected_rendered_range("a \nb", 0..1), Some(0..1));
        assert_eq!(selected_rendered_range("a \nb", 1..2), Some(2..3));
        assert_eq!(selected_rendered_range("a \nb", 2..3), Some(3..4));
        assert_eq!(selected_rendered_range("a \nb", 0..3), Some(0..4));

        assert_eq!(selected_rendered_range("a \r\nb", 1..2), Some(2..4));
        assert_eq!(selected_rendered_range("a \r\nb", 2..3), Some(4..5));

        assert_eq!(selected_rendered_range("a\r\nb", 0..1), Some(0..1));
        assert_eq!(selected_rendered_range("a\r\nb", 2..3), Some(3..4));
        assert_eq!(selected_rendered_range("a\r\nb", 0..3), Some(0..4));
    }

    fn prose(source: &str) -> String {
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        document
            .blocks
            .iter()
            .find_map(first_paragraph)
            .expect("expected paragraph")
            .text()
            .replace(EXTERNAL_LINK_ARROW, "")
    }

    #[test]
    fn prose_gets_curly_quotes_dashes_and_ellipses() {
        assert_eq!(prose("a \"double\" and 'single'"), "a “double” and ‘single’");
        assert_eq!(prose("it's rock 'n' roll, the '90s, 1990s"), "it’s rock ’n’ roll, the ’90s, 1990s");
        assert_eq!(prose("pages 3--5, so -- and --- or ... done"), "pages 3–5, so – and — or … done");
        assert_eq!(prose("(\"in parens\") and \"end.\""), "(“in parens”) and “end.”");
    }

    #[test]
    fn quotes_close_after_emphasis_and_open_after_it() {
        assert_eq!(prose("*\"a\"* \"b\""), "“a” “b”");
        assert_eq!(prose("**bold**\"close\""), "bold”close”");
        assert_eq!(prose("[link](https://x.test)'s text"), "link’s text");
    }

    #[test]
    fn code_flags_and_autolinks_stay_as_written() {
        assert_eq!(prose("`it's \"x\" -- ...` and it's"), "it's \"x\" -- ... and it’s");
        assert_eq!(prose("use --verbose or -- here"), "use --verbose or – here");
        assert_eq!(prose("see https://x.test/a--b now"), "see https://x.test/a--b now");
        let mut cx = NodeContext::default();
        let document = parse("```\nit's \"q\" -- ...\n```", &mut cx).unwrap();
        let code = document.blocks.iter().find_map(first_code_block).unwrap();
        assert_eq!(code.code().as_ref(), "it's \"q\" -- ...");
    }

    #[test]
    fn selection_still_maps_to_source_through_smart_punctuation() {
        let source = "a \"b\" c";
        assert_eq!(prose(source), "a “b” c");
        assert_eq!(selected_rendered_range(source, 5..6), Some(3..4));
        assert_eq!(selected_rendered_range(source, 10..11), Some(6..7));
        assert_eq!(selected_rendered_range(source, 2..5), Some(2..3));

        let source = "x... y -- z";
        assert_eq!(prose(source), "x… y – z");
        assert_eq!(selected_rendered_range(source, 5..6), Some(5..6));
        assert_eq!(selected_rendered_range(source, 1..4), Some(1..4));
        assert_eq!(selected_rendered_range(source, 11..12), Some(10..11));
    }

    #[test]
    fn escaped_and_entity_quotes_stay_straight() {
        assert_eq!(prose(r#"a \"b\" c"#), "a \"b\" c");
        assert_eq!(prose("a &quot;b&quot; and \"c\""), "a \"b\" and “c”");
    }

    #[test]
    fn a_quote_before_a_space_or_a_twin_closes() {
        assert_eq!(prose("a \"\" b"), "a ”” b");
        assert_eq!(prose("a '' b"), "a ’’ b");
        assert_eq!(prose("a \" b"), "a ” b");
        assert_eq!(prose("\"*a*\""), "“a”");
    }

    #[test]
    fn dashes_next_to_code_do_not_eat_it() {
        assert_eq!(prose("*-`--`*"), "-`--`".replace('`', ""));
        let long = format!("{} \"x\" - {}", "=".repeat(20_000), "y".repeat(10));
        assert!(prose(&long).contains("“x”"));
    }

    #[test]
    fn test_nested_emphasis_merges_text_marks() {
        let mut cx = NodeContext::default();
        let document = parse("This has **_bold and italic_** text.", &mut cx).unwrap();

        let BlockNode::Paragraph(paragraph) = &document.blocks[0] else {
            panic!("expected paragraph");
        };

        let bold_italic = paragraph
            .children
            .iter()
            .find(|child| child.text.as_ref() == "bold and italic")
            .expect("expected emphasized text");

        assert!(
            bold_italic
                .marks
                .iter()
                .any(|(_, mark)| mark.bold && mark.italic),
            "nested emphasis should produce a bold and italic mark"
        );
    }

    #[test]
    fn test_inline_html_image_stays_in_markdown_paragraph() {
        let mut cx = NodeContext::default();
        let document = parse(
            r#"Before <img src="https://example.com/avatar.png" alt="Avatar" width="32" height="32" /> after."#,
            &mut cx,
        )
        .unwrap();

        let BlockNode::Paragraph(paragraph) = &document.blocks[0] else {
            panic!("expected paragraph");
        };

        assert_eq!(paragraph.children.len(), 3);
        assert_eq!(paragraph.children[0].text.as_ref(), "Before ");
        assert_eq!(paragraph.children[2].text.as_ref(), " after.");

        let image = paragraph.children[1]
            .image
            .as_ref()
            .expect("expected inline html image");
        assert_eq!(image.url.as_ref(), "https://example.com/avatar.png");
        assert_eq!(image.width, Some(gpui::px(32.).into()));
        assert_eq!(image.height, Some(gpui::px(32.).into()));
    }

    #[test]
    fn test_inline_html_image_without_size_stays_in_markdown_paragraph() {
        let mut cx = NodeContext::default();
        let document = parse(
            r#"Before <img src="https://avatars.githubusercontent.com/u/5518"> after."#,
            &mut cx,
        )
        .unwrap();

        let BlockNode::Paragraph(paragraph) = &document.blocks[0] else {
            panic!("expected paragraph");
        };

        assert_eq!(paragraph.children.len(), 3);
        assert_eq!(paragraph.children[0].text.as_ref(), "Before ");
        assert_eq!(paragraph.children[2].text.as_ref(), " after.");

        let image = paragraph.children[1]
            .image
            .as_ref()
            .expect("expected inline html image");
        assert_eq!(
            image.url.as_ref(),
            "https://avatars.githubusercontent.com/u/5518"
        );
        assert_eq!(image.width, None);
        assert_eq!(image.height, None);
    }

    /// A CommonMark soft break — a bare line ending inside a paragraph —
    /// reflows to a space, so prose hard-wrapped in the source fills the
    /// available width instead of keeping the source's line structure.
    ///
    /// Every line ending CommonMark recognises has to collapse, not just LF:
    /// mdast passes the source's bytes through, so a CRLF document would
    /// otherwise strand its carriage return in the middle of the line.
    #[test]
    fn test_soft_break_reflows_to_space() {
        for source in [
            "this sentence\ncontinues as a soft wrap",
            "this sentence\r\ncontinues as a soft wrap",
            "this sentence\rcontinues as a soft wrap",
        ] {
            let mut cx = NodeContext::default();
            let document = parse(source, &mut cx).unwrap();

            let BlockNode::Paragraph(paragraph) = &document.blocks[0] else {
                panic!("expected paragraph");
            };

            assert_eq!(paragraph.children.len(), 1, "source: {source:?}");
            assert_eq!(
                paragraph.children[0].text.as_ref(),
                "this sentence continues as a soft wrap",
                "source: {source:?}"
            );
        }
    }

    /// The two breaks stay distinguishable in one paragraph: the soft one
    /// reflows into the run, the hard one keeps its own newline node.
    ///
    /// This is the invariant the soft-break collapse rests on — a hard break
    /// arrives as `Node::Break` and never as a newline inside `Node::Text`.
    #[test]
    fn test_soft_break_reflows_while_hard_break_survives() {
        let mut cx = NodeContext::default();
        let document = parse("a\nb  \nc", &mut cx).unwrap();

        let BlockNode::Paragraph(paragraph) = &document.blocks[0] else {
            panic!("expected paragraph");
        };

        let texts: Vec<_> = paragraph
            .children
            .iter()
            .map(|child| child.text.as_ref())
            .collect();
        assert_eq!(texts, ["a b", "\n", "c"]);
    }

    /// A CommonMark hard break — two trailing spaces or a trailing backslash —
    /// renders as a newline, instead of joining the two lines.
    #[test]
    fn test_hard_break_renders_newline() {
        for source in [
            "Owner: Jane  \nPersona: assistant",
            "Owner: Jane\\\nPersona: assistant",
        ] {
            let mut cx = NodeContext::default();
            let document = parse(source, &mut cx).unwrap();

            let BlockNode::Paragraph(paragraph) = &document.blocks[0] else {
                panic!("expected paragraph");
            };

            assert_eq!(paragraph.children.len(), 3, "source: {source:?}");
            assert_eq!(paragraph.children[0].text.as_ref(), "Owner: Jane");
            assert_eq!(paragraph.children[1].text.as_ref(), "\n");
            assert_eq!(paragraph.children[2].text.as_ref(), "Persona: assistant");
        }
    }

    #[test]
    fn yaml_frontmatter_is_disabled_by_default() {
        let source = "---\nSome text\n---";
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();

        assert!(matches!(
            document.blocks[0],
            BlockNode::HorizontalRule { .. }
        ));
        let BlockNode::Heading {
            level, children, ..
        } = &document.blocks[1]
        else {
            panic!("expected setext heading");
        };
        assert_eq!(*level, 2);
        assert_eq!(children.text(), "Some text");
    }

    #[test]
    fn non_mapping_yaml_frontmatter_falls_back_to_code_block() {
        let extensions = MarkdownExtensions::default().frontmatter();
        let mut cx = NodeContext {
            markdown_extensions: extensions.into(),
            ..NodeContext::default()
        };
        let document = parse("---\n- name: example\n---", &mut cx).unwrap();

        let BlockNode::CodeBlock(code_block) = &document.blocks[0] else {
            panic!("expected YAML code block fallback");
        };
        assert_eq!(code_block.lang().as_deref(), Some("yml"));
        assert_eq!(code_block.code().as_ref(), "- name: example");
    }

    #[test]
    fn fenced_yaml_remains_a_code_block() {
        let mut cx = NodeContext::default();
        let document = parse("```yaml\nname: example\n```", &mut cx).unwrap();

        let BlockNode::CodeBlock(code_block) = &document.blocks[0] else {
            panic!("expected fenced YAML code block");
        };
        assert_eq!(code_block.lang().as_deref(), Some("yaml"));
        assert_eq!(code_block.code().as_ref(), "name: example");
    }

    #[derive(Debug, Clone, PartialEq)]
    struct Ticker {
        symbol: String,
    }

    fn parse_ticker_block(node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Paragraph(paragraph) = node else {
            return None;
        };
        let [Node::Text(text)] = paragraph.children.as_slice() else {
            return None;
        };
        let symbol = text.value.strip_prefix('$')?.to_string();
        let node_text = format!("${symbol}");

        Some(
            MarkdownNode::new("ticker", Ticker { symbol })
                .text(node_text)
                .markdown(cx.node_source(node).unwrap_or_default()),
        )
    }

    #[test]
    fn custom_block_parser_converts_ticker_syntax_to_custom_node() {
        let extensions = MarkdownExtensions::default().block_parser(parse_ticker_block);

        let mut cx = NodeContext {
            markdown_extensions: extensions.into(),
            ..NodeContext::default()
        };
        let document = parse("$TSLA.US", &mut cx).unwrap();

        let BlockNode::Custom { node, .. } = &document.blocks[0] else {
            panic!("expected custom markdown node");
        };
        assert_eq!(node.name(), "ticker");
        assert_eq!(node.as_text(), "$TSLA.US");
        assert_eq!(node.as_markdown(), "$TSLA.US");
        assert_eq!(
            node.data::<Ticker>(),
            Some(&Ticker {
                symbol: "TSLA.US".to_string()
            })
        );
        assert_eq!(document.text(), "$TSLA.US\n");
        assert_eq!(document.to_markdown(), "$TSLA.US");
    }

    struct TickerPlugin {
        name: &'static str,
    }

    #[test]
    fn inline_math_is_enabled_by_default_and_respects_code_and_escapes() {
        let extensions = MarkdownExtensions::default().plugin(
            crate::text::markdown_ext::TestInlinePlugin::new("formula").parse_with(|node, _| {
                let Node::InlineMath(math) = node else {
                    return None;
                };
                assert_eq!(math.value, "x");
                Some(MarkdownNode::new("formula", ()).text("formula"))
            }),
        );
        let mut cx = NodeContext {
            markdown_extensions: extensions.into(),
            ..Default::default()
        };
        let document = parse(r"$x$ `$code$` \$escaped\$", &mut cx).unwrap();
        assert_eq!(document.text(), "formula $code$ $escaped$\n");
        let BlockNode::Paragraph(paragraph) = &document.blocks[0] else {
            panic!()
        };
        assert_eq!(
            paragraph
                .children
                .iter()
                .filter(|node| node.custom.is_some())
                .count(),
            1
        );
    }

    #[test]
    fn inline_extensions_preserve_nodes_inside_marks_and_global_source_ranges() {
        let source = "中文 **before $x^2$ after** and `$ignored$`";
        let extensions = MarkdownExtensions::default().plugin(
            crate::text::markdown_ext::TestInlinePlugin::new("test").parse_with(|node, cx| {
                let Node::InlineMath(math) = node else {
                    return None;
                };
                assert_eq!(cx.node_source(node), Some("$x^2$"));
                Some(MarkdownNode::new("formula", math.value.clone()).text("x²"))
            }),
        );
        let mut cx = NodeContext {
            offset: 50,
            markdown_extensions: extensions.into(),
            ..Default::default()
        };
        let document = parse(source, &mut cx).unwrap();
        let BlockNode::Paragraph(paragraph) = &document.blocks[0] else {
            panic!()
        };
        let objects: Vec<_> = paragraph
            .children
            .iter()
            .filter_map(|node| node.custom.as_ref())
            .collect();
        assert_eq!(objects.len(), 1);
        let object = objects[0];
        let start = source.find("$x^2$").unwrap() + 50;
        assert_eq!(object.source_range(), Some(start..start + 5));
        assert_eq!(object.as_text(), "x²");
        assert_eq!(object.as_markdown(), "$x^2$");
        assert_eq!(object.accessibility_name(), "x²");
        assert_eq!(document.text(), "中文 before x² after and $ignored$\n");
        assert!(document.to_markdown().contains("**before $x^2$ after**"));
    }

    #[test]
    fn block_math_parses_as_a_block_and_falls_back_to_a_code_block() {
        // `$$` is a block fence. With only the inline construct enabled it was
        // swallowed as one long inline formula, so a block plugin matching
        // `Node::Math` never fired.
        let source = "$$\n\\sum_{i=1}^{n} i\n$$";
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        let BlockNode::CodeBlock(code) = &document.blocks[0] else {
            panic!("expected a code block fallback")
        };
        assert_eq!(code.code(), "\\sum_{i=1}^{n} i");
        assert!(document.to_markdown().contains("\\sum_{i=1}^{n} i"));
    }

    fn bold_runs(paragraph: &Paragraph) -> Vec<String> {
        paragraph
            .children
            .iter()
            .flat_map(|node| {
                node.marks
                    .iter()
                    .filter(|(_, mark)| mark.bold)
                    .map(|(range, _)| node.text[range.clone()].to_string())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[test]
    fn unclaimed_inline_math_parses_its_span_as_prose() {
        // Two dollar amounts pair up into an unclaimed math span. What lies
        // between them is ordinary prose: inline HTML there must still pair
        // with the tags outside the span, and emphasis inside it must render.
        let source =
            "EPS of <strong>$1.56</strong> beat the <strong>$1.50</strong> consensus, *up $2*";
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        assert_eq!(
            document.text(),
            "EPS of $1.56 beat the $1.50 consensus, up $2\n"
        );
        let BlockNode::Paragraph(paragraph) = &document.blocks[0] else {
            panic!()
        };
        assert_eq!(bold_runs(paragraph), ["$1.56", "$1.50"]);
        assert!(paragraph.children.iter().any(|node| {
            node.marks
                .iter()
                .any(|(range, mark)| mark.italic && node.text[range.clone()] == *"up $2")
        }));
        assert!(document.to_markdown().contains("$1.56"));
    }

    #[test]
    fn claimed_inline_math_survives_prose_flattening() {
        let extensions = MarkdownExtensions::default().plugin(
            crate::text::markdown_ext::TestInlinePlugin::new("formula").parse_with(|node, _| {
                let Node::InlineMath(math) = node else {
                    return None;
                };
                Some(MarkdownNode::new("formula", ()).text(format!("[{}]", math.value)))
            }),
        );
        let mut cx = NodeContext {
            markdown_extensions: extensions.into(),
            ..Default::default()
        };
        let document = parse("area $x^2$ costs $5 and $10", &mut cx).unwrap();
        assert_eq!(document.text(), "area [x^2] costs [5 and ]10\n");
    }

    #[test]
    fn inline_html_formatting_tags_pair_across_siblings() {
        let source = "a <strong>b *c* <em>d</em></strong> e <b>f</b> <i>g</i> <del>h</del> <br> <strong>unclosed";
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        assert_eq!(document.text(), "a b c d e f g h \n unclosed\n");
        let BlockNode::Paragraph(paragraph) = &document.blocks[0] else {
            panic!()
        };
        assert_eq!(bold_runs(paragraph), ["b c d", "f"]);
        let marked = |predicate: fn(&TextMark) -> bool| -> Vec<String> {
            paragraph
                .children
                .iter()
                .flat_map(|node| {
                    node.marks
                        .iter()
                        .filter(|(_, mark)| predicate(mark))
                        .map(|(range, _)| node.text[range.clone()].to_string())
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        assert_eq!(marked(|mark| mark.italic), ["c", "d", "g"]);
        assert_eq!(marked(|mark| mark.strikethrough), ["h"]);
    }

    fn highlight_runs(paragraph: &Paragraph) -> Vec<(String, gpui::Hsla)> {
        paragraph
            .children
            .iter()
            .flat_map(|node| {
                node.marks
                    .iter()
                    .filter_map(|(range, mark)| {
                        mark.highlight
                            .map(|color| (node.text[range.clone()].to_string(), color))
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Inline `<mark>` is paired like `<strong>` and keeps the same color
    /// rules as the block HTML path: default yellow, `color`, then
    /// `style="background-color"`.
    #[test]
    fn inline_html_mark_applies_highlight_colors() {
        let source = concat!(
            r#"Plain <mark>highlighted</mark> text and "#,
            r##"<mark data_id="1" color="#ff000059">red</mark> and "##,
            r#"<mark style="background-color: #336699">hex</mark>."#
        );
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        assert_eq!(document.text(), "Plain highlighted text and red and hex.\n");
        let BlockNode::Paragraph(paragraph) = &document.blocks[0] else {
            panic!()
        };
        assert_eq!(
            highlight_runs(paragraph),
            [
                ("highlighted".into(), gpui::rgb(0xfef08a).into()),
                (
                    "red".into(),
                    gpui::Rgba::try_from("#ff000059").unwrap().into()
                ),
                (
                    "hex".into(),
                    gpui::Rgba::try_from("#336699").unwrap().into()
                ),
            ]
        );
    }

    #[test]
    fn unclaimed_inline_math_keeps_its_literal_source() {
        // Math parsing is on by default, so prose that merely contains dollar
        // signs parses as a math node. With no plugin to render it, the text
        // must survive display and a Markdown round trip untouched.
        let source = "spent $5 and $10 today";
        let mut cx = NodeContext::default();
        let document = parse(source, &mut cx).unwrap();
        assert_eq!(document.text(), "spent $5 and $10 today\n");
        assert!(document.to_markdown().contains("spent $5 and $10 today"));
        let BlockNode::Paragraph(paragraph) = &document.blocks[0] else {
            panic!()
        };
        assert!(paragraph.children.iter().all(|node| node.custom.is_none()));
    }

    #[test]
    fn inline_object_markdown_roundtrip_preserves_nested_emphasis() {
        for source in [
            "*before $x$ after*",
            "*before **$x$** after*",
            "**before *$x$* after**",
        ] {
            let mut cx = NodeContext {
                markdown_extensions: MarkdownExtensions::default()
                    .plugin(
                        crate::text::markdown_ext::TestInlinePlugin::new("test").parse_with(
                            |node, _| match node {
                                Node::InlineMath(math) => {
                                    Some(MarkdownNode::new("math", ()).text(math.value.clone()))
                                }
                                _ => None,
                            },
                        ),
                    )
                    .into(),
                ..Default::default()
            };
            let original = parse(source, &mut cx).unwrap();
            let markdown = original.to_markdown();
            let reparsed = parse(&markdown, &mut cx).unwrap();
            let BlockNode::Paragraph(before) = &original.blocks[0] else {
                panic!()
            };
            let BlockNode::Paragraph(after) = &reparsed.blocks[0] else {
                panic!()
            };
            let before_object = before
                .children
                .iter()
                .find(|node| node.custom.is_some())
                .unwrap();
            let after_object = after
                .children
                .iter()
                .find(|node| node.custom.is_some())
                .unwrap();
            assert_eq!(before.text(), after.text(), "{source} -> {markdown}");
            assert_eq!(
                before_object.marks, after_object.marks,
                "{source} -> {markdown}"
            );
        }
    }

    #[test]
    fn inline_parser_without_metadata_falls_back_to_original_source() {
        let mut cx = NodeContext {
            markdown_extensions: MarkdownExtensions::default()
                .plugin(
                    crate::text::markdown_ext::TestInlinePlugin::new("test").parse_with(
                        |node, _| {
                            matches!(node, Node::InlineCode(_))
                                .then(|| MarkdownNode::new("opaque", ()))
                        },
                    ),
                )
                .into(),
            ..Default::default()
        };
        let document = parse("`原子`", &mut cx).unwrap();
        let BlockNode::Paragraph(paragraph) = &document.blocks[0] else {
            panic!()
        };
        let node = paragraph.children[0].custom.as_ref().unwrap();
        assert_eq!(node.as_text(), "`原子`");
        assert_eq!(node.as_markdown(), "`原子`");
        assert_eq!(node.accessibility_name(), "`原子`");
    }

    impl TickerPlugin {
        fn new(name: &'static str) -> Self {
            Self { name }
        }
    }

    impl MarkdownPlugin for TickerPlugin {
        fn is_block(&self) -> bool {
            true
        }

        fn name(&self) -> &str {
            self.name
        }

        fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
            parse_ticker_block(node, cx)
        }

        fn render(
            &self,
            node: &MarkdownNode,
            _window: &mut gpui::Window,
            _cx: &mut gpui::App,
        ) -> impl gpui::IntoElement {
            gpui::div().child(node.as_text().to_string())
        }
    }

    #[test]
    fn custom_block_plugin_registers_parser_and_renderer() {
        let extensions = MarkdownExtensions::default().plugin(TickerPlugin::new("ticker"));

        let mut cx = NodeContext {
            markdown_extensions: extensions.into(),
            ..NodeContext::default()
        };
        let document = parse("$TSLA.US", &mut cx).unwrap();

        let BlockNode::Custom { node, .. } = &document.blocks[0] else {
            panic!("expected custom markdown node");
        };
        assert_eq!(node.name(), "ticker");
        assert_eq!(
            node.data::<Ticker>(),
            Some(&Ticker {
                symbol: "TSLA.US".to_string()
            })
        );
    }

    const EDIT_DOC: &str = "# Title\n\n\
        First paragraph with *emphasis* and `code`.\n\n\
        Second paragraph, a [link](https://example.com) and ünïcode.\n\n\
        - one\n- two\n\n\
        > quote\n\n\
        ```rust\nfn main() {}\n```\n\n\
        | a | b |\n|---|---|\n| 1 | 2 |\n\n\
        Last paragraph.\n";

    fn parse_whole(source: &str) -> ParsedDocument {
        parse(source, &mut NodeContext::default()).unwrap()
    }

    /// `parse_edit` of `new` against the parse of `old`, which must equal the
    /// parse of `new` whenever it returns one.
    fn parse_edit_of(old: &str, new: &str) -> Option<ParsedDocument> {
        let edited = parse_edit(&parse_whole(old), new, &mut NodeContext::default());
        if let Some(edited) = &edited {
            assert_eq!(edited, &parse_whole(new), "edit of {old:?} to {new:?}");
        }
        edited
    }

    #[test]
    fn parse_edit_reparses_a_paragraph_edit_like_a_full_parse() {
        for (find, replace) in [
            ("First paragraph", "First, longer paragraph"),
            ("emphasis", "em"),
            ("ünïcode", "ünïcode ünï"),
            ("# Title", "# A different title"),
            ("Last paragraph.", "Last paragraph, edited."),
            ("- two", "- two and more"),
        ] {
            let new = EDIT_DOC.replacen(find, replace, 1);
            assert_ne!(new, EDIT_DOC);
            assert!(
                parse_edit_of(EDIT_DOC, &new).is_some(),
                "{find:?} to {replace:?} parsed in full"
            );
        }
    }

    #[test]
    fn parse_edit_keeps_source_positions_after_the_edit() {
        let new = EDIT_DOC.replacen("emphasis", "a good deal of emphasis", 1);
        let edited = parse_edit_of(EDIT_DOC, &new).expect("incremental");
        // Selecting the last paragraph's text still maps to the new source.
        let last = edited.blocks.last().and_then(BlockNode::span).unwrap();
        assert_eq!(&new[last.start..last.end], "Last paragraph.");
    }

    #[test]
    fn parse_edit_falls_back_when_block_boundaries_could_move() {
        for (find, replace) in [
            // Inside a code fence.
            ("fn main() {}", "fn main() { 1 }"),
            // Closing a fence.
            ("```rust", "rust"),
            // A new fence opening in the block.
            ("> quote", "> quote\n```"),
            // A pipe next to a code fence.
            ("> quote", "> quote|"),
            ("| 1 | 2 |", "| 10 | 20 |"),
            // Joining two paragraphs.
            ("`code`.\n\nSecond", "`code`.\nSecond"),
            // Splitting one.
            ("Second paragraph,", "Second\n\nparagraph,"),
            // Removing a block.
            ("> quote\n\n", ""),
            // A setext underline under a paragraph.
            ("Last paragraph.\n", "Last paragraph.\n---\n"),
            // A link reference definition.
            ("Last paragraph.\n", "Last paragraph.\n\n[a]: https://example.com\n"),
            // Inline HTML.
            ("Last paragraph.", "Last <b>paragraph</b>."),
        ] {
            let new = EDIT_DOC.replacen(find, replace, 1);
            assert_ne!(new, EDIT_DOC);
            assert!(
                parse_edit_of(EDIT_DOC, &new).is_none(),
                "{find:?} to {replace:?} was spliced"
            );
        }
    }

    /// Blocks that touch with no blank line between them, where an edit can
    /// change what the block next to it is.
    const TOUCHING_DOC: &str = "# Heading\nparagraph text\n- item one\n- item two\n\
        ## Sub\n1. a\n2. b\n> quoted\n---\nlast words\n\n    indented code\n\nTail.\n";

    /// Every one-character insert and delete in `doc`: each result of
    /// `parse_edit` equals a full parse. Returns how many were spliced.
    fn sweep_small_edits(doc: &str) -> usize {
        let mut spliced = 0;
        for ix in (0..=doc.len()).filter(|&ix| doc.is_char_boundary(ix)) {
            for insert in ["x", "\n", "\n\n", "# ", "*", "    ", "- ", "|", "> ", "\t"] {
                let mut new = doc.to_string();
                new.insert_str(ix, insert);
                spliced += parse_edit_of(doc, &new).is_some() as usize;
            }
            if let Some(next) = doc[ix..].chars().next() {
                let mut new = doc.to_string();
                new.replace_range(ix..ix + next.len_utf8(), "");
                spliced += parse_edit_of(doc, &new).is_some() as usize;
            }
        }
        spliced
    }

    #[test]
    fn parse_edit_matches_a_full_parse_for_every_small_edit() {
        let spliced = sweep_small_edits(EDIT_DOC);
        assert!(spliced > EDIT_DOC.len(), "only {spliced} edits spliced");
    }

    #[test]
    fn parse_edit_matches_a_full_parse_where_blocks_touch() {
        let spliced = sweep_small_edits(TOUCHING_DOC);
        assert!(spliced > 0, "no edit was spliced");
    }

    #[test]
    fn parse_edit_falls_back_when_the_next_block_changes_kind() {
        // The new line break makes `details` a lazy continuation of the list
        // item above `- step two`, which then joins that list.
        let old = "# Plan - step one\ndetails\n- step two\n\nEnd.\n";
        let new = "# Plan \n- step one\ndetails\n- step two\n\nEnd.\n";
        assert!(parse_edit_of(old, new).is_none());

        // The same with a blank line before `- step two`.
        let old = "# Plan - step one\ndetails\n\n- step two\n\nEnd.\n";
        let new = "# Plan \n- step one\ndetails\n\n- step two\n\nEnd.\n";
        assert!(parse_edit_of(old, new).is_none());

        // An indented code block that becomes a lazy continuation.
        let old = "# T - item\n\n    code\n\nEnd.\n";
        let new = "# T\n- item\n\n    code\n\nEnd.\n";
        parse_edit_of(old, new);
    }

    fn frontmatter_cx() -> NodeContext {
        NodeContext {
            markdown_extensions: MarkdownExtensions::default().frontmatter().into(),
            ..NodeContext::default()
        }
    }

    /// Like `sweep_small_edits` with a wider set of inserts, parsing with
    /// `cx`'s extensions. Returns how many edits were spliced.
    fn sweep_construct_edits(doc: &str, cx: fn() -> NodeContext) -> usize {
        let old = parse(doc, &mut cx()).unwrap();
        let mut spliced = 0;
        for ix in (0..=doc.len()).filter(|&ix| doc.is_char_boundary(ix)) {
            for insert in [
                "x", "\n", "\n\n", "`", "```", "~~~", "$$", "-", "---", "===", "<b>", "</b>", "<!--",
                "[a]", "[^1]", "[", "]", ":", "]: x", "[a]: x\n", "- ", "> ", "|",
            ] {
                let mut new = doc.to_string();
                new.insert_str(ix, insert);
                if let Some(edited) = parse_edit(&old, &new, &mut cx()) {
                    assert_eq!(
                        edited,
                        parse(&new, &mut cx()).unwrap(),
                        "insert {insert:?} at {ix} of {doc:?}"
                    );
                    spliced += 1;
                }
            }
            if let Some(next) = doc[ix..].chars().next() {
                let mut new = doc.to_string();
                new.replace_range(ix..ix + next.len_utf8(), "");
                if let Some(edited) = parse_edit(&old, &new, &mut cx()) {
                    assert_eq!(
                        edited,
                        parse(&new, &mut cx()).unwrap(),
                        "delete at {ix} of {doc:?}"
                    );
                    spliced += 1;
                }
            }
        }
        spliced
    }

    const DEFINITIONS_DOC: &str = "# Notes\n\n\
        Plain first paragraph.\n\n\
        Plain second paragraph.\n\n\
        Plain third paragraph.\n\n\
        See [the site][site] and [other] for more.\n\n\
        A footnote reference[^1] here.\n\n\
        Plain fourth paragraph.\n\n\
        Plain fifth paragraph.\n\n\
        - item\n- item two\n\n\
        Closing paragraph.\n\n\
        [site]: https://example.com \"Site\"\n\
        [other]: https://example.org\n";

    const CONSTRUCTS_DOC: &str = "Intro paragraph.\n\n\
        ```rust\nfn main() {}\n```\n\n\
        Between the fence and the rule.\n\n\
        ---\n\n\
        Title line\n===\n\n\
        $$\nx^2\n$$\n\n\
        Inline <b>tag</b> paragraph.\n\n\
        <div>\nblock html\n</div>\n\n\
        ~~~\ntilde\n~~~\n\n\
        Last paragraph.\n";

    const FRONTMATTER_DOC: &str = "---\ntitle: A note\ntags: [a, b]\n---\n\n\
        First paragraph.\n\n\
        ```\ncode\n```\n\n\
        Second paragraph.\n\n---\n\nThird paragraph.\n";

    #[test]
    fn parse_edit_matches_a_full_parse_with_link_definitions() {
        let spliced = sweep_construct_edits(DEFINITIONS_DOC, NodeContext::default);
        assert!(spliced > DEFINITIONS_DOC.len(), "only {spliced} edits spliced");
    }

    #[test]
    fn parse_edit_matches_a_full_parse_beside_fences_rules_and_tags() {
        let spliced = sweep_construct_edits(CONSTRUCTS_DOC, NodeContext::default);
        assert!(spliced > CONSTRUCTS_DOC.len(), "only {spliced} edits spliced");
    }

    const PIPE_DOC: &str = "> a |\n\nBetween.\n\n$$\nx\n$$\n\n---\n\n<!-- c -->\n\n\
        | a | b |\n|---|---|\n| 1 | 2 |\n\nEnd.\n";

    #[test]
    fn parse_edit_matches_a_full_parse_around_a_pipe_led_table() {
        sweep_construct_edits(PIPE_DOC, NodeContext::default);
        for (old, find, replace) in [
            ("> quote\n\n$$\nx\n$$\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nEnd.\n", "> quote", "> quote|"),
            ("> quote\n\n---\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nEnd.\n", "> quote", "> quote|"),
            ("> quote\n\n<!-- c -->\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nEnd.\n", "> quote", "> quote|"),
            ("> q|\n\n$$\nx\n$$\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nEnd.\n", "| 1 | 2 |", "| 1 | 3 |"),
        ] {
            parse_edit_of(old, &old.replacen(find, replace, 1));
        }
        for (old, find, replace) in [
            (
                "> q|\n\n$$\nx\n$$\n\nH\n\n$$\ny\n$$\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nEnd.\n",
                "H",
                "eH",
            ),
            (
                "> q|\n\n$$\nx\n$$\n\nH\n\n$$\ny\n$$\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nEnd.\n",
                "H",
                "# H",
            ),
            ("> quote\n\nevery word\n\n| a | b |\n|---|---|\n", "> quote", "> quote|"),
            ("> q|\n\n$$\nx\n$$\n\n- - -\n\n| a | b |\n|---|---|\n| 1 | 2 |\n", "- - -", "- - x"),
            ("> q|\n\n$$\nx\n$$\n\n    code\n\n| a | b |\n|---|---|\n| 1 | 2 |\n", "    code", "   code"),
        ] {
            parse_edit_of(old, &old.replacen(find, replace, 1));
        }
    }

    #[test]
    fn parse_edit_matches_a_full_parse_in_a_file_with_front_matter() {
        let spliced = sweep_construct_edits(FRONTMATTER_DOC, frontmatter_cx);
        assert!(spliced > FRONTMATTER_DOC.len(), "only {spliced} edits spliced");
    }

    #[test]
    fn parse_edit_splices_beside_constructs_it_cannot_change() {
        let defs = DEFINITIONS_DOC.replacen("Plain second", "Plain, longer second", 1);
        assert!(parse_edit_of(DEFINITIONS_DOC, &defs).is_some());
        let beside = CONSTRUCTS_DOC.replacen("Between the", "Typed between the", 1);
        assert!(parse_edit_of(CONSTRUCTS_DOC, &beside).is_some());
        // Next to a code fence.
        for (find, replace) in [("> quote", "> quoted")] {
            let new = EDIT_DOC.replacen(find, replace, 1);
            assert!(parse_edit_of(EDIT_DOC, &new).is_some(), "{find:?} parsed in full");
        }
        let old = parse(FRONTMATTER_DOC, &mut frontmatter_cx()).unwrap();
        let new = FRONTMATTER_DOC.replacen("First", "First typed", 1);
        let edited = parse_edit(&old, &new, &mut frontmatter_cx()).expect("spliced");
        assert_eq!(edited, parse(&new, &mut frontmatter_cx()).unwrap());
    }

    #[test]
    fn parse_edit_falls_back_on_what_definitions_decide() {
        for (find, replace) in [
            // A definition edited.
            ("[other]: https://example.org", "[other]: https://example.net"),
            // A definition added.
            ("Plain third paragraph.", "Plain third paragraph.\n\n[x]: https://x.org"),
            // A footnote reference added.
            ("Plain third paragraph.", "Plain third[^1] paragraph."),
        ] {
            let new = DEFINITIONS_DOC.replacen(find, replace, 1);
            assert_ne!(new, DEFINITIONS_DOC);
            assert!(
                parse_edit_of(DEFINITIONS_DOC, &new).is_none(),
                "{find:?} to {replace:?} was spliced"
            );
        }
    }

    #[test]
    fn parse_edit_resolves_reference_links_in_the_slice() {
        for (find, replace) in [
            // A new reference use, and one that is edited, in a document with definitions.
            ("Plain third paragraph.", "Plain [site] third paragraph."),
            ("Plain third paragraph.", "Plain [the site][site] third paragraph."),
            ("[other] for more", "[otherx] for more"),
            ("[other] for more", "[other][] for more"),
            // Typing beside a paragraph that holds reference links.
            ("Plain third paragraph.", "Plain third paragraphs."),
            ("Plain fourth paragraph.", "Plain fourth, longer paragraph."),
        ] {
            let new = DEFINITIONS_DOC.replacen(find, replace, 1);
            assert_ne!(new, DEFINITIONS_DOC);
            let edited = parse_edit_of(DEFINITIONS_DOC, &new);
            assert!(edited.is_some(), "{find:?} to {replace:?} parsed in full");
        }
        // Two definitions of one label: which comes first decides, so it parses in full.
        let twice = DEFINITIONS_DOC.replacen("[other]: https://example.org", "[site]: https://example.org", 1);
        let new = twice.replacen("Plain third paragraph.", "Plain third paragraphs.", 1);
        assert!(parse_edit_of(&twice, &new).is_none());
        // A definition inside a quote is not a top-level block: the slice cannot be given it.
        let nested = DEFINITIONS_DOC.replacen("Closing paragraph.", "> [q]: https://q.org\n\nClosing paragraph.", 1);
        let new = nested.replacen("Plain third paragraph.", "Plain [q] third.", 1);
        assert!(parse_edit_of(&nested, &new).is_none());
    }

    /// Notes, reference links, a table and a fence, with a reference and a note
    /// beside most of them.
    const FOOTNOTES_DOC: &str = "# Notes\n\n\
        Intro paragraph.\n\n\
        First claim[^a] here.\n\n\
        Plain between.\n\n\
        Second claim[^b] there.\n\n\
        | a | b |\n|---|---|\n| 1 | 2 |\n\n\
        ```rust\nfn x() {}\n```\n\n\
        > quote with a [site][s] link\n\n\
        - item [s]\n- item two\n\n\
        Another plain paragraph.\n\n\
        Last paragraph.\n\n\
        [s]: https://example.com\n\n\
        [^b]: Second note.\n\n\
        [^a]: First note\n    continued.\n\n\
        [^c]: Unused.\n";

    #[test]
    fn parse_edit_matches_a_full_parse_in_a_file_with_footnotes() {
        let spliced = sweep_construct_edits(FOOTNOTES_DOC, NodeContext::default);
        assert!(spliced > FOOTNOTES_DOC.len(), "only {spliced} edits spliced");
        let spliced = sweep_small_edits(FOOTNOTES_DOC);
        assert!(spliced > FOOTNOTES_DOC.len(), "only {spliced} edits spliced");
    }

    #[test]
    fn parse_edit_keeps_footnote_numbers_and_notes() {
        for (find, replace) in [
            ("Intro paragraph.", "Intro, a longer paragraph."),
            ("Plain between.", "Plain, typed between."),
            ("Another plain paragraph.", "Another plain paragraphs."),
            ("Last paragraph.", "Last paragraph, edited."),
            ("Last paragraph.\n\n", "Last paragraph.\n\nAdded paragraph.\n\n"),
            ("Plain between.\n\n", ""),
            // Beside the table, the fence and the reference links.
            ("| 1 | 2 |", "| 1 | 22 |"),
            ("fn x() {}", "fn x() { 1 }"),
            ("quote with a", "quote with some"),
            ("- item two", "- item two, longer"),
        ] {
            let new = FOOTNOTES_DOC.replacen(find, replace, 1);
            assert_ne!(new, FOOTNOTES_DOC);
            parse_edit_of(FOOTNOTES_DOC, &new);
        }
        // Plain prose far from the notes splices, and the notes follow the text.
        for (find, replace) in [
            ("Another plain paragraph.", "Another plain paragraphs."),
            ("Last paragraph.", "Last paragraph, edited."),
        ] {
            let new = FOOTNOTES_DOC.replacen(find, replace, 1);
            let edited = parse_edit_of(FOOTNOTES_DOC, &new).expect("spliced");
            let notes: Vec<_> = edited
                .blocks
                .iter()
                .filter_map(|block| match block {
                    BlockNode::Footnote { number, ref_start, .. } => Some((*number, *ref_start)),
                    _ => None,
                })
                .collect();
            assert_eq!(notes.len(), 3);
            for (number, ref_start) in notes.iter().take(2) {
                let at = ref_start.expect("referenced");
                let label = if *number == 1 { "[^a]" } else { "[^b]" };
                assert_eq!(&new[at..at + 4], label);
            }
        }
        // A footnote label touched, added or removed parses in full.
        for (find, replace) in [
            ("Plain between.", "Plain between[^b]."),
            ("First claim[^a] here.", "First claim here."),
            ("[^c]: Unused.", "[^c]: Unused, now noted."),
            ("[^a]: First note", "[^a]: First, longer note"),
        ] {
            let new = FOOTNOTES_DOC.replacen(find, replace, 1);
            assert_ne!(new, FOOTNOTES_DOC);
            assert!(parse_edit_of(FOOTNOTES_DOC, &new).is_none(), "{find:?} was spliced");
        }
    }

    #[test]
    fn parse_edit_matches_a_full_parse_when_a_footnote_note_is_the_neighbour() {
        // Typing in the last paragraph before the notes, and in the one before a note.
        for (find, replace) in [
            ("Last paragraph.", "Last paragraph, typed."),
            ("Last paragraph.\n\n[s]:", "Last paragraph.\n\nNew.\n\n[s]:"),
        ] {
            let new = FOOTNOTES_DOC.replacen(find, replace, 1);
            parse_edit_of(FOOTNOTES_DOC, &new);
        }
    }

    /// A fixture of about `size` bytes in sections, each with a "Typed target
    /// paragraph". `defs` puts reference links on both sides of the target
    /// and closes with their definitions; `notes` puts a footnote reference
    /// in each section and closes with the notes; `constructs` puts a fence,
    /// a rule and an inline tag next to the target; `front_matter` opens
    /// with a YAML block.
    fn big_fixture(size: usize, defs: bool, notes: bool, constructs: bool, front_matter: bool) -> String {
        let mut text = String::with_capacity(size + 1024);
        if front_matter {
            text.push_str("---\ntitle: Big\n---\n\n");
        }
        let mut n = 0;
        while text.len() < size {
            n += 1;
            text.push_str(&format!("## Section {n}\n\n"));
            if notes {
                text.push_str(&format!("A claim[^n{n}] with a note.\n\n"));
            }
            text.push_str(&format!(
                "A plain paragraph number {n} with *emphasis* and `code`.\n\n"
            ));
            if constructs {
                text.push_str(&format!(
                    "Text with <b>a tag</b> inside.\n\n```rust\nfn f{n}() {{}}\n```\n\n"
                ));
            }
            if defs {
                text.push_str(&format!("Text with a [link][ref{n}] inside.\n\n"));
            }
            text.push_str(&format!("Typed target paragraph {n}.\n\n"));
            if defs {
                text.push_str(&format!("See [again][ref{n}] and [ref{n}] after.\n\n"));
            }
            if constructs {
                text.push_str("---\n\n");
            }
            text.push_str("- one\n- two\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n");
        }
        if defs {
            for k in 1..=n {
                text.push_str(&format!("[ref{k}]: https://example.com/{k}\n"));
            }
            text.push('\n');
        }
        if notes {
            for k in 1..=n {
                text.push_str(&format!("[^n{k}]: Note {k}.\n\n"));
            }
        }
        text
    }

    /// Types 50 keys into the middle "Typed target paragraph" of 1 MB
    /// fixtures and counts spliced against full parses (`--nocapture`).
    #[test]
    fn parse_edit_counts_over_fifty_typed_keys() {
        let mut fell_back = Vec::new();
        for (name, defs, notes, constructs, front_matter) in [
            ("reference links beside the target", true, false, false, false),
            ("footnotes", false, true, false, false),
            ("fences, rules and tags", false, false, true, false),
            ("front matter, fences, rules and tags", false, false, true, true),
            ("everything", true, true, true, true),
        ] {
            let cx: fn() -> NodeContext = if front_matter { frontmatter_cx } else { NodeContext::default };
            let mut text = big_fixture(1 << 20, defs, notes, constructs, front_matter);
            let mut doc = parse(&text, &mut cx()).unwrap();
            let mut at = text.len() / 2;
            at += text[at..].find("Typed target paragraph").unwrap();
            at += text[at..].find('.').unwrap();
            let (mut spliced, mut full) = (0, 0);
            for key in 0..50 {
                text.insert(at + key, 'x');
                match parse_edit(&doc, &text, &mut cx()) {
                    Some(edited) => {
                        spliced += 1;
                        doc = edited;
                    }
                    None => {
                        full += 1;
                        doc = parse(&text, &mut cx()).unwrap();
                    }
                }
            }
            assert_eq!(doc, parse(&text, &mut cx()).unwrap());
            println!("COUNTS {name}, {} bytes: spliced {spliced}, full {full}", text.len());
            if full > 0 {
                fell_back.push(name);
            }
        }
        assert!(fell_back.is_empty(), "keys parsed in full for: {fell_back:?}");
    }
}
