use std::{ops::Range, rc::Rc, sync::Arc};

use gpui::{AnyElement, Context, HighlightStyle, Hsla, SharedString, Window};
use ropey::Rope;

use super::{EditorState, FoldRange, InputEdit};
use crate::SemanticThemeTokens;

/// Resolves semantic highlight names into renderable GPUI styles.
///
/// Base deliberately knows nothing about a concrete syntax theme. UI crates and
/// applications can provide any resolver, independently of their parser.
pub trait HighlightStyleResolver: Send + Sync {
    fn style(&self, name: &str) -> Option<HighlightStyle>;
}

#[derive(Default)]
struct NoHighlightStyles;

impl HighlightStyleResolver for NoHighlightStyles {
    fn style(&self, _: &str) -> Option<HighlightStyle> {
        None
    }
}

/// Parser-independent syntax highlighting seam consumed by the Base editor.
///
/// Implementations own parsing, incremental state, and language-specific
/// behavior. Base only asks for styled ranges and fold candidates.
pub trait InputHighlighter {
    fn language(&self) -> SharedString;

    fn update(
        &mut self,
        edit: Option<InputEdit>,
        text: &Rope,
        folding: bool,
        window: &mut Window,
        cx: &mut Context<EditorState>,
    );

    /// Apply several edits made as one change, such as typing with multiple
    /// cursors or Replace all. The edits are in the order they were applied,
    /// each in the coordinates of the text after the one before; `text` is
    /// the text after the last. No snapshot is kept per edit.
    ///
    /// The default has no incremental state to feed, so it hands the final
    /// text to [`Self::update`] once with no edit. Override it to reparse
    /// once from the edits.
    fn update_batch(
        &mut self,
        edits: &[InputEdit],
        text: &Rope,
        folding: bool,
        window: &mut Window,
        cx: &mut Context<EditorState>,
    ) {
        let _ = edits;
        self.update(None, text, folding, window, cx);
    }

    /// Return ordered, non-overlapping style runs that fully cover `range`.
    /// Use [`HighlightStyle::default`] for text without a semantic style.
    fn styles(
        &self,
        range: &Range<usize>,
        resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)>;

    fn fold_ranges(&self, text: &Rope) -> Vec<FoldRange>;

    fn fold_ranges_for_edit(&self, range: Range<usize>, text: &Rope) -> Vec<FoldRange> {
        let _ = range;
        self.fold_ranges(text)
    }

    /// Classify the one-byte characters (brackets) at each of `offsets` as
    /// code, string or comment from the parse the highlighter already keeps
    /// up to date, appending one entry per offset to `out`. Returns `false`,
    /// appending nothing, when there is no parse to read, and the caller
    /// treats every offset as code. Never parses.
    fn syntax_contexts(&self, offsets: &[usize], out: &mut Vec<SyntaxContext>) -> bool {
        let _ = (offsets, out);
        false
    }

    /// The context of an insertion at `offset` in `text`, read from the parse
    /// the highlighter already keeps, for indent and auto-close decisions.
    /// `None` when that parse is missing or may not describe `text` (a
    /// background parse is still pending), and the caller asks the syntax
    /// provider, which parses `text` itself. Never parses.
    fn syntax_context_at(&self, text: &Rope, offset: usize) -> Option<SyntaxContext> {
        let _ = (text, offset);
        None
    }

    /// `None` while there is no parse for [`Self::syntax_contexts`]; otherwise
    /// a number that must change whenever an input of [`Self::styles`] changes
    /// other than the text (a parse replacing the tree, injection layers
    /// applying), so a caller can tell its cached answers are out of date. The
    /// editor's highlight cache keys on it, and never caches on `None`.
    fn syntax_generation(&self) -> Option<u64> {
        None
    }

    /// The brackets `()[]{}` of code (never inside a string or comment) within
    /// `range` as `(byte offset, depth)` in source order, where depth counts the
    /// bracket pairs around the bracket and an opening and its closing bracket
    /// share one. Read from the parse the highlighter keeps; appends to `out`.
    /// Returns `false`, appending nothing, when there is no parse. Never parses.
    fn bracket_depths(&self, range: &Range<usize>, out: &mut Vec<(usize, u32)>) -> bool {
        let _ = (range, out);
        false
    }

    /// [`Self::styles`], and also the identifiers within `range` the highlighter
    /// classes as a variable or a parameter (not a property, function or type) as
    /// `(byte range, hash of the name)`, appended to `variables`, the hash stable
    /// across runs. One pass for both. The default has no variables to report.
    fn styles_with_variables(
        &self,
        range: &Range<usize>,
        resolver: &dyn HighlightStyleResolver,
        variables: &mut Vec<(Range<usize>, u64)>,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let _ = variables;
        self.styles(range, resolver)
    }

    /// The range of the first named syntax node strictly larger than `range`,
    /// for expanding a selection by syntax.
    ///
    /// `None` means there is no syntax tree (the editor then grows the
    /// selection by word, line and text). `Some(None)` means there is a tree
    /// but nothing larger than `range` to grow to.
    fn syntax_ancestor(&self, range: Range<usize>) -> Option<Option<Range<usize>>> {
        let _ = range;
        None
    }

    /// Whether `offset` is inside prose (such as a Markdown paragraph), where
    /// expanding the selection takes the word first, as in a string or comment.
    fn syntax_prose_at(&self, offset: usize) -> bool {
        let _ = offset;
        false
    }

    /// Whether `offset` is inside a fenced code block of a Markdown document, read from the
    /// parse the highlighter already keeps. `None` when that parse is missing or may not
    /// describe `text` (a background parse is still pending), and the caller scans the text
    /// itself. Never parses.
    fn in_fenced_code(&self, text: &Rope, offset: usize) -> Option<bool> {
        let _ = (text, offset);
        None
    }

    /// Hands `f` the parse tree the highlighter already holds, as `Any` (the app
    /// downcasts it to its parser's tree type), when that tree describes `text`
    /// exactly. Returns false, without calling `f`, when there is no tree or it is
    /// behind the text. Read-only; never parses.
    fn inspect_current_tree(&self, text: &Rope, f: &mut dyn FnMut(&dyn std::any::Any)) -> bool {
        let _ = (text, f);
        false
    }
}

pub type InputHighlighterFactory = Rc<dyn Fn(&str) -> Option<Box<dyn InputHighlighter>>>;
pub type SharedHighlightStyleResolver = Arc<dyn HighlightStyleResolver>;
pub type FoldIconRenderer = Rc<dyn Fn(usize, bool) -> AnyElement>;

/// Where in the syntax tree an offset sits, for editing decisions.
///
/// Parser-independent: `gpui-component` answers from tree-sitter, apps may
/// answer heuristically. `None` (no provider installed) means `Code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxContext {
    Code,
    String,
    Comment,
}

/// Answers syntax context for editing decisions (pairing, skip, indent).
///
/// Created per editor by the application's [`super::LanguageProvider`].
/// Base never imports a parser; implementations live in UI crates or apps.
pub trait SyntaxContextProvider {
    fn context_at(&self, text: &Rope, offset: usize) -> SyntaxContext;

    /// Drop what the provider keeps of the last text it parsed. Called while
    /// the highlighter's own parse answers instead.
    fn release_cache(&self) {}
}

#[derive(Clone, Copy, Default)]
pub struct DiagnosticColors {
    pub error: Hsla,
    pub warning: Hsla,
    pub info: Hsla,
    pub hint: Hsla,
}

/// Application-owned colors and highlight resolver consumed by editor painting.
#[derive(Clone)]
pub struct InputEditorStyle {
    pub foreground: Hsla,
    pub muted_foreground: Hsla,
    pub background: Hsla,
    pub border: Hsla,
    pub selection: Hsla,
    pub caret: Hsla,
    pub diagnostics: DiagnosticColors,
    pub highlight_styles: SharedHighlightStyleResolver,
    pub editor_invisible: Option<Hsla>,
    pub editor_active_line: Option<Hsla>,
    pub editor_gutter_background: Option<Hsla>,
    pub fold_icon_renderer: Option<FoldIconRenderer>,
}

impl InputEditorStyle {
    /// Fills in every colour that was left unset, from the active palette.
    ///
    /// `Hsla::default()` is fully transparent, and every colour on `Default` is
    /// that — so an input nothing projected onto painted its glyphs, its caret
    /// and its selection in nothing at all. Transparent is not a colour anyone
    /// means for ink, which is what makes it usable as "unset" here.
    ///
    /// This is resolution, not assignment: whatever a consumer did project is
    /// kept exactly. `crates/component` projects the whole style on every render and
    /// never reaches this; a consumer that projects once at construction gets
    /// the palette that is current now rather than the one that happened to be
    /// installed when the state was built.
    pub fn resolved(&self, tokens: &SemanticThemeTokens) -> Self {
        let colors = &tokens.colors;
        let unset = |value: Hsla| value.a == 0.;
        let or = |value: Hsla, fallback: Hsla| if unset(value) { fallback } else { value };

        let foreground = or(self.foreground, colors.foreground);
        let mut selection = self.selection;
        if unset(selection) {
            selection = colors.accent;
            // A selection must not hide the glyphs it selects.
            selection.a = 0.4;
        }

        Self {
            foreground,
            muted_foreground: or(self.muted_foreground, colors.muted_foreground),
            background: or(self.background, colors.surface),
            border: or(self.border, colors.border),
            selection,
            caret: or(self.caret, foreground),
            ..self.clone()
        }
    }
}

impl Default for InputEditorStyle {
    fn default() -> Self {
        Self {
            foreground: Hsla::default(),
            muted_foreground: Hsla::default(),
            background: Hsla::default(),
            border: Hsla::default(),
            selection: Hsla::default(),
            caret: Hsla::default(),
            diagnostics: DiagnosticColors::default(),
            highlight_styles: Arc::new(NoHighlightStyles),
            editor_invisible: None,
            editor_active_line: None,
            editor_gutter_background: None,
            fold_icon_renderer: None,
        }
    }
}

/// Palette-colored runs for rainbow brackets (`range`'s brackets from the
/// highlighter, `brackets[depth % len]`) and semantic variable colors (the
/// `variables` the highlighter reported with its styles, `semantic[hash % len]`),
/// ordered and non-overlapping, to apply over the highlighter's runs with
/// [`apply_overlay`]. Empty when both are off or empty, so runs stay as they were.
pub(crate) fn palette_overlay(
    highlighter: &dyn InputHighlighter,
    range: &Range<usize>,
    brackets: Option<&[Hsla]>,
    semantic: Option<&[Hsla]>,
    variables: &[(Range<usize>, u64)],
) -> Vec<(Range<usize>, Hsla)> {
    let mut overlay = Vec::new();
    if let Some(palette) = brackets.filter(|p| !p.is_empty()) {
        let mut depths = Vec::new();
        if highlighter.bracket_depths(range, &mut depths) {
            overlay.extend(depths.into_iter().map(|(offset, depth)| {
                (offset..offset + 1, palette[depth as usize % palette.len()])
            }));
        }
    }
    if let Some(palette) = semantic.filter(|p| !p.is_empty()) {
        overlay.extend(
            variables
                .iter()
                .map(|(r, hash)| (r.clone(), palette[(hash % palette.len() as u64) as usize])),
        );
    }
    overlay.sort_by_key(|(r, _)| r.start);
    // Runs must not overlap: keep the first of any pair that does.
    let mut end = 0;
    overlay.retain(|(r, _)| {
        let keep = r.start >= end && !r.is_empty();
        if keep {
            end = r.end;
        }
        keep
    });
    overlay
}

/// Sets the color of `styles` (ordered, non-overlapping) to the overlay's on the
/// bytes `overlay` (ordered, non-overlapping) covers, splitting a run at the
/// overlay's edges. The palette color always wins over the theme's.
pub(crate) fn apply_overlay(
    styles: Vec<(Range<usize>, HighlightStyle)>,
    overlay: &[(Range<usize>, Hsla)],
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut out = Vec::with_capacity(styles.len() + overlay.len() * 2);
    let mut j = 0;
    for (run, style) in styles {
        while j < overlay.len() && overlay[j].0.end <= run.start {
            j += 1;
        }
        let mut cur = run.start;
        while j < overlay.len() && overlay[j].0.start < run.end {
            let (o, color) = &overlay[j];
            let from = o.start.max(cur);
            let to = o.end.min(run.end);
            if cur < from {
                out.push((cur..from, style));
            }
            out.push((
                from..to,
                HighlightStyle {
                    color: Some(*color),
                    ..style
                },
            ));
            cur = to;
            if o.end <= run.end {
                j += 1;
            } else {
                break;
            }
        }
        if cur < run.end {
            out.push((cur..run.end, style));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use gpui::hsla;

    use super::InputEditorStyle;
    use crate::SemanticThemeTokens;

    fn dark() -> SemanticThemeTokens {
        let mut tokens = SemanticThemeTokens::default();
        tokens.colors.foreground = hsla(0., 0., 0.98, 1.0);
        tokens.colors.muted_foreground = hsla(0., 0., 0.64, 1.0);
        tokens.colors.surface = hsla(0., 0., 0.04, 1.0);
        tokens.colors.border = hsla(0., 0., 0.15, 1.0);
        tokens.colors.accent = hsla(0.6, 0.5, 0.5, 1.0);
        tokens
    }

    #[test]
    fn an_unprojected_style_takes_its_ink_from_the_palette() {
        let tokens = dark();
        let resolved = InputEditorStyle::default().resolved(&tokens);

        assert_eq!(resolved.foreground, tokens.colors.foreground);
        assert_eq!(resolved.caret, tokens.colors.foreground);
        assert_eq!(resolved.muted_foreground, tokens.colors.muted_foreground);
        assert_eq!(resolved.background, tokens.colors.surface);
        assert_eq!(resolved.border, tokens.colors.border);
        // The point of the change: every one of these was transparent, so an
        // input nothing projected onto painted its text in nothing at all.
        for colour in [
            resolved.foreground,
            resolved.caret,
            resolved.muted_foreground,
            resolved.selection,
        ] {
            assert!(colour.a > 0., "{colour:?} is still invisible");
        }
    }

    #[test]
    fn a_selection_stays_translucent_enough_to_read_through() {
        let resolved = InputEditorStyle::default().resolved(&dark());
        assert_eq!(resolved.selection.a, 0.4);
    }

    #[test]
    fn projected_colours_are_kept_verbatim() {
        let chosen = hsla(0.3, 0.4, 0.5, 1.0);
        let style = InputEditorStyle {
            foreground: chosen,
            caret: chosen,
            ..Default::default()
        };
        let resolved = style.resolved(&dark());

        assert_eq!(resolved.foreground, chosen);
        assert_eq!(resolved.caret, chosen);
        // And what was not projected still comes from the palette.
        assert_eq!(resolved.border, dark().colors.border);
    }

    #[test]
    fn resolution_never_consumes_its_own_output() {
        // The projected style is kept verbatim precisely so that this holds:
        // resolving against a second palette must follow it, not stay on the
        // first. Resolving in place would have frozen after one pass.
        let projected = InputEditorStyle::default();
        let first = projected.resolved(&dark());

        let mut light = SemanticThemeTokens::default();
        light.colors.foreground = hsla(0., 0., 0.04, 1.0);
        let second = projected.resolved(&light);

        assert_ne!(first.foreground, second.foreground);
        assert_eq!(second.foreground, light.colors.foreground);
    }

    struct Fake;

    impl super::InputHighlighter for Fake {
        fn language(&self) -> gpui::SharedString {
            "fake".into()
        }
        fn update(
            &mut self,
            _: Option<super::InputEdit>,
            _: &ropey::Rope,
            _: bool,
            _: &mut gpui::Window,
            _: &mut gpui::Context<super::EditorState>,
        ) {
        }
        fn styles(
            &self,
            range: &std::ops::Range<usize>,
            _: &dyn super::HighlightStyleResolver,
        ) -> Vec<(std::ops::Range<usize>, gpui::HighlightStyle)> {
            // A theme color on every byte of the first ten, as a bracket or
            // variable scope would carry.
            let theme = gpui::HighlightStyle {
                color: Some(hsla(0.9, 0.1, 0.9, 1.0)),
                ..Default::default()
            };
            vec![(range.start..10, theme), (10..range.end, Default::default())]
        }
        fn fold_ranges(&self, _: &ropey::Rope) -> Vec<super::FoldRange> {
            Vec::new()
        }
        fn bracket_depths(&self, _: &std::ops::Range<usize>, out: &mut Vec<(usize, u32)>) -> bool {
            out.extend([(0, 0), (1, 1), (2, 2), (3, 2), (4, 1), (5, 0)]);
            true
        }
    }

    fn variables() -> Vec<(std::ops::Range<usize>, u64)> {
        vec![(10..11, 7), (20..25, 7), (30..31, 8)]
    }

    fn palette(n: usize) -> Vec<gpui::Hsla> {
        (0..n).map(|i| hsla(i as f32 / n as f32, 0.8, 0.5, 1.0)).collect()
    }

    #[test]
    fn brackets_take_the_palette_by_depth_and_wrap() {
        let colors = palette(2);
        let overlay = super::palette_overlay(&Fake, &(0..40), Some(&colors), None, &[]);
        let got: Vec<_> = overlay.iter().map(|(r, c)| (r.start, *c)).collect();
        assert_eq!(
            got,
            vec![
                (0, colors[0]),
                (1, colors[1]),
                (2, colors[0]),
                (3, colors[0]),
                (4, colors[1]),
                (5, colors[0]),
            ]
        );
    }

    #[test]
    fn one_name_always_gets_one_color() {
        let colors = palette(5);
        let overlay = super::palette_overlay(&Fake, &(0..40), None, Some(&colors), &variables());
        assert_eq!(overlay.len(), 3);
        assert_eq!(overlay[0].1, overlay[1].1);
        assert_eq!(overlay[0].1, colors[7 % 5]);
        assert_eq!(overlay[2].1, colors[8 % 5]);
    }

    #[test]
    fn both_off_or_empty_adds_nothing() {
        assert!(super::palette_overlay(&Fake, &(0..40), None, None, &variables()).is_empty());
        assert!(super::palette_overlay(&Fake, &(0..40), Some(&[]), Some(&[]), &variables()).is_empty());
    }

    #[test]
    fn the_palette_beats_the_theme_color_and_runs_still_cover_the_range() {
        use super::InputHighlighter as _;
        let colors = palette(3);
        let theme = hsla(0.9, 0.1, 0.9, 1.0);
        let overlay = super::palette_overlay(&Fake, &(0..40), Some(&colors), Some(&colors), &variables());
        let styles = Fake.styles(&(0..40), &super::NoHighlightStyles);
        let out = super::apply_overlay(styles, &overlay);
        // Contiguous, covering 0..40, and every overlay byte carries its color.
        let mut at = 0;
        for (r, _) in &out {
            assert_eq!(r.start, at);
            at = r.end;
        }
        assert_eq!(at, 40);
        let color_at = |byte: usize| out.iter().find(|(r, _)| r.contains(&byte)).unwrap().1.color;
        for (r, c) in &overlay {
            for byte in r.clone() {
                assert_eq!(color_at(byte), Some(*c), "byte {byte}");
            }
        }
        // A themed byte nothing overlays keeps the theme color.
        assert_eq!(color_at(8), Some(theme));
        assert_eq!(color_at(12), None);
    }
}
