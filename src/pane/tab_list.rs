//! The tab list: the chevron at the strip's end (shown while the strip overflows) opens a native
//! menu under it with every tab of the pane in strip order. Picking one shows it through
//! `Pane::activate_id`, which scrolls it into view as any other way of showing a tab does.

use super::*;

impl Pane {
    /// Opens the tab list under its button once the frame has drawn (as `show_tab_menu` does).
    /// False while the button is hidden, since the strip fits.
    pub fn show_tab_list(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(button) = self.motion.list_button() else { return false };
        let at = (f32::from(button.origin.x), f32::from(button.origin.y + button.size.height));
        let names: Vec<String> = self
            .tabs
            .iter()
            .map(|tab| {
                let document = tab.document.read(cx);
                match document.path.as_deref().and_then(|path| path.file_name()) {
                    Some(name) => name.to_string_lossy().into_owned(),
                    None => document.draft_title().map_or_else(|| tab_bar::untitled_title(self.untitled.get(&tab.document.entity_id()).copied().unwrap_or(1)), str::to_owned),
                }
            })
            .collect();
        let twins = twin_flags(names.iter());
        let entries: Vec<Entry<EntityId>> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, tab)| {
                let id = tab.document.entity_id();
                let document = tab.document.read(cx);
                let mut title = crate::explorer::cut_middle(&names[ix], NAME_MAX);
                if twins[ix]
                    && let Some(folder) = document.path.as_deref().and_then(Path::parent).and_then(Path::file_name)
                {
                    title = format!("{title} ({})", folder.to_string_lossy());
                }
                if document.shows_dot() {
                    title.push_str(" \u{25CF}");
                }
                // What the strip's tip says of the tab, after the dot, in the tip's order.
                if self.is_pinned(ix) {
                    title.push_str(" \u{B7} Pinned");
                }
                if document.is_read_only() {
                    title.push_str(" \u{B7} Read-only");
                }
                if document.is_gone() {
                    title.push_str(" \u{B7} Deleted");
                } else if document.in_conflict() {
                    title.push_str(" \u{B7} Changed on disk");
                }
                let item = Item::new(title, id).checked(ix == self.active);
                Entry::Item(match document.path.clone() {
                    Some(path) => item.icon(path),
                    None => item.symbol("doc"),
                })
            })
            .collect();
        let this = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            // The button takes its held look in this frame, which draws before the menu blocks the thread.
            this.update(cx, |this, cx| {
                this.motion.hold_list(true);
                cx.notify();
            })
            .ok();
            window.on_next_frame(move |window, cx| {
                window
                    .spawn(cx, async move |cx| {
                        let chosen = native_menu::pop_up(&entries, Some(at));
                        this.update_in(cx, |this, window, cx| {
                            this.motion.hold_list(false);
                            cx.notify();
                            if let Some(id) = chosen {
                                this.activate_id(id, window, cx);
                            }
                        })
                        .ok();
                    })
                    .detach();
            });
        });
        true
    }
}

/// The longest a name runs in the list, in characters.
const NAME_MAX: usize = 40;

#[cfg(test)]
mod tests {
    use super::NAME_MAX;
    use crate::explorer::{MENU_NAME_MAX, cut_middle};

    #[test]
    fn short_names_stay() {
        assert_eq!(cut_middle("README.md", NAME_MAX), "README.md");
    }

    #[test]
    fn long_names_lose_their_middle_and_keep_the_extension() {
        let name = format!("{}.markdown", "a".repeat(60));
        let cut = cut_middle(&name, NAME_MAX);
        assert_eq!(cut.chars().count(), NAME_MAX);
        assert!(cut.ends_with("aaaaaa.markdown") && cut.contains('\u{2026}'));
    }

    #[test]
    fn long_names_without_an_extension_cut_too() {
        let cut = cut_middle(&"b".repeat(50), NAME_MAX);
        assert_eq!(cut.chars().count(), NAME_MAX);
    }

    /// The tab list and the tree's menus cut names with the one `cut_middle`, so a name too long for
    /// both comes out the same shape: its start, the "…", its last six letters and the extension.
    #[test]
    fn the_tab_list_and_the_tree_cut_a_name_the_same_way() {
        let mid = format!("{}.markdown", "a".repeat(28));
        // Short enough for the list, too long for a menu: only the menu cuts it.
        assert_eq!(cut_middle(&mid, NAME_MAX), mid);
        assert_eq!(cut_middle(&mid, MENU_NAME_MAX), format!("{}\u{2026}aaaaaa.markdown", "a".repeat(MENU_NAME_MAX - 1 - 15)));
        let long = format!("{}.markdown", "b".repeat(80));
        for max in [MENU_NAME_MAX, NAME_MAX] {
            assert_eq!(cut_middle(&long, max), format!("{}\u{2026}bbbbbb.markdown", "b".repeat(max - 1 - 15)));
        }
    }
}
