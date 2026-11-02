//! Heading anchors: the `#slug` of a heading, as GitHub and Zed make it, and finding the heading
//! a link names.

use std::collections::HashMap;

/// The slug of one heading's text: trimmed and lowercased, letters, digits (Unicode too), `-` and
/// `_` kept, spaces turned to `-`, everything else dropped. The preview draws `--` and `---` as an
/// en and em dash, which slug back to `--` and `---` so the anchor is the one the source spells.
pub fn slug(text: &str) -> String {
    let mut slug = String::with_capacity(text.len());
    for c in text.trim().chars() {
        if c.is_alphanumeric() {
            slug.extend(c.to_lowercase());
        } else if c == '-' || c == '_' {
            slug.push(c);
        } else if c == '–' {
            slug.push_str("--");
        } else if c == '—' {
            slug.push_str("---");
        } else if c == ' ' {
            slug.push('-');
        }
    }
    slug
}

/// The source byte where the heading with anchor `anchor` starts, among `headings` (each one's
/// start and text, in document order). A slug used before gets `-1`, `-2` and so on; the first
/// heading of a slug has none. `anchor` is compared lowercased.
pub fn heading_start(headings: &[(usize, String)], anchor: &str) -> Option<usize> {
    let anchor = anchor.to_lowercase();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (start, text) in headings {
        let base = slug(text);
        let count = seen.entry(base.clone()).or_insert(0);
        let unique = if *count == 0 { base } else { format!("{base}-{count}") };
        *count += 1;
        if unique == anchor {
            return Some(*start);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{heading_start, slug};

    #[test]
    fn slug_lowercases_and_dashes_spaces() {
        assert_eq!(slug("Getting Started"), "getting-started");
        assert_eq!(slug("  Padded  "), "padded");
    }

    #[test]
    fn slug_drops_punctuation_and_keeps_dash_and_underscore() {
        assert_eq!(slug("What's new? (v2.0)"), "whats-new-v20");
        assert_eq!(slug("snake_case - and-dash"), "snake_case---and-dash");
        assert_eq!(slug("Before – after"), "before----after");
        assert_eq!(slug("a — b"), "a-----b");
    }

    #[test]
    fn slug_keeps_unicode_letters() {
        assert_eq!(slug("Über Café 日本語"), "über-café-日本語");
        assert_eq!(slug("İstanbul"), "i\u{307}stanbul");
    }

    #[test]
    fn duplicate_headings_get_numbered_slugs() {
        let headings = vec![
            (0, "Setup".to_owned()),
            (10, "Usage".to_owned()),
            (20, "Setup".to_owned()),
            (30, "Setup".to_owned()),
        ];
        assert_eq!(heading_start(&headings, "setup"), Some(0));
        assert_eq!(heading_start(&headings, "setup-1"), Some(20));
        assert_eq!(heading_start(&headings, "setup-2"), Some(30));
        assert_eq!(heading_start(&headings, "usage"), Some(10));
        assert_eq!(heading_start(&headings, "setup-3"), None);
    }

    #[test]
    fn anchor_matches_any_case_and_a_missing_one_is_none() {
        let headings = vec![(5, "Install".to_owned())];
        assert_eq!(heading_start(&headings, "Install"), Some(5));
        assert_eq!(heading_start(&headings, "nope"), None);
        assert_eq!(heading_start(&[], "install"), None);
    }
}
