//! LaTeX math to SVG for the preview's `$...$` and `$$...$$`.
//!
//! `latex-rust` lays the formula out with TeX's rules and STIX Two Math, and
//! draws every glyph as an SVG `<path>`, so resvg (GPUI's SVG path) needs no
//! font and no CSS. The 840 KB font is embedded in the crate and parsed on the
//! first render, never at launch.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::LazyLock;

use latex_rust::{
    Color, Dim, MathFont, MathStyle, SvgOptions, layout_with_em_size_pt, parse, render_svg,
};

/// A rendered formula, in logical px at the font size it was rendered for.
pub struct MathImage {
    pub svg: String,
    pub width: f32,
    pub height: f32,
    /// Distance from the top of the image to the formula's baseline, for
    /// lining inline math up with the text around it.
    pub baseline: f32,
}

static FONT: LazyLock<Result<MathFont, String>> =
    LazyLock::new(|| MathFont::stix_two_math().map_err(|e| e.to_string()));

/// Pure and blocking; call from the background executor. `display` is
/// `$$...$$` (display style, the caller centers the block), else inline `$...$`.
/// `color` is `0xRRGGBB`. Bad TeX, or a crate panic, gives `Err(message)`.
pub fn render(tex: &str, display: bool, font_size: f32, color: u32) -> Result<MathImage, String> {
    catch_unwind(AssertUnwindSafe(|| render_inner(tex, display, font_size, color)))
        .unwrap_or_else(|_| Err("math renderer failed".to_string()))
}

fn render_inner(tex: &str, display: bool, font_size: f32, color: u32) -> Result<MathImage, String> {
    if !(font_size.is_finite() && font_size > 0.0) {
        return Err("font size must be positive".to_string());
    }
    let font = FONT.as_ref().map_err(Clone::clone)?;
    let ast = parse(tex).map_err(|e| e.to_string())?;
    // The crate's lengths are "pt", but its SVG numbers are plain user units;
    // the header below says px so one unit is one logical px.
    let em = Dim::parse(&format!("{font_size:.3}"));
    let style = if display { MathStyle::Display } else { MathStyle::Text };
    let tree = layout_with_em_size_pt(&ast, font, style, &em).map_err(|e| e.to_string())?;
    let options = SvgOptions {
        font_size_pt: em.clone(),
        color: Color::rgb((color >> 16) as u8, (color >> 8) as u8, color as u8),
        display,
    };
    let svg = render_svg(&tree, font, &options).map_err(|e| e.to_string())?;

    let px = |d: &Dim| (d * &em).to_svg_string().parse::<f32>().unwrap_or(0.0);
    let width = px(&tree.width);
    let height = px(&(&tree.height + &tree.depth));
    let baseline = px(&tree.height);
    if !(width > 0.0 && height > 0.0) {
        return Err("empty formula".to_string());
    }
    Ok(MathImage { svg: px_header(&svg, width, height), width, height, baseline })
}

/// Swaps the `<svg ...>` tag's `pt` sizes for `px` ones (resvg reads 1pt as
/// 1.33px); the viewBox and every coordinate stay as they were.
fn px_header(svg: &str, width: f32, height: f32) -> String {
    let Some(start) = svg.find("<svg ") else {
        return svg.to_string();
    };
    let end = start + svg[start..].find('>').map_or(0, |i| i + 1);
    let tag = &svg[start..end];
    let view_box = tag
        .find("viewBox=\"")
        .and_then(|i| tag[i..].split('"').nth(1))
        .unwrap_or("");
    format!(
        "{}<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}px\" height=\"{height}px\" viewBox=\"{view_box}\">{}",
        &svg[..start],
        &svg[end..],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_and_sum_render_as_plain_paths() {
        for tex in [r"\frac{a+b}{2}", r"\sum_{i=1}^{n} x_i^2"] {
            let m = render(tex, true, 16.0, 0x112233).expect(tex);
            assert!(m.width > 0.0 && m.height > 0.0, "{tex}");
            assert!(m.baseline > 0.0 && m.baseline < m.height, "{tex}");
            assert!(!m.svg.contains("var("), "{tex}");
            assert!(m.svg.contains("<path"), "{tex}");
            assert!(m.svg.contains("#112233"), "{tex}");
            assert!(!m.svg.contains("pt\""), "{tex}");
        }
    }

    #[test]
    fn bad_tex_is_an_error_not_a_panic() {
        assert!(render(r"\frac{1", false, 16.0, 0).is_err());
        assert!(render(r"\notamacro{x}", false, 16.0, 0).is_err());
        assert!(render("", false, 16.0, 0).is_err());
        assert!(render("x", false, 0.0, 0).is_err());
    }
}
