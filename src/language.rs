//! Which GPUI Kit highlighter a file gets: its name first, then a `#!` line, then an Emacs or
//! Vim modeline.
//!
//! The name lists are Zed's `path_suffixes` (crates/grammars/src/*/config.toml and its
//! extensions), the longest match winning, so `CMakeLists.txt` beats `.txt`. The first-line
//! patterns are Zed's `first_line_pattern`s, and the modelines follow Zed's
//! crates/language/src/modeline.rs. Only languages the highlighter has are named. SQL has none
//! built in: its grammar crate pins a `cc` that clashes with the build's.

use std::path::Path;

/// Zed's path suffixes per highlighter language. An entry matches a file name that is the entry
/// or ends with `.` and the entry.
const SUFFIXES: &[(&str, &[&str])] = &[
    ("astro", &["astro"]),
    (
        "bash",
        &[
            "sh", "bash", "bashrc", "bash_profile", "bash_aliases", "bash_login", "bash_logout", "brushrc", "bats", "envrc", "profile",
            "zsh", "zshrc", "zshenv", "zsh_profile", "zsh_aliases", "zsh_histfile", "zlogin", "zprofile", "PKGBUILD", "APKBUILD", "ebuild",
        ],
    ),
    ("c", &["c"]),
    ("cmake", &["cmake", "CMakeLists.txt"]),
    (
        "cpp",
        &[
            "cc", "ccm", "hh", "cpp", "cppm", "h", "hpp", "cxx", "cxxm", "hxx", "c++", "c++m", "h++", "hip", "ipp", "inl", "ino", "ixx", "cu", "cuh",
            "C", "H",
        ],
    ),
    ("csharp", &["cs"]),
    ("css", &["css", "postcss", "pcss", "scss"]),
    ("diff", &["diff", "patch"]),
    ("ejs", &["ejs"]),
    ("elixir", &["ex", "exs"]),
    ("erb", &["erb"]),
    ("go", &["go"]),
    ("graphql", &["graphql", "gql"]),
    ("html", &["html", "htm", "shtml"]),
    ("java", &["java"]),
    ("javascript", &["js", "jsx", "mjs", "cjs"]),
    (
        "json",
        &[
            "json", "flake.lock", "geojson", "topojson", "prettierrc", "json.dist", "deno.lock", "jsonc", "bun.lock", "devcontainer.json",
            "pyrightconfig.json", "tsconfig.json", "luaurc", "swcrc", "babelrc", "eslintrc", "stylelintrc", "jshintrc",
        ],
    ),
    ("kotlin", &["kt", "kts", "ktm"]),
    ("lua", &["lua", "rockspec"]),
    ("make", &["Makefile", "makefile", "GNUmakefile", "mk", "mak", "make"]),
    ("markdown", &["md", "mdx", "mdwn", "mdc", "markdown", "MD"]),
    ("php", &["php", "php3", "php4", "php5", "phtml"]),
    ("proto", &["proto"]),
    ("python", &["py", "pyi", "mpy"]),
    (
        "ruby",
        &[
            "rb", "Gemfile", "Rakefile", "gemspec", "rake", "ru", "Guardfile", "Podfile", "Fastfile", "Brewfile", "Capfile", "Vagrantfile", "Thorfile",
            "Appraisals", "Berksfile", "Cheffile", "thor", "podspec", "jbuilder", "rbi",
        ],
    ),
    ("rust", &["rs"]),
    ("scala", &["scala", "sbt", "sc"]),
    ("svelte", &["svelte"]),
    ("swift", &["swift"]),
    ("toml", &["toml", "Cargo.lock", "Pipfile", "poetry.lock", "pdm.lock", "uv.lock"]),
    ("tsx", &["tsx"]),
    ("typescript", &["ts", "cts", "mts"]),
    ("yaml", &["yml", "yaml", "pixi.lock", "clang-format", "clangd", "bst"]),
    ("zig", &["zig", "zon"]),
];

/// A file longer than this many bytes gets no highlighter: the highlight tree costs about 40 MB
/// per MB of text (measured), with no ceiling, so past this the editor shows plain text.
/// Only the editor's highlighter is cut; the status line and the Markdown preview keep the real
/// language. Checked when a highlighter is set (open, rename, a tab coming back), not per key.
pub const MAX_HIGHLIGHT_BYTES: usize = 4 * 1024 * 1024;

/// The language to give the editor's highlighter for a file of `bytes` whose language is `lang`:
/// `lang`, or "text" (no grammar) past `MAX_HIGHLIGHT_BYTES`. The one rule every place that sets
/// the highlighter goes through.
pub fn for_highlighter(lang: &'static str, bytes: usize) -> &'static str {
    if bytes > MAX_HIGHLIGHT_BYTES { "text" } else { lang }
}

/// The highlighter language for a file name or ending alone, or "text" (no coloring) when it
/// isn't known. A file with no path is "text" too.
pub fn for_path(path: Option<&Path>) -> &'static str {
    let Some(path) = path else { return "text" };
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    // Exact case first, so `.C` is C++ and `.c` is C; then any case, as `README.MD` or `A.PY`.
    by_name(name, false).or_else(|| by_name(name, true)).unwrap_or("text")
}

/// The highlighter language for a file whose text is `text`: its name, else its first line, else
/// a modeline.
pub fn for_text(path: Option<&Path>, text: &str) -> &'static str {
    let by_path = for_path(path);
    if path.is_none() || by_path != "text" {
        return by_path;
    }
    let first: Vec<&str> = text.lines().take(EDGE_LINES).map(clip).collect();
    let last: Vec<&str> = text.lines().rev().take(EDGE_LINES).map(clip).collect::<Vec<_>>().into_iter().rev().collect();
    for_lines(path, &first, &last)
}

/// Like `for_text`, given only the first and last few lines (`EDGE_LINES` of each), already cut
/// to `LINE_CHARS`.
pub fn for_lines(path: Option<&Path>, first: &[&str], last: &[&str]) -> &'static str {
    let by_path = for_path(path);
    if path.is_none() || by_path != "text" {
        return by_path;
    }
    first
        .first()
        .and_then(|line| by_first_line(line))
        .or_else(|| by_modeline(first, last))
        .unwrap_or("text")
}

/// How many lines at each end of a file a modeline may sit in (Vim's default).
pub const EDGE_LINES: usize = 5;

/// How many characters of a line are looked at.
pub const LINE_CHARS: usize = 256;

/// `line` cut to its first `LINE_CHARS` characters.
pub fn clip(line: &str) -> &str {
    line.char_indices().nth(LINE_CHARS).map_or(line, |(at, _)| &line[..at])
}

fn by_name(name: &str, any_case: bool) -> Option<&'static str> {
    let same = |a: &[u8], b: &[u8]| if any_case { a.eq_ignore_ascii_case(b) } else { a == b };
    let mut best: Option<(&'static str, usize)> = None;
    for (lang, suffixes) in SUFFIXES {
        for suffix in *suffixes {
            let (name, suffix) = (name.as_bytes(), suffix.as_bytes());
            let matches = name.len() >= suffix.len()
                && same(&name[name.len() - suffix.len()..], suffix)
                && (name.len() == suffix.len() || name[name.len() - suffix.len() - 1] == b'.');
            if matches && best.is_none_or(|(_, len)| suffix.len() > len) {
                best = Some((lang, suffix.len()));
            }
        }
    }
    best.map(|(lang, _)| lang)
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether `word` sits in `line` with no word character on either side (the `\b` of Zed's
/// patterns).
fn has_word(line: &str, word: &str) -> bool {
    line.match_indices(word).any(|(at, _)| {
        !line[..at].chars().next_back().is_some_and(is_word) && !line[at + word.len()..].chars().next().is_some_and(is_word)
    })
}

/// `\bpython[0-9.]*\b`.
fn has_python(line: &str) -> bool {
    line.match_indices("python").any(|(at, _)| {
        if line[..at].chars().next_back().is_some_and(is_word) {
            return false;
        }
        let rest = &line[at + "python".len()..];
        let run = rest.bytes().take_while(|b| b.is_ascii_digit() || *b == b'.').count();
        // A boundary has a word character on one side only; try the longest run first.
        (0..=run).rev().any(|len| {
            let before = len == 0 || rest.as_bytes()[len - 1].is_ascii_digit();
            let after = rest[len..].chars().next().is_some_and(is_word);
            before != after
        })
    })
}

/// Whether `line`, the first line of a page with no name, starts a patch.
fn starts_patch(line: &str) -> bool {
    by_first_line(line) == Some("diff")
}

/// The language of a page with no name, by its first line (cut to `LINE_CHARS`): Diff for a
/// patch, Markdown for a heading (one to six `#` and a space) or front matter's `---`, else plain
/// text. `markdown`, a page made from a Markdown template, makes any first line but a patch's
/// Markdown.
pub fn for_untitled(line: &str, markdown: bool) -> &'static str {
    if starts_patch(line) {
        "diff"
    } else if markdown || starts_markdown(line) {
        "markdown"
    } else {
        "text"
    }
}

/// Whether `line` opens a Markdown page: an ATX heading or a front matter block.
fn starts_markdown(line: &str) -> bool {
    let line = line.strip_prefix('\u{feff}').unwrap_or(line);
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    line.trim_end() == "---" || ((1..=6).contains(&hashes) && line[hashes..].starts_with([' ', '\t']))
}

/// Zed's `first_line_pattern`s, for the languages we have.
fn by_first_line(line: &str) -> Option<&'static str> {
    if let Some(shebang) = line.strip_prefix("#!") {
        let has_any = |words: &[&str]| words.iter().any(|word| has_word(shebang, word));
        // The same order of Zed's languages: shell, Python, TypeScript, JavaScript, Ruby, Lua, Elixir, PHP.
        if has_any(&["ash", "bash", "bats", "dash", "sh", "zsh"]) {
            return Some("bash");
        }
        if has_python(shebang) || has_word(shebang, "uv run") {
            return Some("python");
        }
        if has_word(shebang, "deno run") || has_word(shebang, "ts-node") || has_word(shebang, "bun") || has_word(shebang, "tsx") {
            return Some("typescript");
        }
        if shebang.match_indices("node").any(|(at, _)| {
            shebang[..at].ends_with(['/', ' ']) && !shebang[at + 4..].chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_')
        }) {
            return Some("javascript");
        }
        for (word, lang) in [("ruby", "ruby"), ("lua", "lua"), ("elixir", "elixir"), ("php", "php")] {
            if has_word(shebang, word) {
                return Some(lang);
            }
        }
        return None;
    }
    if line.starts_with("//") && has_word(line, "go run") {
        return Some("go");
    }
    // A patch piped in (`git diff | finch -`) has no name: its first line says it (finch#9s91).
    if line.starts_with("diff --git ") || is_mbox_from(line) {
        return Some("diff");
    }
    None
}

/// `git format-patch`'s first line: `From <40 hex digits> <date>`.
fn is_mbox_from(line: &str) -> bool {
    line.strip_prefix("From ").map(str::as_bytes).is_some_and(|rest| rest.len() > 41 && rest[..40].iter().all(u8::is_ascii_hexdigit) && rest[40] == b' ')
}

/// A language from an Emacs or Vim modeline, as Zed reads them: Emacs `-*- mode: x -*-` in the
/// first lines (which then settles it), else a Vim `ft=` there, else an Emacs "Local Variables"
/// block or a Vim modeline in the last lines.
fn by_modeline(first: &[&str], last: &[&str]) -> Option<&'static str> {
    let mode = first
        .iter()
        .find_map(|line| emacs_modeline(line))
        .or_else(|| first.iter().filter_map(|line| vim_modeline(line)).last())
        .or_else(|| emacs_local_variables(last))
        .or_else(|| last.iter().filter_map(|line| vim_modeline(line)).last())?;
    by_mode(&mode)
}

/// The mode of `-*- mode: x; other: y -*-` or `-*- x -*-`; `None` when the line has no such part.
fn emacs_modeline(line: &str) -> Option<String> {
    let start = line.find("-*-")? + 3;
    let inner = &line[start..start + line[start..].find("-*-")?];
    let mut mode = None;
    for part in inner.split(';').map(str::trim).filter(|part| !part.is_empty()) {
        match part.split_once(':') {
            Some((key, value)) if key.trim().eq_ignore_ascii_case("mode") => mode = Some(value.trim().to_owned()),
            Some(_) => {}
            None => mode = Some(part.to_owned()),
        }
    }
    mode
}

/// The `mode:` of an Emacs `Local Variables:` block, its lines carrying the same comment prefix
/// and suffix as the `Local Variables:` line, up to `End:`.
fn emacs_local_variables(lines: &[&str]) -> Option<String> {
    const MARK: &str = "Local Variables:";
    let (start, at) = lines.iter().enumerate().find_map(|(i, line)| Some((i, line.find(MARK)?)))?;
    let (prefix, suffix) = (&lines[start][..at], &lines[start][at + MARK.len()..]);
    for line in &lines[start + 1..] {
        let content = line.strip_prefix(prefix)?.strip_suffix(suffix)?.trim();
        if content == "End:" {
            return None;
        }
        if let Some((key, value)) = content.split_once(':')
            && key.trim().eq_ignore_ascii_case("mode")
        {
            return Some(value.trim().to_owned());
        }
    }
    None
}

/// The `ft=`/`filetype=` of a Vim modeline (`vim: set ft=lua ts=4:` or `vim: ft=lua`).
fn vim_modeline(line: &str) -> Option<String> {
    // `vi:` and `vim:` may open the line; otherwise whitespace comes before the marker, and `ex:`
    // needs it.
    let mut rest = None;
    for (at, _) in line.match_indices(':') {
        let before = &line[..at];
        let marker = ["vim", "Vim", "vi", "ex"].into_iter().find(|marker| before.ends_with(marker));
        let Some(marker) = marker else { continue };
        let lead = &before[..before.len() - marker.len()];
        let spaced = lead.chars().next_back().is_some_and(char::is_whitespace);
        if spaced || (lead.is_empty() && marker != "ex") {
            rest = Some(line[at + 1..].trim_start());
            break;
        }
    }
    let rest = rest?;
    // "set ft=lua:" second form first, else the whole rest is options.
    let options = ["set", "se"]
        .into_iter()
        .find_map(|set| rest.strip_prefix(set).filter(|after| after.starts_with(char::is_whitespace)))
        .and_then(|after| {
            let mut escaped = false;
            let end = after.char_indices().find(|(_, c)| match (escaped, c) {
                (true, _) => {
                    escaped = false;
                    false
                }
                (false, '\\') => {
                    escaped = true;
                    false
                }
                (false, ':') => true,
                _ => false,
            })?;
            Some(&after[..end.0])
        })
        .unwrap_or(rest);
    let mut mode = None;
    for part in options.split(':').flat_map(str::split_whitespace) {
        if let Some((key, value)) = part.split_once('=')
            && matches!(key, "ft" | "filetype")
        {
            mode = Some(value.to_owned());
        }
    }
    mode
}

/// The highlighter language for an Emacs mode or Vim filetype name, any case.
fn by_mode(mode: &str) -> Option<&'static str> {
    let mode = mode.to_ascii_lowercase();
    let mode = mode.strip_suffix("-mode").unwrap_or(&mode);
    Some(match mode {
        "astro" => "astro",
        "sh" | "bash" | "zsh" | "shell" | "shell-script" | "shellscript" | "ksh" => "bash",
        "c" => "c",
        "cmake" => "cmake",
        "c++" | "cpp" => "cpp",
        "csharp" | "c#" | "cs" => "csharp",
        "css" | "scss" => "css",
        "diff" => "diff",
        "ejs" => "ejs",
        "elixir" | "ex" => "elixir",
        "erb" => "erb",
        "go" | "golang" => "go",
        "graphql" => "graphql",
        "html" => "html",
        "java" => "java",
        "javascript" | "js" | "js2" | "jsx" => "javascript",
        "json" | "jsonc" => "json",
        "kotlin" | "kt" => "kotlin",
        "lua" => "lua",
        "make" | "makefile" | "gnumakefile" => "make",
        "markdown" | "md" => "markdown",
        "php" => "php",
        "proto" | "protobuf" => "proto",
        "python" | "py" => "python",
        "ruby" | "rb" => "ruby",
        "rust" | "rs" => "rust",
        "scala" => "scala",
        "svelte" => "svelte",
        "swift" => "swift",
        "toml" => "toml",
        "tsx" => "tsx",
        "typescript" | "ts" => "typescript",
        "yaml" | "yml" => "yaml",
        "zig" => "zig",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{for_path, for_text, for_untitled};

    fn lang(name: &str, text: &str) -> &'static str {
        for_text(Some(Path::new(name)), text)
    }

    #[test]
    fn names_win_and_the_longest_suffix_wins() {
        assert_eq!(for_path(Some(Path::new("/a/main.go"))), "go");
        assert_eq!(for_path(Some(Path::new("/a/Makefile"))), "make");
        assert_eq!(for_path(Some(Path::new("/a/CMakeLists.txt"))), "cmake");
        assert_eq!(for_path(Some(Path::new("/a/notes.txt"))), "text");
        assert_eq!(for_path(Some(Path::new("/h/.bash_profile"))), "bash");
        assert_eq!(for_path(Some(Path::new("/a/tsconfig.json"))), "json");
        assert_eq!(for_path(Some(Path::new("/a/x.C"))), "cpp");
        assert_eq!(for_path(Some(Path::new("/a/x.c"))), "c");
        assert_eq!(for_path(Some(Path::new("/a/X.PY"))), "python");
        assert_eq!(for_path(None), "text");
    }

    #[test]
    fn path_beats_shebang_beats_modeline() {
        assert_eq!(lang("run", "#!/usr/bin/env python3\nprint(1)\n"), "python");
        assert_eq!(lang("run", "#!/bin/bash\n"), "bash");
        assert_eq!(lang("run", "#!/usr/bin/env node\n"), "javascript");
        assert_eq!(lang("run.rb", "#!/bin/bash\n"), "ruby");
        assert_eq!(lang("run", "# -*- mode: ruby -*-\nputs 1\n"), "ruby");
        assert_eq!(lang("run", "#!/bin/bash\n# vim: ft=lua\n"), "bash");
        assert_eq!(lang("run", "x\n-- vim: set ft=lua ts=4:\n"), "lua");
        assert_eq!(lang("run", "x\n\n\n\n\n\n\n\n\n\n\n\n\n/* vim: ft=lua */\n"), "lua");
        assert_eq!(lang("run", "x\n-- vim: ft=lua\n"), "lua");
        assert_eq!(lang("run", "nothing here\n"), "text");
        assert_eq!(lang("run", "#!/usr/bin/env perl\n"), "text");
        assert_eq!(for_text(None, "#!/bin/bash\n"), "text");
        assert_eq!(lang("-", "diff --git a/x b/x\n--- a/x\n"), "diff");
        assert_eq!(lang("-", "From 0123456789abcdef0123456789abcdef01234567 Mon Sep 17 00:00:00 2001\n"), "diff");
        assert_eq!(lang("-", "diffuse light\n"), "text");
    }

    #[test]
    fn an_untitled_page_reads_its_first_line() {
        assert_eq!(for_untitled("# 2026-11-02", false), "markdown");
        assert_eq!(for_untitled("###### Six", false), "markdown");
        assert_eq!(for_untitled("---", false), "markdown");
        assert_eq!(for_untitled("\u{feff}--- ", false), "markdown");
        // A shebang, a hashtag, seven `#`s and a rule with more dashes are not.
        for line in ["#!/bin/bash", "#hashtag", "####### seven", "----", "#", "", "notes"] {
            assert_eq!(for_untitled(line, false), "text", "{line:?}");
        }
        // A Markdown template keeps any first line Markdown, but a patch still reads as one.
        assert_eq!(for_untitled("notes", true), "markdown");
        assert_eq!(for_untitled("diff --git a/x b/x", true), "diff");
        assert_eq!(for_untitled("diff --git a/x b/x", false), "diff");
    }
}
