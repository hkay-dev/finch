//! File > New from template: a few built-in starting texts, then the files in the `templates`
//! folder next to the settings file. Picking one makes a new Untitled page that starts with its
//! text (`Workspace::new_page_with`), with `{{date}}` filled in and `{{title}}` and `{{cursor}}`
//! made tab stops (`tab_stops.rs`). The folder is listed off the main thread at start and when a
//! window becomes key, as Open Recent is, never per menu refresh; a file is read when it's picked.

use std::{
    fs,
    path::{Path, PathBuf},
};

use gpui_kit::*;

use crate::{menu, settings};

/// The built-in templates, in menu order: their menu title, their text and whether it is Markdown.
pub const BUILT_IN: [(&str, &str, bool); 4] = [
    ("Markdown note", "---\ntitle: \ntags: []\n---\n\n", true),
    ("Daily note", "# {{date}}\n\n{{cursor}}", true),
    ("README", "# Project\n\nWhat it does, in a sentence or two.\n\n## Install\n\n## Use\n", true),
    ("Shell script", "#!/usr/bin/env bash\nset -euo pipefail\n\n", false),
];

/// The most files of the folder the menu lists.
const LISTED: usize = 50;

/// The largest template file that is read: a starting text, not a document to open.
const MOST_BYTES: u64 = 1024 * 1024;

/// One template: a built-in one by its place in `BUILT_IN`, or a file in the folder.
#[derive(Clone, PartialEq, Debug)]
pub enum Template {
    BuiltIn(usize),
    File(PathBuf),
}

/// File > New from template's item for one template: a new Untitled page with its text.
#[derive(Action, Clone, PartialEq)]
#[action(namespace = gpui_editor, no_json)]
pub struct NewFromTemplate {
    pub template: Template,
}

actions!(gpui_editor, [OpenTemplatesFolder]);

/// The folder's files as last listed, and the listing in flight.
struct Listed {
    files: Vec<PathBuf>,
    check: Option<Task<()>>,
}

impl Global for Listed {}

/// The folder for his own templates: `templates` beside the settings file.
pub fn folder() -> PathBuf {
    let settings = settings::Store::default_path();
    settings.parent().unwrap_or(Path::new("/")).join("templates")
}

/// Sets up the listing and File > New from template > Open templates folder.
pub fn install(cx: &mut App) {
    cx.set_global(Listed { files: Vec::new(), check: None });
    cx.on_action(|_: &OpenTemplatesFolder, cx| open_folder(cx));
    reload(cx);
}

/// Lists the folder again off the main thread; the menu changes when the list did.
pub fn reload(cx: &mut App) {
    let check = cx.spawn(async move |cx| {
        let files = cx.background_executor().spawn(async move { list(&folder()) }).await;
        cx.update(|cx| {
            let listed = cx.global_mut::<Listed>();
            listed.check = None;
            if listed.files != files {
                listed.files = files;
                menu::refresh(cx);
            }
        });
    });
    cx.global_mut::<Listed>().check = Some(check);
}

/// The folder's files as the menu lists them.
pub fn files(cx: &App) -> &[PathBuf] {
    cx.try_global::<Listed>().map_or(&[], |listed| &listed.files)
}

/// A template file's name with its extension.
fn file_title(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// The menu titles of template files, in the order given: each file's name without its extension,
/// except for a file whose title would match a built-in template's, another file's title or another
/// file's name (`a.md` and `a.txt`; `README.md`), which keeps its extension.
pub fn file_titles(paths: &[PathBuf]) -> Vec<String> {
    let stem = |path: &Path| path.file_stem().map_or_else(|| file_title(path), |stem| stem.to_string_lossy().into_owned());
    let stems: Vec<String> = paths.iter().map(|path| stem(path)).collect();
    let names: Vec<String> = paths.iter().map(|path| file_title(path)).collect();
    stems
        .iter()
        .enumerate()
        .map(|(i, title)| {
            let shared = BUILT_IN.iter().any(|(built_in, ..)| built_in.eq_ignore_ascii_case(title))
                || (0..paths.len()).any(|j| j != i && (stems[j].eq_ignore_ascii_case(title) || names[j].eq_ignore_ascii_case(title)));
            if shared { names[i].clone() } else { title.clone() }
        })
        .collect()
}

/// The plain files in `dir` (a link counts by what it points to), hidden ones left out, by name
/// in any case, at most `LISTED`. A folder that isn't there has none.
fn list(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    files.sort_by_cached_key(|path| file_title(path).to_lowercase());
    files.truncate(LISTED);
    files
}

/// The template named `name` (a built-in title, or a file's menu title or full name in the folder,
/// in any case), for the control socket.
pub fn find(name: &str) -> Option<Template> {
    if let Some(ix) = BUILT_IN.iter().position(|(title, ..)| title.eq_ignore_ascii_case(name)) {
        return Some(Template::BuiltIn(ix));
    }
    let files = list(&folder());
    let titles = file_titles(&files);
    let ix = files.iter().position(|path| file_title(path).eq_ignore_ascii_case(name)).or_else(|| titles.iter().position(|title| title.eq_ignore_ascii_case(name)))?;
    files.into_iter().nth(ix).map(Template::File)
}

/// Every template's title, built-in ones first, for the control socket's refusal.
pub fn titles() -> Vec<String> {
    BUILT_IN.iter().map(|(title, ..)| (*title).to_owned()).chain(file_titles(&list(&folder()))).collect()
}

/// The template's text, or why it can't be read.
pub fn text(template: &Template) -> Result<String, String> {
    match template {
        Template::BuiltIn(ix) => BUILT_IN.get(*ix).map(|(_, text, _)| (*text).to_owned()).ok_or_else(|| "no such template".into()),
        Template::File(path) => read(path),
    }
}

/// Whether a page made from the template is Markdown: a built-in one that says so, or a file with
/// a Markdown name (`.md` and the rest).
pub fn is_markdown(template: &Template) -> bool {
    match template {
        Template::BuiltIn(ix) => BUILT_IN.get(*ix).is_some_and(|(.., markdown)| *markdown),
        Template::File(path) => crate::language::for_path(Some(path)) == "markdown",
    }
}

fn read(path: &Path) -> Result<String, String> {
    let name = file_title(path);
    let size = fs::metadata(path).map_err(|err| crate::file_ops::explain("open", path, &err))?.len();
    if size > MOST_BYTES {
        return Err(format!("\u{201C}{name}\u{201D} is over 1 MB, too large for a template."));
    }
    let bytes = fs::read(path).map_err(|err| crate::file_ops::explain("open", path, &err))?;
    crate::encoding::decode(bytes).map(|decoded| decoded.text).map_err(|_| format!("\u{201C}{name}\u{201D} isn’t text."))
}

/// Makes the folder when it isn't there yet and shows it in Finder.
fn open_folder(cx: &mut App) {
    let dir = folder();
    if let Err(err) = fs::create_dir_all(&dir) {
        eprintln!("templates: {}", crate::file_ops::explain("create", &dir, &err));
        return;
    }
    cx.open_with_system(&dir);
    reload(cx);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::file_titles;

    #[test]
    fn titles_drop_the_extension_unless_two_files_would_share_one() {
        let files: Vec<PathBuf> = ["a.md", "a.txt", "b.md", "Notes"].iter().map(PathBuf::from).collect();
        assert_eq!(file_titles(&files), ["a.md", "a.txt", "b", "Notes"]);
    }

    #[test]
    fn a_title_that_matches_a_built_in_or_another_files_name_keeps_its_extension() {
        let readme: Vec<PathBuf> = vec![PathBuf::from("README.md")];
        assert_eq!(file_titles(&readme), ["README.md"]);
        let nested: Vec<PathBuf> = ["a.md", "a.md.txt"].iter().map(PathBuf::from).collect();
        assert_eq!(file_titles(&nested), ["a", "a.md.txt"]);
    }
}
