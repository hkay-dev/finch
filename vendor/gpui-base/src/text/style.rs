use std::sync::Arc;

use gpui::{AnyElement, HighlightStyle, Hsla, Pixels, Rems, StyleRefinement, rems};

use crate::ColorTokens;

/// TextViewStyle used to customize the style for [`super::TextView`].
///
/// The fields are private because this type crosses the `gpui-base` seam:
/// build one with the `with_*` methods and read it back through the accessors
/// of the same name, so a later field is an additive change rather than a
/// breaking one.
#[derive(Clone)]
pub struct TextViewStyle {
    foreground: Hsla,
    muted_foreground: Hsla,
    link: Hsla,
    /// The ticked task box fill, or the link color when `None`.
    task_fill: Option<Hsla>,
    /// Callout colors in the order Note, Tip, Important, Warning, Caution, Harsha, or the built-in ones when `None`.
    callout_colors: Option<[Hsla; 6]>,
    selection: Hsla,
    code_background: Hsla,
    border: Hsla,
    paragraph_gap: Rems,
    heading: Arc<dyn Fn(u8) -> StyleRefinement + Send + Sync + 'static>,
    code_block: StyleRefinement,
    table: StyleRefinement,
    table_head: StyleRefinement,
    table_cell: StyleRefinement,
    inline_code: HighlightStyle,
    /// Explicit inline code refinements, applied after theme and surface colors.
    inline_code_style: StyleRefinement,
    /// Inline code's size as a share of its run, or 0.875 when `None`.
    inline_code_scale: Option<f32>,
    /// Explicit list marker refinements, applied after theme and surface colors.
    list_style: StyleRefinement,
    /// Explicit blockquote refinements, applied after theme and surface colors.
    blockquote_style: StyleRefinement,
    /// The table body background, or the theme surface when `None`. Only
    /// [`Self::on_text_color`] sets it, to let a table on an inverted surface
    /// show that surface.
    table_background: Option<Hsla>,
    /// Blocks are held to this width and centered, when set.
    measure: Option<Pixels>,
    /// Space held on each side of every block, as `(share of the view's width, left least, right least, most)`, inside the scrolled gutter: a block never gets closer than `least` or farther than `most`. Wheel and drag over it still reach the text view.
    gutter: Option<(f32, Pixels, Pixels, Pixels)>,
    /// Width of a table's lines, a code block's edge and a rule: one device pixel in the host app (`1 / scale_factor`).
    hairline: Pixels,
    /// Ink of a code block's language name, or the muted foreground when `None`.
    code_label: Option<Hsla>,
    /// Corner radius of the plate behind a code block's language name and actions.
    code_plate_radius: Pixels,
    /// Fill of that plate: opaque, so it hides what is under it; the code background when `None`.
    code_plate: Option<Hsla>,
    /// Fill behind a `<mark>` that names no color, or pale yellow when `None`.
    mark_fill: Option<Hsla>,
    /// A callout's fill alpha on `(dark, light)` themes.
    callout_fill: (f32, f32),
    /// A callout's left bar: its width and the kind colour's alpha.
    callout_bar: (Pixels, f32),
    /// A callout's corner radius.
    callout_radius: Pixels,
    /// A link's underline: its thickness and the link colour's alpha.
    link_underline: (Pixels, f32),
    /// Draws the SF Symbol named, at a size in points: the host app's own drawing,
    /// which this crate cannot do. Callout and footnote icons come from it; without
    /// one they show no icon.
    symbol: Option<fn(&'static str, f32) -> AnyElement>,
    is_dark: bool,
}

impl PartialEq for TextViewStyle {
    fn eq(&self, other: &Self) -> bool {
        self.paragraph_gap == other.paragraph_gap
            && self.foreground == other.foreground
            && self.muted_foreground == other.muted_foreground
            && self.link == other.link
            && self.task_fill == other.task_fill
            && self.callout_colors == other.callout_colors
            && self.selection == other.selection
            && self.code_background == other.code_background
            && self.border == other.border
            && (Arc::ptr_eq(&self.heading, &other.heading)
                || (1..=6).all(|level| (self.heading)(level) == (other.heading)(level)))
            && self.code_block == other.code_block
            && self.table == other.table
            && self.table_head == other.table_head
            && self.table_cell == other.table_cell
            && self.inline_code == other.inline_code
            && self.inline_code_style == other.inline_code_style
            && self.inline_code_scale == other.inline_code_scale
            && self.list_style == other.list_style
            && self.blockquote_style == other.blockquote_style
            && self.table_background == other.table_background
            && self.measure == other.measure
            && self.gutter == other.gutter
            && self.hairline == other.hairline
            && self.code_label == other.code_label
            && self.code_plate_radius == other.code_plate_radius
            && self.code_plate == other.code_plate
            && self.mark_fill == other.mark_fill
            && self.callout_fill == other.callout_fill
            && self.callout_bar == other.callout_bar
            && self.callout_radius == other.callout_radius
            && self.link_underline == other.link_underline
            && self.symbol.map(|draw| draw as usize) == other.symbol.map(|draw| draw as usize)
            && self.is_dark == other.is_dark
    }
}

impl Default for TextViewStyle {
    fn default() -> Self {
        Self::from_colors(&ColorTokens::light(), false)
    }
}

impl TextViewStyle {
    /// Derives rich-text colors from Base semantic theme tokens.
    pub fn from_theme(theme: &crate::Theme) -> Self {
        Self::from_colors(
            &theme.tokens.colors,
            theme.appearance == crate::ThemeAppearance::Dark,
        )
    }

    /// Derives rich-text colors from one palette.
    ///
    /// Rich text needs a handful of roles the palette does not name directly —
    /// a code background, a link color — so they are mapped here once instead
    /// of at every call site.
    fn from_colors(colors: &ColorTokens, is_dark: bool) -> Self {
        Self {
            foreground: colors.foreground,
            muted_foreground: colors.muted_foreground,
            link: colors.primary,
            task_fill: None,
            callout_colors: None,
            selection: colors.selection,
            code_background: colors.accent,
            border: colors.border,
            paragraph_gap: rems(0.75),
            heading: Arc::new(|_| StyleRefinement::default()),
            code_block: StyleRefinement::default(),
            table: StyleRefinement::default(),
            table_head: StyleRefinement::default(),
            table_cell: StyleRefinement::default(),
            inline_code: HighlightStyle {
                background_color: Some(colors.accent),
                ..Default::default()
            },
            inline_code_style: StyleRefinement::default(),
            inline_code_scale: None,
            list_style: StyleRefinement::default(),
            blockquote_style: StyleRefinement::default(),
            table_background: None,
            measure: None,
            gutter: None,
            hairline: gpui::px(1.),
            code_label: None,
            code_plate_radius: gpui::px(4.),
            code_plate: None,
            mark_fill: None,
            callout_fill: (0.09, 0.13),
            callout_bar: (gpui::px(2.), 0.85),
            callout_radius: gpui::px(6.),
            link_underline: (gpui::px(1.), 0.4),
            symbol: None,
            is_dark,
        }
    }

    /// Sets the default body-text color.
    pub fn with_foreground(mut self, color: Hsla) -> Self {
        self.foreground = color;
        self
    }

    /// Sets the secondary text color.
    pub fn with_muted_foreground(mut self, color: Hsla) -> Self {
        self.muted_foreground = color;
        self
    }

    /// Sets the link text color.
    pub fn with_link(mut self, color: Hsla) -> Self {
        self.link = color;
        self
    }

    /// Sets the fill of a ticked task-list box (the link color when unset).
    pub fn with_task_fill(mut self, color: Hsla) -> Self {
        self.task_fill = Some(color);
        self
    }

    /// Sets the callout colors (fill, bar and title), in the order Note, Tip,
    /// Important, Warning, Caution, Harsha. Without them each kind uses a built-in
    /// color for the light or dark surface.
    pub fn with_callout_colors(mut self, colors: [Hsla; 6]) -> Self {
        self.callout_colors = Some(colors);
        self
    }

    /// The color of a callout kind: the host's, or the built-in one.
    pub(crate) fn callout_color(&self, kind: super::node::CalloutKind) -> Hsla {
        match self.callout_colors {
            Some(colors) => colors[kind.index()],
            None => kind.color(self.is_dark),
        }
    }

    /// The fill of a ticked task-list box.
    pub fn task_fill(&self) -> Hsla {
        self.task_fill.unwrap_or(self.link)
    }

    /// Sets the background painted behind selected text.
    ///
    /// Selection quads are painted under the glyphs, so this is normally a
    /// translucent wash rather than a solid fill.
    pub fn with_selection(mut self, color: Hsla) -> Self {
        self.selection = color;
        self
    }

    /// Sets the background of fenced code blocks and table header rows.
    pub fn with_code_background(mut self, color: Hsla) -> Self {
        self.code_background = color;
        self
    }

    /// Sets the color of borders and horizontal rules.
    pub fn with_border(mut self, color: Hsla) -> Self {
        self.border = color;
        self
    }

    /// Sets the gap between paragraphs. Defaults to 0.75 rem.
    pub fn with_paragraph_gap(mut self, gap: Rems) -> Self {
        self.paragraph_gap = gap;
        self
    }

    /// Holds every block to at most `measure` wide, centered in the view when
    /// the view is wider. The scroll container and its scrollbar keep the
    /// view's full width, so the side gutters scroll and select like the text.
    pub fn with_measure(mut self, measure: Pixels) -> Self {
        self.measure = Some(measure);
        self
    }

    /// The width every block is held to, when set.
    pub fn measure(&self) -> Option<Pixels> {
        self.measure
    }

    /// Holds every block `share` of the view's width from each side, between `left_least` (right: `right_least`, for a scrollbar track) and `most`.
    /// The space is part of the scrolled content, so the wheel and a selection drag work over it,
    /// and it follows the width the view is laid out at.
    pub fn with_gutter(mut self, share: f32, left_least: Pixels, right_least: Pixels, most: Pixels) -> Self {
        self.gutter = Some((share, left_least, right_least, most));
        self
    }

    /// The side space of every block: `(share, left least, right least, most)`, when set.
    pub fn gutter(&self) -> Option<(f32, Pixels, Pixels, Pixels)> {
        self.gutter
    }

    /// Sets the width of table lines, code block edges and rules (default 1 pt).
    pub fn with_hairline(mut self, width: Pixels) -> Self {
        self.hairline = width;
        self
    }

    /// The width of table lines, code block edges and rules.
    pub fn hairline(&self) -> Pixels {
        self.hairline
    }

    /// Sets the ink of a code block's language name (the muted foreground when unset).
    pub fn with_code_label(mut self, color: Hsla) -> Self {
        self.code_label = Some(color);
        self
    }

    /// The ink of a code block's language name.
    pub fn code_label(&self) -> Hsla {
        self.code_label.unwrap_or(self.muted_foreground)
    }

    /// Sets the corner radius of the plate behind a code block's language name and actions.
    pub fn with_code_plate_radius(mut self, radius: Pixels) -> Self {
        self.code_plate_radius = radius;
        self
    }

    /// Sets the opaque fill of that plate (the code background when unset); a translucent code background would stack on itself.
    pub fn with_code_plate(mut self, color: Hsla) -> Self {
        self.code_plate = Some(color);
        self
    }

    /// The fill of the plate behind a code block's language name and actions.
    pub fn code_plate(&self) -> Hsla {
        self.code_plate.unwrap_or(self.code_background)
    }

    /// Sets the fill behind `<mark>` text that names no color of its own (pale yellow when unset).
    pub fn with_mark_fill(mut self, color: Hsla) -> Self {
        self.mark_fill = Some(color);
        self
    }

    /// The fill behind `<mark>` text that names no color of its own.
    pub fn mark_fill(&self) -> Hsla {
        self.mark_fill.unwrap_or_else(super::format::html::default_mark_color)
    }

    /// Sets a callout's fill: the kind's colour at `dark` on a dark theme and at `light` on a light one (0.09 and 0.13 when unset).
    pub fn with_callout_fill(mut self, dark: f32, light: f32) -> Self {
        self.callout_fill = (dark, light);
        self
    }

    /// A callout's fill alpha for the style's theme.
    pub fn callout_fill(&self) -> f32 {
        if self.is_dark { self.callout_fill.0 } else { self.callout_fill.1 }
    }

    /// Sets a callout's left bar: its width and the kind's colour at `alpha` (2 pt at 0.85 when unset).
    pub fn with_callout_bar(mut self, width: Pixels, alpha: f32) -> Self {
        self.callout_bar = (width, alpha);
        self
    }

    /// A callout's bar width.
    pub fn callout_bar_width(&self) -> Pixels {
        self.callout_bar.0
    }

    /// The kind's colour's alpha on a callout's bar.
    pub fn callout_bar_alpha(&self) -> f32 {
        self.callout_bar.1
    }

    /// Sets a callout's corner radius (6 pt when unset).
    pub fn with_callout_radius(mut self, radius: Pixels) -> Self {
        self.callout_radius = radius;
        self
    }

    /// A callout's corner radius.
    pub fn callout_radius(&self) -> Pixels {
        self.callout_radius
    }

    /// Sets a link's underline: its thickness and the link colour's alpha (1 pt at 0.4 when unset).
    pub fn with_link_underline(mut self, thickness: Pixels, alpha: f32) -> Self {
        self.link_underline = (thickness, alpha);
        self
    }

    /// A link's underline thickness.
    pub fn link_underline_thickness(&self) -> Pixels {
        self.link_underline.0
    }

    /// The link colour's alpha on its underline.
    pub fn link_underline_alpha(&self) -> f32 {
        self.link_underline.1
    }

    /// The corner radius of the plate behind a code block's language name and actions.
    pub fn code_plate_radius(&self) -> Pixels {
        self.code_plate_radius
    }

    /// Sets the style refinement for headings, selected by heading level (1-6).
    pub fn with_heading<F>(mut self, heading: F) -> Self
    where
        F: Fn(u8) -> StyleRefinement + Send + Sync + 'static,
    {
        self.heading = Arc::new(heading);
        self
    }

    /// Sets the style refinement for code blocks.
    ///
    /// Set `overflow.y` to `Overflow::Scroll` together with a max height to
    /// scroll long code inside the block: it gets its own scrollbar, and wheel
    /// input over it no longer scrolls an ancestor list until the code reaches
    /// its edge.
    pub fn with_code_block(mut self, style: StyleRefinement) -> Self {
        self.code_block = style;
        self
    }

    /// Sets the highlight style for inline code spans.
    ///
    /// When `background_color` is `None`, the neutral code background is used,
    /// which keeps [`TextViewStyle::default`] usable without a theme.
    pub fn with_inline_code(mut self, style: HighlightStyle) -> Self {
        self.inline_code = style;
        self
    }

    /// Overrides inline code typography and its box presentation.
    ///
    /// Supports font family, size, features, fallbacks, weight, style, color, underline and
    /// strikethrough, background, border widths/color/style, corner radii, and
    /// padding. Sizes and padding resolve pixels/rem against the window.
    /// Unspecified properties keep the existing defaults. Box layout, margin,
    /// positioning, overflow, and interaction properties are not used.
    /// Line height remains owned by the paragraph. Non-finite or non-positive
    /// font sizes are ignored. Explicit refinements also take precedence on
    /// inverted surfaces.
    pub fn with_inline_code_style(mut self, style: StyleRefinement) -> Self {
        self.inline_code_style = normalize_inline_code_style(style);
        self
    }

    /// Sets the size of inline code as a share of the run it sits in (0.875 when unset), so a
    /// span in a heading follows the heading. A font size in [`Self::with_inline_code_style`]
    /// pins the span instead.
    pub fn with_inline_code_scale(mut self, scale: f32) -> Self {
        self.inline_code_scale = Some(scale).filter(|scale| scale.is_finite() && *scale > 0.);
        self
    }

    /// The size of inline code as a share of its run.
    pub fn inline_code_scale(&self) -> f32 {
        self.inline_code_scale.unwrap_or(0.875)
    }

    /// Refines the marker of a bullet or numbered list item.
    ///
    /// Applied to the marker column after its defaults: the muted text color,
    /// a minimum width of the list indent, right-aligned text, and a right
    /// padding of 0.3 times that indent. Text properties such as color, font
    /// family, size, and weight style the bullet or number. The text color
    /// also outlines an unchecked task-list checkbox; a checked one keeps the
    /// link color. Unspecified properties keep the existing defaults, and
    /// explicit refinements also take precedence on inverted surfaces.
    pub fn with_list_style(mut self, style: StyleRefinement) -> Self {
        self.list_style = style;
        self
    }

    /// Refines the blockquote container.
    ///
    /// Applied after its defaults: the muted text color, a 2px left border in
    /// the border color, and a left padding of 0.9 times the body font size.
    /// Border widths and color style the quote bar; text properties,
    /// background, padding, and corner radii style the quote and its
    /// contents. Child blocks keep properties they set explicitly, including
    /// heading and table font sizes. Unspecified properties keep the existing
    /// defaults, and explicit refinements also take precedence on inverted surfaces.
    pub fn with_blockquote_style(mut self, style: StyleRefinement) -> Self {
        self.blockquote_style = style;
        self
    }

    /// Sets the style refinement for the table container (the bordered wrapper
    /// in wrap mode, the scroll viewport in horizontal-scroll mode).
    ///
    /// Set `overflow_x: scroll` on the refinement for adaptive table layout:
    /// columns fit their content when space allows, shrink (wrapping cell
    /// text) down to a per-column floor when the frame is narrower, and below
    /// that the table scrolls horizontally instead of squeezing further.
    pub fn with_table(mut self, style: StyleRefinement) -> Self {
        self.table = style;
        self
    }

    /// Sets the style refinement for the header row (the first row) of a
    /// table, applied on top of the header background and foreground.
    pub fn with_table_head(mut self, style: StyleRefinement) -> Self {
        self.table_head = style;
        self
    }

    /// Sets the style refinement for each table cell.
    ///
    /// With the scroll table layout, `white_space: nowrap` here keeps cells on
    /// a single line — columns then never shrink and the table scrolls as soon
    /// as the content is wider than the frame.
    pub fn with_table_cell(mut self, style: StyleRefinement) -> Self {
        self.table_cell = style;
        self
    }

    /// Sets whether content-specific assets should use their dark variant.
    pub fn with_dark(mut self, is_dark: bool) -> Self {
        self.is_dark = is_dark;
        self
    }

    /// Sets how SF Symbols are drawn (see the `symbol` field).
    pub fn with_symbol(mut self, draw: fn(&'static str, f32) -> AnyElement) -> Self {
        self.symbol = Some(draw);
        self
    }

    /// The symbol `name` at `size` points, when the host app draws symbols.
    pub fn symbol(&self, name: &'static str, size: f32) -> Option<AnyElement> {
        self.symbol.map(|draw| draw(name, size))
    }

    /// The default body-text color.
    pub fn foreground(&self) -> Hsla {
        self.foreground
    }

    /// The secondary text color.
    pub fn muted_foreground(&self) -> Hsla {
        self.muted_foreground
    }

    /// The link text color.
    pub fn link(&self) -> Hsla {
        self.link
    }

    /// The background painted behind selected text.
    pub fn selection(&self) -> Hsla {
        self.selection
    }

    /// The background of fenced code blocks and table header rows.
    pub fn code_background(&self) -> Hsla {
        self.code_background
    }

    /// The color of borders and horizontal rules.
    pub fn border(&self) -> Hsla {
        self.border
    }

    /// The gap between paragraphs.
    pub fn paragraph_gap(&self) -> Rems {
        self.paragraph_gap
    }

    /// The style refinement for a heading at `level` (1-6).
    pub fn heading(&self, level: u8) -> StyleRefinement {
        (self.heading)(level)
    }

    /// The style refinement for code blocks.
    pub fn code_block(&self) -> &StyleRefinement {
        &self.code_block
    }

    /// The style refinement for the table container.
    pub fn table(&self) -> &StyleRefinement {
        &self.table
    }

    /// The style refinement for table header rows.
    pub fn table_head(&self) -> &StyleRefinement {
        &self.table_head
    }

    /// The style refinement for table cells.
    pub fn table_cell(&self) -> &StyleRefinement {
        &self.table_cell
    }

    /// The highlight style for inline code, before the code-background
    /// fallback in [`Self::inline_code_highlight`] applies.
    pub fn inline_code(&self) -> HighlightStyle {
        self.inline_code
    }

    /// Explicit inline code typography and box refinements.
    pub fn inline_code_style(&self) -> &StyleRefinement {
        &self.inline_code_style
    }

    /// Explicit list marker refinements.
    pub fn list_style(&self) -> &StyleRefinement {
        &self.list_style
    }

    /// Explicit blockquote refinements.
    pub fn blockquote_style(&self) -> &StyleRefinement {
        &self.blockquote_style
    }

    /// Whether content-specific assets should use their dark variant.
    pub fn is_dark(&self) -> bool {
        self.is_dark
    }

    /// The table body background, when it is not the theme surface.
    pub(crate) fn table_background(&self) -> Option<Hsla> {
        self.table_background
    }

    /// Returns the [`HighlightStyle`] to use for inline code, falling back to
    /// the code background when no custom background was supplied.
    pub(crate) fn inline_code_highlight(&self) -> HighlightStyle {
        let mut style = self.inline_code;
        if style.background_color.is_none() {
            style.background_color = Some(self.code_background);
        }
        style
    }

    /// This style adapted to body text drawn in `color`, the text color a
    /// container sets for its surface.
    ///
    /// The body text always takes `color`. When `color` is far from this
    /// style's foreground in lightness, the surface is inverted from the one
    /// this style was made for (a `primary` fill, say): the link, muted text,
    /// code, border and selection colors would vanish on it, so they are all
    /// derived from `color`, and [`Self::is_dark`] flips.
    pub(crate) fn on_text_color(&self, color: Hsla) -> Self {
        let style = self.clone().with_foreground(color);
        if !self.is_inverted_by(color) {
            return style;
        }

        let code_background = color.opacity(0.12);
        let mut table_head = self.table_head.clone();
        table_head.background = Some(code_background.into());
        table_head.text.color = Some(color);
        let mut style = style
            .with_muted_foreground(color.opacity(0.7))
            .with_link(color)
            .with_selection(color.opacity(0.25))
            .with_code_background(code_background)
            .with_border(color.opacity(0.2))
            .with_inline_code(HighlightStyle {
                background_color: Some(code_background),
                ..self.inline_code
            })
            .with_table_head(table_head)
            .with_dark(!self.is_dark);
        style.table_background = Some(gpui::transparent_black());
        style
    }

    /// Whether body text in `color` sits on a surface inverted from the one
    /// this style was made for.
    ///
    /// Mid-tone text such as a destructive red reads on either kind of surface,
    /// so only a lightness gap wider than that counts as inverted.
    pub(crate) fn is_inverted_by(&self, color: Hsla) -> bool {
        const INVERTED_LIGHTNESS_GAP: f32 = 0.6;
        (oklab_lightness(color) - oklab_lightness(self.foreground)).abs() > INVERTED_LIGHTNESS_GAP
    }
}

/// The perceptual (Oklab) lightness of `color`, from 0 (black) to 1 (white).
fn oklab_lightness(color: Hsla) -> f32 {
    let rgb = color.to_rgb();
    let linear = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (linear(rgb.r), linear(rgb.g), linear(rgb.b));
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s
}

/// Drop an invalid font size before it can replace an inherited value or
/// make retained style equality fail every frame (NaN is never equal).
pub(super) fn normalize_inline_code_style(mut style: StyleRefinement) -> StyleRefinement {
    if style.text.font_size.is_some_and(|size| {
        let size = size.to_pixels(gpui::px(1.)).as_f32();
        !size.is_finite() || size <= 0.
    }) {
        style.text.font_size = None;
    }
    style
}

#[cfg(test)]
mod tests {
    use gpui::{Styled as _, px};

    use super::*;

    #[test]
    fn callout_and_link_underline_defaults_are_the_old_literals_and_count_in_equality() {
        let base = TextViewStyle::default();
        assert_eq!(base.callout_fill(), 0.13);
        assert_eq!(base.clone().with_dark(true).callout_fill(), 0.09);
        assert_eq!(base.callout_bar_width(), px(2.));
        assert_eq!(base.callout_bar_alpha(), 0.85);
        assert_eq!(base.callout_radius(), px(6.));
        assert_eq!(base.link_underline_thickness(), px(1.));
        assert_eq!(base.link_underline_alpha(), 0.4);
        assert!(base != base.clone().with_callout_fill(0.1, 0.2));
        assert!(base != base.clone().with_callout_bar(px(3.), 0.85));
        assert!(base != base.clone().with_callout_radius(px(8.)));
        assert!(base != base.clone().with_link_underline(px(2.), 0.4));
    }

    #[test]
    fn selection_layout_fingerprint_covers_callback_table_and_theme_fields() {
        let base = TextViewStyle::default();
        let heading = base
            .clone()
            .with_heading(|_| StyleRefinement::default().text_size(px(14.)));
        assert!(
            heading
                == base
                    .clone()
                    .with_heading(|_| StyleRefinement::default().text_size(px(14.)))
        );
        assert!(
            heading
                != base
                    .clone()
                    .with_heading(|_| StyleRefinement::default().text_size(px(28.)))
        );

        let mut table = StyleRefinement::default();
        table.text.white_space = Some(gpui::WhiteSpace::Nowrap);
        assert!(base != base.clone().with_table_cell(table));

        assert!(base != base.clone().with_dark(true));
        assert!(
            base != base
                .clone()
                .with_inline_code_style(StyleRefinement::default().border_color(gpui::red()))
        );
        assert!(
            base != base
                .clone()
                .with_list_style(StyleRefinement::default().text_color(gpui::red()))
        );
        assert!(
            base != base
                .clone()
                .with_blockquote_style(StyleRefinement::default().border_color(gpui::red()))
        );
    }

    #[test]
    fn heading_equality_skips_shared_callbacks_and_compares_distinct_ones() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_heading = calls.clone();
        let style = TextViewStyle::default().with_heading(move |_| {
            calls_for_heading.fetch_add(1, Ordering::Relaxed);
            StyleRefinement::default().text_size(px(14.))
        });

        assert!(style == style.clone());
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(style != style.clone().with_paragraph_gap(rems(1.)));
        assert!(style != style.clone().with_dark(!style.is_dark()));
        assert_eq!(calls.load(Ordering::Relaxed), 0);

        let equal = style
            .clone()
            .with_heading(|_| StyleRefinement::default().text_size(px(14.)));
        assert!(style == equal);
        assert_eq!(calls.load(Ordering::Relaxed), 6);

        calls.store(0, Ordering::Relaxed);
        let different = style.clone().with_heading(|level| {
            StyleRefinement::default().text_size(px(if level == 6 { 28. } else { 14. }))
        });
        assert!(style != different);
        assert_eq!(calls.load(Ordering::Relaxed), 6);
    }

    #[test]
    fn default_style_is_readable_without_an_application_theme() {
        let style = TextViewStyle::default();

        assert_eq!(style.foreground().a, 1.0);
        assert_eq!(style.link().a, 1.0);
        assert!(style.selection().a > 0.0);
        assert!(style.inline_code().background_color.is_some());
        assert!(style.code_background().a > 0.0);
        assert!(style.border().a > 0.0);
        assert_eq!(style.code_block().corner_radii.top_left, None);
        assert_eq!(style.code_block().corner_radii.top_right, None);
        assert_eq!(style.code_block().corner_radii.bottom_left, None);
        assert_eq!(style.code_block().corner_radii.bottom_right, None);
    }

    #[test]
    fn heading_refinement_defaults_empty_and_resolves_by_level() {
        let base = TextViewStyle::default();
        assert_eq!(base.heading(1), StyleRefinement::default());

        let style = base.clone().with_heading(|level| match level {
            1 => StyleRefinement::default().pt(rems(1.)).pb(rems(0.5)),
            _ => StyleRefinement::default().pb(rems(0.25)),
        });

        assert_eq!(
            style.heading(1),
            StyleRefinement::default().pt(rems(1.)).pb(rems(0.5))
        );
        assert_eq!(style.heading(2), StyleRefinement::default().pb(rems(0.25)));
        assert!(style != base);
    }

    #[test]
    fn inline_code_falls_back_to_the_code_background() {
        let style = TextViewStyle::default()
            .with_code_background(gpui::rgb(0x123456).into())
            .with_inline_code(HighlightStyle::default());

        assert_eq!(
            style.inline_code_highlight().background_color,
            Some(gpui::rgb(0x123456).into())
        );
    }

    #[test]
    fn inline_code_refinements_remain_explicit_on_an_inverted_surface() {
        let colors = ColorTokens::light();
        let plain = TextViewStyle::from_colors(&colors, false);
        assert_eq!(plain.inline_code_style(), &StyleRefinement::default());
        for size in [f32::NAN, f32::INFINITY, 0., -1.] {
            assert!(
                plain
                    .clone()
                    .with_inline_code_style(StyleRefinement::default().text_size(px(size)))
                    .inline_code_style()
                    .text
                    .font_size
                    .is_none()
            );
        }
        let refinement = StyleRefinement::default()
            .text_size(gpui::rems(1.))
            .bg(gpui::red())
            .border_1()
            .border_color(gpui::blue());
        let style = plain.with_inline_code_style(refinement.clone());
        assert_eq!(style.inline_code_style(), &refinement);
        assert_eq!(
            style
                .on_text_color(colors.primary_foreground)
                .inline_code_style(),
            &refinement
        );
    }

    #[test]
    fn list_and_blockquote_refinements_remain_explicit_on_an_inverted_surface() {
        let colors = ColorTokens::light();
        let plain = TextViewStyle::from_colors(&colors, false);
        assert_eq!(plain.list_style(), &StyleRefinement::default());
        assert_eq!(plain.blockquote_style(), &StyleRefinement::default());

        let marker = StyleRefinement::default()
            .text_color(gpui::red())
            .font_weight(gpui::FontWeight::BOLD);
        let quote = StyleRefinement::default()
            .border_l_4()
            .border_color(gpui::blue())
            .bg(gpui::green());
        let style = plain
            .with_list_style(marker.clone())
            .with_blockquote_style(quote.clone());
        assert_eq!(style.list_style(), &marker);
        assert_eq!(style.blockquote_style(), &quote);

        let inverted = style.on_text_color(colors.primary_foreground);
        assert_eq!(inverted.list_style(), &marker);
        assert_eq!(inverted.blockquote_style(), &quote);
    }

    #[test]
    fn text_color_of_a_matching_surface_only_replaces_the_body_text() {
        let style = TextViewStyle::from_colors(&ColorTokens::light(), false);
        let destructive = ColorTokens::light().destructive;

        let adapted = style.on_text_color(destructive);
        assert_eq!(adapted.foreground(), destructive);
        assert_eq!(adapted.link(), style.link());
        assert_eq!(adapted.muted_foreground(), style.muted_foreground());
        assert_eq!(adapted.code_background(), style.code_background());
        assert_eq!(adapted.table_background(), None);
        assert!(!adapted.is_dark());
    }

    #[test]
    fn text_color_of_an_inverted_surface_derives_every_color_from_it() {
        for (colors, is_dark) in [(ColorTokens::light(), false), (ColorTokens::dark(), true)] {
            let style = TextViewStyle::from_colors(&colors, is_dark);
            let text = colors.primary_foreground;

            let adapted = style.on_text_color(text);
            assert_eq!(adapted.foreground(), text);
            assert_eq!(adapted.link(), text);
            assert_eq!(adapted.muted_foreground(), text.opacity(0.7));
            assert_eq!(adapted.code_background(), text.opacity(0.12));
            assert_eq!(
                adapted.inline_code_highlight().background_color,
                Some(text.opacity(0.12))
            );
            assert_eq!(adapted.border(), text.opacity(0.2));
            assert_eq!(adapted.selection(), text.opacity(0.25));
            assert_eq!(adapted.table_background(), Some(gpui::transparent_black()));
            assert_eq!(adapted.table_head().text.color, Some(text));
            assert_eq!(adapted.is_dark(), !is_dark);
        }
    }

    #[test]
    fn from_theme_maps_base_semantic_tokens() {
        let mut theme = crate::Theme::default();
        theme.tokens.colors.foreground = gpui::rgb(0x112233).into();
        theme.tokens.colors.muted_foreground = gpui::rgb(0x445566).into();
        theme.tokens.colors.primary = gpui::rgb(0x3366ff).into();
        theme.tokens.colors.accent = gpui::rgb(0xddeeff).into();
        theme.tokens.colors.border = gpui::rgb(0x778899).into();
        theme.tokens.colors.selection = gpui::rgb(0x55a0fc).into();

        let style = TextViewStyle::from_theme(&theme);
        assert_eq!(style.foreground(), theme.tokens.colors.foreground);
        assert_eq!(style.link(), theme.tokens.colors.primary);
        assert_eq!(style.selection(), theme.tokens.colors.selection);
        assert_eq!(style.code_background(), theme.tokens.colors.accent);
        assert_eq!(style.border(), theme.tokens.colors.border);
    }
}
