//! File > Export as HTML: a Markdown file's text as one self-contained page. The vendored
//! Markdown crate renders the body; this wraps it in a full document with an inline style that
//! follows the preview's look (the sizes, alphas and colors of `tokens.rs`, light and dark by
//! `prefers-color-scheme`), and turns local pictures into data URLs so the file travels alone.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::tokens;

/// A local picture of at most this many bytes is inlined as a data URL; a bigger one keeps its address.
pub const INLINE_LIMIT: u64 = 5 * 1024 * 1024;

/// The page for `text`: `title` names it, `base` is the folder relative picture paths start from
/// (the Markdown file's folder, `None` for a page with no file).
pub fn document(text: &str, title: &str, base: Option<&Path>) -> String {
    let body = body(text, base);
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{}</title>\n<style>\n{}</style>\n</head>\n<body>\n<main>\n{}\n</main>\n</body>\n</html>\n",
        escape(title),
        style(),
        body
    )
}

/// `text` as the HTML fragment `document` wraps, local pictures inlined.
fn body(text: &str, base: Option<&Path>) -> String {
    let mut options = markdown::Options::gfm();
    options.parse.constructs.frontmatter = true;
    let body = markdown::to_html_with_options(text, &options).unwrap_or_default();
    inline_images(&body, base)
}

/// The page Copy as Rich Text puts on the pasteboard for `text`: the same body as `document`, with
/// a light, plain style (no page background, no width limit, every color a solid hex, since the
/// importers of Mail, Pages and Notes read no variables or alpha) so it reads well pasted into a
/// white message or page.
pub fn pasteboard(text: &str, base: Option<&Path>) -> String {
    format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<style>\n{}</style>\n</head>\n<body>\n{}\n</body>\n</html>\n",
        pasteboard_style(),
        body(text, base)
    )
}

/// Writes `html` to `path` through a sibling temp file and a rename, so a failure leaves the
/// old file (or none) and never half a page.
pub fn write(path: &Path, html: &str) -> io::Result<()> {
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let temp = path.with_file_name(format!(".{name}.export-tmp"));
    let result = fs::write(&temp, html.as_bytes()).and_then(|()| crate::document::full_sync(&fs::File::open(&temp)?)).and_then(|()| fs::rename(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// The page's name for a file: its name without the extension, `Untitled` with none.
pub fn title_of(path: Option<&Path>) -> String {
    path.and_then(|path| path.file_stem()).map_or_else(|| "Untitled".to_owned(), |stem| stem.to_string_lossy().into_owned())
}

/// The inline style: the preview's type, spacing and washes, light by default and dark under
/// `prefers-color-scheme`.
fn style() -> String {
    let px = |value: gpui_kit::Pixels| f32::from(value);
    let [h1, h2, h3, h4] = tokens::PREVIEW_HEADINGS;
    let (light_text, dark_text) = (rgb(tokens::TEXT_LIGHT), "229 229 229".to_owned());
    format!(
        "\
:root {{
  color-scheme: light dark;
  --fg: {light_text};
  --secondary: {secondary_light};
  --ground: 255 255 255;
  --link: #{link_light:06X};
}}
@media (prefers-color-scheme: dark) {{
  :root {{
    --fg: {dark_text};
    --secondary: {secondary_dark};
    --ground: 30 30 30;
    --link: #{link_dark:06X};
  }}
}}
html {{ background: rgb(var(--ground)); }}
body {{
  margin: 0;
  color: rgb(var(--fg));
  font: {body}px/{line} -apple-system, BlinkMacSystemFont, \"SF Pro Text\", system-ui, sans-serif;
  -webkit-font-smoothing: antialiased;
}}
main {{ max-width: {measure}px; margin: 0 auto; padding: {inset}px {reading}px; }}
h1, h2, h3, h4, h5, h6 {{ font-weight: 600; line-height: 1.25; margin: 1.6em 0 0.5em; }}
h1 {{ font-size: {h1}px; }}
h2 {{ font-size: {h2}px; }}
h3 {{ font-size: {h3}px; }}
h4, h5, h6 {{ font-size: {h4}px; }}
main > :first-child {{ margin-top: 0; }}
p, ul, ol, blockquote, pre, table {{ margin: 0 0 1em; }}
a {{ color: var(--link); text-decoration: none; }}
a:hover {{ text-decoration: underline; }}
img {{ max-width: 100%; height: auto; border-radius: {card}px; }}
blockquote {{
  padding: 0 0 0 1em;
  border-left: {bar}px solid rgb(var(--fg) / {bar_alpha});
  color: rgb(var(--fg) / var(--secondary));
}}
li::marker {{ color: rgb(var(--fg) / var(--secondary)); }}
code, pre {{
  font-family: ui-monospace, \"SF Mono\", Menlo, monospace;
  font-size: {code_em}em;
}}
code {{ background: rgb(var(--fg) / {surface}); border-radius: {tint}px; padding: 0.1em 0.3em; }}
pre {{
  background: rgb(var(--fg) / {card_alpha});
  border: 1px solid rgb(var(--fg) / {hairline});
  border-radius: {card}px;
  padding: 0.8em 1em;
  line-height: {code_line};
  overflow-x: auto;
}}
pre code {{ background: none; padding: 0; font-size: 1em; }}
table {{ border-collapse: collapse; background: rgb(var(--fg) / {card_alpha}); }}
th, td {{ border: 1px solid rgb(var(--fg) / {hairline}); padding: 0.35em 0.7em; font-variant-numeric: tabular-nums; }}
th {{ background: rgb(var(--fg) / {surface}); text-align: left; }}
hr {{ border: 0; border-top: 1px solid rgb(var(--fg) / {hairline}); margin: 1.6em 0; }}
input[type=checkbox] {{ margin: 0 0.5em 0 0; }}
li.task-list-item, ul.contains-task-list {{ list-style: none; }}
section[data-footnotes] {{ border-top: 1px solid rgb(var(--fg) / {hairline}); margin-top: 2em; font-size: 0.9em; }}
sup a {{ font-size: 11px; }}
",
        link_light = tokens::LINK_BLUE_LIGHT,
        link_dark = tokens::LINK_BLUE,
        body = tokens::PREVIEW_BODY,
        line = tokens::PREVIEW_LINE,
        measure = px(tokens::PREVIEW_MEASURE) + 2. * px(tokens::PREVIEW_READING_INSET),
        inset = px(tokens::PREVIEW_INSET) * 2.,
        reading = px(tokens::PREVIEW_READING_INSET),
        card = px(tokens::RADIUS_CARD),
        tint = px(tokens::RADIUS_TINT),
        bar = px(tokens::QUOTE_BAR),
        bar_alpha = tokens::ALPHA_QUOTE_BAR,
        secondary_light = tokens::ink_alpha(true, tokens::ALPHA_SECONDARY),
        secondary_dark = tokens::ink_alpha(false, tokens::ALPHA_SECONDARY),
        code_em = tokens::PREVIEW_CODE_EM,
        code_line = tokens::PREVIEW_CODE_LINE,
        surface = tokens::ALPHA_SURFACE,
        card_alpha = tokens::ALPHA_CARD,
        hairline = tokens::ALPHA_HAIRLINE,
    )
}

/// The style of `pasteboard`: the preview's type and washes in light colors, each wash blended
/// over white into one solid hex.
fn pasteboard_style() -> String {
    let px = |value: gpui_kit::Pixels| f32::from(value);
    let [h1, h2, h3, h4] = tokens::PREVIEW_HEADINGS;
    format!(
        "\
body {{ color: #{text:06X}; font-family: -apple-system, \"SF Pro Text\", \"Helvetica Neue\", sans-serif; font-size: {body}px; line-height: {line}; }}
h1, h2, h3, h4, h5, h6 {{ font-weight: 600; line-height: 1.25; margin: 1.2em 0 0.4em; }}
h1 {{ font-size: {h1}px; }}
h2 {{ font-size: {h2}px; }}
h3 {{ font-size: {h3}px; }}
h4, h5, h6 {{ font-size: {h4}px; }}
p, ul, ol, blockquote, pre, table {{ margin: 0 0 0.8em; }}
a {{ color: #{link:06X}; }}
blockquote {{ padding: 0 0 0 1em; border-left: {bar}px solid #{bar_color}; color: #{secondary}; }}
code, pre {{ font-family: \"SF Mono\", Menlo, monospace; font-size: {code_em}em; }}
code {{ background-color: #{surface}; }}
pre {{ background-color: #{card}; padding: 0.6em 0.8em; }}
pre code {{ background-color: transparent; font-size: 1em; }}
table {{ border-collapse: collapse; }}
th, td {{ border: 1px solid #{hairline}; padding: 0.3em 0.6em; }}
th {{ background-color: #{surface}; text-align: left; }}
hr {{ border: 0; border-top: 1px solid #{hairline}; }}
",
        text = tokens::TEXT_LIGHT,
        link = tokens::LINK_BLUE_LIGHT,
        body = tokens::PREVIEW_BODY,
        line = tokens::PREVIEW_LINE,
        bar = px(tokens::QUOTE_BAR),
        bar_color = wash(tokens::ALPHA_QUOTE_BAR),
        secondary = wash(tokens::ink_alpha(true, tokens::ALPHA_SECONDARY)),
        code_em = tokens::PREVIEW_CODE_EM,
        surface = wash(tokens::ALPHA_SURFACE),
        card = wash(tokens::ALPHA_CARD),
        hairline = wash(tokens::ALPHA_HAIRLINE),
    )
}

/// The light text color at `alpha` over white, as `RRGGBB`.
fn wash(alpha: f32) -> String {
    let channel = |shift: u32| {
        let ink = (tokens::TEXT_LIGHT >> shift & 0xFF) as f32;
        (255. + (ink - 255.) * alpha).round() as u32
    };
    format!("{:02X}{:02X}{:02X}", channel(16), channel(8), channel(0))
}

/// `0x423E39` as CSS channels: `66 62 57`.
fn rgb(hex: u32) -> String {
    format!("{} {} {}", hex >> 16 & 0xFF, hex >> 8 & 0xFF, hex & 0xFF)
}

/// Text safe inside an element or a double-quoted attribute.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// Swaps the `src` of each `<img>` that names a small local file for a data URL.
fn inline_images(html: &str, base: Option<&Path>) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find("<img ") {
        let (head, tail) = rest.split_at(at);
        out.push_str(head);
        let end = tail.find('>').map_or(tail.len(), |end| end + 1);
        out.push_str(&inline_tag(&tail[..end], base));
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn inline_tag(tag: &str, base: Option<&Path>) -> String {
    const SRC: &str = " src=\"";
    let Some(start) = tag.find(SRC).map(|at| at + SRC.len()) else { return tag.to_owned() };
    let Some(len) = tag[start..].find('"') else { return tag.to_owned() };
    match data_url(&tag[start..start + len], base) {
        Some(url) => format!("{}{}{}", &tag[..start], url, &tag[start + len..]),
        None => tag.to_owned(),
    }
}

/// The data URL for the picture `src` names, or `None` to keep `src`: a remote address, a file
/// that is missing, over `INLINE_LIMIT` or of a kind with no known type.
fn data_url(src: &str, base: Option<&Path>) -> Option<String> {
    let path = local_path(&unescape(src), base)?;
    let mime = mime_of(&path)?;
    fs::metadata(&path).ok().filter(|meta| meta.is_file() && meta.len() <= INLINE_LIMIT)?;
    let bytes = fs::read(&path).ok()?;
    Some(format!("data:{mime};base64,{}", base64(&bytes)))
}

/// The file an image address means: an absolute path, a `file://` address or a path relative to `base`.
fn local_path(src: &str, base: Option<&Path>) -> Option<PathBuf> {
    let src = src.strip_prefix("file://").unwrap_or(src);
    let scheme = src.split_once(':').is_some_and(|(scheme, _)| !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')));
    if src.is_empty() || src.starts_with("//") || scheme {
        return None;
    }
    let decoded = percent_decode(src);
    let path = Path::new(&decoded);
    Some(if path.is_absolute() { path.to_path_buf() } else { base?.join(path) })
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = |byte: u8| (byte as char).to_digit(16);
        match (bytes[at], bytes.get(at + 1).and_then(|&b| hex(b)), bytes.get(at + 2).and_then(|&b| hex(b))) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                at += 3;
            }
            (byte, ..) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn mime_of(path: &Path) -> Option<&'static str> {
    Some(match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "tif" | "tiff" => "image/tiff",
        "avif" => "image/avif",
        "heic" => "image/heic",
        _ => return None,
    })
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for (at, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if at <= chunk.len() {
                out.push(ALPHABET[(word >> shift & 0x3F) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("finch-export-html-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_wrapper_has_title_charset_and_style() {
        let page = document("# Hi\n\ntext", "notes", None);
        assert!(page.starts_with("<!DOCTYPE html>"));
        assert!(page.contains("<meta charset=\"utf-8\">"));
        assert!(page.contains("<title>notes</title>"));
        assert!(page.contains("<style>") && page.contains("prefers-color-scheme: dark"));
        assert!(page.contains("<h1>Hi</h1>"));
    }

    #[test]
    fn the_title_is_escaped() {
        assert!(document("x", "a <b> & c", None).contains("<title>a &lt;b&gt; &amp; c</title>"));
    }

    #[test]
    fn a_relative_picture_becomes_a_data_url() {
        let dir = scratch("relative");
        fs::write(dir.join("my pic.png"), [1u8, 2, 3, 4]).unwrap();
        let page = document("![alt](my%20pic.png)", "n", Some(&dir));
        assert!(page.contains("src=\"data:image/png;base64,AQIDBA==\""), "{page}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_absolute_picture_becomes_a_data_url() {
        let dir = scratch("absolute");
        let file = dir.join("a.jpg");
        fs::write(&file, [255u8, 0]).unwrap();
        let page = document(&format!("![a]({})", file.display()), "n", None);
        assert!(page.contains("src=\"data:image/jpeg;base64,/wA=\""), "{page}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_remote_picture_is_kept() {
        let dir = scratch("remote");
        let page = document("![a](https://example.com/a.png)", "n", Some(&dir));
        assert!(page.contains("src=\"https://example.com/a.png\""));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_picture_is_kept() {
        let dir = scratch("missing");
        let page = document("![a](gone.png)", "n", Some(&dir));
        assert!(page.contains("src=\"gone.png\""));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_picture_over_the_limit_is_kept() {
        let dir = scratch("big");
        fs::write(dir.join("big.png"), vec![0u8; INLINE_LIMIT as usize + 1]).unwrap();
        let page = document("![a](big.png)", "n", Some(&dir));
        assert!(page.contains("src=\"big.png\""));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn base64_pads() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
    }

    #[test]
    fn write_replaces_the_file_and_leaves_no_temp() {
        let dir = scratch("write");
        let file = dir.join("out.html");
        fs::write(&file, "old").unwrap();
        write(&file, "new").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "new");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_pasteboard_page_keeps_structure_and_is_light() {
        let page = pasteboard("# Title\n\nSee [docs](https://example.com/a).\n\n| a | b |\n|---|---|\n| 1 | 2 |\n", None);
        assert!(page.contains("<h1>Title</h1>"), "{page}");
        assert!(page.contains("href=\"https://example.com/a\""));
        assert!(page.contains("<table>") && page.contains("<th>a</th>"));
        assert!(!page.contains("prefers-color-scheme") && !page.contains("--ground") && !page.contains("30 30 30"));
        assert!(!page.contains("html {") && !page.contains("var(--"), "no page background or variables");
    }
}
