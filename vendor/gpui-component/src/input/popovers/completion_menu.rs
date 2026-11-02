use std::{ops::Range, rc::Rc};

use gpui::{
    Action, AnyElement, App, AppContext, AvailableSpace, Bounds, Context, ContentMask,
    DismissEvent, Element, ElementId, Empty, Entity, EventEmitter, GlobalElementId, HighlightStyle,
    InspectorElementId, InteractiveElement as _, IntoElement, LayoutId, ParentElement, Pixels,
    Render, RenderOnce, SharedString, Style, Styled, StyledText, Subscription, TextAlign,
    TextStyle, WeakEntity, Window, deferred, div, prelude::FluentBuilder, px, relative, rems, size,
};
use gpui_base::TestSupportExt as _;
use gpui_base::input::CompletionHighlighter;
use lsp_types::{CompletionItem, CompletionItemKind};

const MAX_MENU_HEIGHT: Pixels = px(240.);
const POPOVER_GAP: Pixels = px(4.);
/// A `FILE` item's row, in px: two lines, the name over its detail.
const STACKED_ROW_HEIGHT: f32 = 44.;

/// Drop ranges a custom highlighter cannot safely paint, then order the rest.
///
/// A range past the label, or one that begins or ends inside a character,
/// would split a glyph. Empty ranges paint nothing, so they go too. What
/// remains is sorted and merged: GPUI builds text runs assuming highlights
/// are ordered and do not overlap, and an overlap makes the run longer than
/// the label.
fn retained_highlight_ranges(
    label: &str,
    ranges: impl IntoIterator<Item = Range<usize>>,
) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = ranges
        .into_iter()
        .filter(|range| {
            range.start < range.end
                && range.end <= label.len()
                && label.is_char_boundary(range.start)
                && label.is_char_boundary(range.end)
        })
        .collect();
    ranges.sort_by_key(|range| range.start);

    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        if let Some(last) = merged.last_mut() {
            if range.start <= last.end {
                last.end = last.end.max(range.end);
                continue;
            }
        }
        merged.push(range);
    }
    merged
}

/// The characters a cut file name keeps of its stem before the extension, so the ellipsis never sits against the dot.
const TAIL_KEPT: usize = 2;

/// `text` cut until `fits` says it does: whole if it fits, else the stem's start, an ellipsis, the
/// stem's last `TAIL_KEPT` characters and the extension ("planning-no…v2.md"). A name with no
/// extension is cut at its end. `ranges` (match highlights, byte ranges of `text`) come back at their
/// places in the cut text; the part of a range that was cut away is dropped.
fn cut_file_name(
    text: &str,
    ranges: &[Range<usize>],
    fits: &dyn Fn(&str) -> bool,
) -> (String, Vec<Range<usize>>) {
    if fits(text) {
        return (text.to_string(), ranges.to_vec());
    }
    let dot = text.rfind('.').filter(|&dot| dot > 0).unwrap_or(text.len());
    let (stem, extension) = text.split_at(dot);
    let stem_chars = stem.chars().count();
    let keep = if dot < text.len() { TAIL_KEPT.min(stem_chars / 2) } else { 0 };
    let tail_from = stem_chars - keep;
    let byte = |ix: usize| stem.char_indices().nth(ix).map_or(stem.len(), |(byte, _)| byte);
    let tail_byte = byte(tail_from);
    let candidate = |end: usize| format!("{}\u{2026}{}{extension}", &stem[..byte(end)], &stem[tail_byte..]);
    // The most head characters that fit; none still draws the ellipsis, the tail and the extension.
    let (mut low, mut high) = (0, tail_from);
    while low < high {
        let mid = (low + high).div_ceil(2);
        if fits(&candidate(mid)) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    let head = byte(low);
    let shift = head + '\u{2026}'.len_utf8();
    let mut moved = Vec::new();
    for range in ranges {
        let kept = range.start..range.end.min(head);
        if kept.start < kept.end {
            moved.push(kept);
        }
        let kept = range.start.max(tail_byte)..range.end;
        if kept.start < kept.end {
            moved.push(kept.start - tail_byte + shift..kept.end - tail_byte + shift);
        }
    }
    (candidate(low), moved)
}

/// A file row's name, drawn as one run and cut to the width layout gives it with
/// [`cut_file_name`], so a long note's name keeps its extension. Its highlights are painted on
/// the cut text.
struct FileLabel {
    text: SharedString,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
}

/// What a file name needs to be cut and drawn, read from the text style where it sits.
struct FileLabelSpec {
    text: SharedString,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    style: TextStyle,
    font_size: Pixels,
    line_height: Pixels,
}

impl FileLabelSpec {
    fn shape(&self, text: SharedString, highlights: &[(Range<usize>, HighlightStyle)], window: &Window) -> gpui::ShapedLine {
        let mut runs = Vec::new();
        let mut at = 0;
        for (range, style) in highlights {
            if range.start > at {
                runs.push(self.style.to_run(range.start - at));
            }
            let mut run = self.style.to_run(range.end - range.start);
            if let Some(color) = style.color {
                run.color = color;
            }
            runs.push(run);
            at = range.end;
        }
        if at < text.len() {
            runs.push(self.style.to_run(text.len() - at));
        }
        window.text_system().shape_line(text, self.font_size, &runs, None)
    }

    fn width(&self, text: &str, window: &Window) -> Pixels {
        self.shape(SharedString::from(text.to_string()), &[], window).width
    }

    fn cut(&self, width: Pixels, window: &Window) -> (SharedString, Vec<(Range<usize>, HighlightStyle)>) {
        let ranges: Vec<Range<usize>> = self.highlights.iter().map(|(range, _)| range.clone()).collect();
        let (text, moved) = cut_file_name(&self.text, &ranges, &|text| self.width(text, window) <= width);
        // Highlights are merged and ordered with one style, so the pieces of a range share it.
        let style = self.highlights.first().map(|(_, style)| *style).unwrap_or_default();
        (SharedString::from(text), moved.into_iter().map(|range| (range, style)).collect())
    }
}

impl Element for FileLabel {
    type RequestLayoutState = std::rc::Rc<FileLabelSpec>;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = style.line_height_in_pixels(window.rem_size());
        let spec = std::rc::Rc::new(FileLabelSpec {
            text: self.text.clone(),
            highlights: std::mem::take(&mut self.highlights),
            style,
            font_size,
            line_height,
        });
        let measured = spec.clone();
        let mut layout_style = Style::default();
        layout_style.size.width = relative(1.).into();
        layout_style.min_size.width = px(0.).into();
        let layout = window.request_measured_layout(layout_style, move |known, available, window, _| {
            let width = match (known.width, available.width) {
                (Some(width), _) => width,
                (None, AvailableSpace::Definite(width)) => measured.width(&measured.text, window).min(width),
                (None, AvailableSpace::MinContent) => px(0.),
                (None, AvailableSpace::MaxContent) => measured.width(&measured.text, window),
            };
            size(width, measured.line_height)
        });
        (layout, spec)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        spec: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (text, highlights) = spec.cut(bounds.size.width, window);
        let line = spec.shape(text, &highlights, window);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            line.paint(bounds.origin, spec.line_height, TextAlign::Left, None, window, cx).ok();
        });
    }
}

impl IntoElement for FileLabel {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

use crate::{
    ActiveTheme, IndexPath, Selectable, ThemeStyled as _, actions, h_flex, v_flex,
    input::{
        self, EditorState,
        popovers::{CaretAnchoredPopup, editor_popover, render_markdown},
    },
    label::Label,
    list::{List, ListDelegate, ListEvent, ListState},
    menu::MenuItemElement,
};

struct ContextMenuDelegate {
    query: SharedString,
    menu: Entity<CompletionMenu>,
    items: Vec<Rc<CompletionItem>>,
    selected_ix: usize,
    highlight: Rc<CompletionHighlighter>,
}

impl ContextMenuDelegate {
    fn set_items(&mut self, items: Vec<CompletionItem>) {
        self.items = items.into_iter().map(Rc::new).collect();
        self.selected_ix = 0;
    }

    fn selected_item(&self) -> Option<&Rc<CompletionItem>> {
        self.items.get(self.selected_ix)
    }
}

#[derive(IntoElement)]
struct CompletionMenuItem {
    ix: usize,
    item: Rc<CompletionItem>,
    children: Vec<AnyElement>,
    selected: bool,
    query: SharedString,
    highlight: Rc<CompletionHighlighter>,
}

impl CompletionMenuItem {
    fn new(
        ix: usize,
        item: Rc<CompletionItem>,
        query: SharedString,
        highlight: Rc<CompletionHighlighter>,
    ) -> Self {
        Self {
            ix,
            item,
            children: vec![],
            selected: false,
            query,
            highlight,
        }
    }
}
impl Selectable for CompletionMenuItem {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl ParentElement for CompletionMenuItem {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}
impl RenderOnce for CompletionMenuItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let item = self.item;

        let deprecated = item.deprecated.unwrap_or(false);
        let stacked = item.kind == Some(CompletionItemKind::FILE);
        let highlights = retained_highlight_ranges(
            &item.label,
            (self.highlight)(self.query.as_ref(), item.as_ref()),
        )
        .into_iter()
        .map(|range| {
            (
                range,
                HighlightStyle {
                    color: Some(cx.theme().blue),
                    ..Default::default()
                },
            )
        })
        .collect::<Vec<_>>();

        MenuItemElement::new(self.ix)
            .aria_label(item.label.clone())
            .selected(self.selected)
            .text_sm()
            .py_0()
            .px_2()
            .h(rems(if stacked { STACKED_ROW_HEIGHT } else { 26. } / 16.))
            .mb_0p5()
            .rounded(cx.theme().radius)
            .when(item.deprecated.unwrap_or(false), |this| this.line_through())
            .child(if stacked {
                // A file's row: its name, cut by width keeping the extension, and under it the
                // detail (a note's first heading).
                v_flex()
                    .w_full()
                    .justify_center()
                    .child(div().w_full().min_w_0().child(FileLabel {
                        text: item.label.clone().into(),
                        highlights,
                    }))
                    .when_some(item.detail.as_ref(), |this, detail| {
                        this.child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(detail.clone()),
                        )
                    })
                    .into_any_element()
            } else {
                h_flex()
                    .w_full()
                    .gap_3()
                    .justify_between()
                    .child(StyledText::new(item.label.clone()).with_highlights(highlights))
                    .when_some(item.detail.as_ref(), |this, detail| {
                        this.child(
                            Label::new(detail.clone())
                                .text_color(cx.theme().muted_foreground)
                                .when(deprecated, |this| this.line_through())
                                .italic(),
                        )
                    })
                    .into_any_element()
            })
            .children(self.children)
    }
}

impl EventEmitter<DismissEvent> for ContextMenuDelegate {}

impl ListDelegate for ContextMenuDelegate {
    type Item = CompletionMenuItem;

    fn items_count(&self, _: usize, _: &gpui::App) -> usize {
        self.items.len()
    }

    fn render_item(
        &mut self,
        ix: crate::IndexPath,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let item = self.items.get(ix.row)?;
        Some(CompletionMenuItem::new(
            ix.row,
            item.clone(),
            self.query.clone(),
            self.highlight.clone(),
        ))
    }

    fn set_selected_index(
        &mut self,
        ix: Option<crate::IndexPath>,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected_ix = ix.map(|i| i.row).unwrap_or(0);
        cx.notify();
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        let Some(item) = self.selected_item() else {
            return;
        };

        self.menu.update(cx, |this, cx| {
            this.select_item(&item, window, cx);
        });
    }
}

/// A context menu for code completions and code actions.
pub struct CompletionMenu {
    offset: usize,
    editor: WeakEntity<EditorState>,
    list: Entity<ListState<ContextMenuDelegate>>,
    open: bool,

    /// The offset of the first character that triggered the completion.
    pub(crate) trigger_start_offset: Option<usize>,
    query: SharedString,
    _subscriptions: Vec<Subscription>,
}

impl CompletionMenu {
    /// Creates a new `CompletionMenu` with the given offset and completion items.
    ///
    /// NOTE: This element should not call from EditorState::new, unless that will stack overflow.
    pub(crate) fn new(
        editor: Entity<EditorState>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let view = cx.entity();
            let menu = ContextMenuDelegate {
                query: SharedString::default(),
                menu: view,
                items: vec![],
                selected_ix: 0,
                highlight: Rc::new(gpui_base::input::highlight_prefix),
            };

            let list = cx.new(|cx| ListState::new(menu, window, cx));

            let _subscriptions =
                vec![
                    cx.subscribe(&list, |this: &mut Self, _, ev: &ListEvent, cx| {
                        match ev {
                            ListEvent::Confirm(_) => {
                                this.dismiss(cx);
                            }
                            _ => {}
                        }
                        cx.notify();
                    }),
                ];

            Self {
                offset: 0,
                editor: editor.downgrade(),
                list,
                open: false,
                trigger_start_offset: None,
                query: SharedString::default(),
                _subscriptions,
            }
        })
    }

    fn select_item(&mut self, item: &CompletionItem, window: &mut Window, cx: &mut Context<Self>) {
        let item = item.clone();
        let range = self.trigger_start_offset.unwrap_or(self.offset)..self.offset;

        let editor = self.editor.clone();

        cx.spawn_in(window, async move |_, cx| {
            editor.update_in(cx, |editor, window, cx| {
                editor.insert_completion(&item, range, window, cx);
            })
        })
        .detach();

        self.dismiss(cx);
    }

    pub(crate) fn handle_action(
        &mut self,
        action: Box<dyn Action>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.open {
            return false;
        }

        if input::Enter::is_primary(&*action) {
            self.on_action_enter(window, cx);
        } else if action.partial_eq(&input::Escape) {
            self.on_action_escape(window, cx);
        } else if action.partial_eq(&input::MoveUp) {
            self.on_action_up(window, cx);
        } else if action.partial_eq(&input::MoveDown) {
            self.on_action_down(window, cx);
        } else {
            cx.propagate();
            return false;
        }

        true
    }

    fn on_action_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.list.read(cx).delegate().selected_item().cloned() else {
            return;
        };
        self.select_item(&item, window, cx);
    }

    fn on_action_escape(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.dismiss(cx);
    }

    fn on_action_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.list.update(cx, |this, cx| {
            this.on_action_select_prev(&actions::SelectUp, window, cx)
        });
    }

    fn on_action_down(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.list.update(cx, |this, cx| {
            this.on_action_select_next(&actions::SelectDown, window, cx)
        });
    }

    /// Synchronize a hidden editor menu without dismissing a newer response.
    pub(crate) fn hide(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.trigger_start_offset = None;
        cx.notify();
    }

    /// Dismiss the menu in response to a user action.
    fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.hide(cx);
        let editor = self.editor.clone();
        cx.spawn(async move |_, cx| {
            let _ = editor.update(cx, |editor, cx| editor.dismiss_completion_overlay(cx));
        })
        .detach();
    }

    /// Sets the trigger start offset if it is not already set.
    pub(crate) fn update_query(&mut self, start_offset: usize, query: impl Into<SharedString>) {
        if self.trigger_start_offset.is_none() {
            self.trigger_start_offset = Some(start_offset);
        }
        self.query = query.into();
    }

    pub(crate) fn show(
        &mut self,
        offset: usize,
        items: impl Into<Vec<CompletionItem>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let items = items.into();
        self.offset = offset;
        self.open = true;
        self.list.update(cx, |this, cx| {
            let longest_ix = items
                .iter()
                .enumerate()
                .max_by_key(|(_, item)| {
                    item.label.len() + item.detail.as_ref().map(|d| d.len()).unwrap_or(0)
                })
                .map(|(ix, _)| ix)
                .unwrap_or(0);

            this.delegate_mut().query = self.query.clone();
            this.delegate_mut().set_items(items);
            this.set_selected_index(Some(IndexPath::new(0)), window, cx);
            this.set_item_to_measure_index(IndexPath::new(longest_ix), window, cx);
        });

        cx.notify();
    }

    /// Replace the label highlighter and repaint an open menu.
    pub(crate) fn set_highlight(
        &mut self,
        highlight: Rc<CompletionHighlighter>,
        cx: &mut Context<Self>,
    ) {
        self.list.update(cx, |list, cx| {
            if Rc::ptr_eq(&list.delegate().highlight, &highlight) {
                return;
            }
            list.delegate_mut().highlight = highlight;
            cx.notify();
        });
    }
}

impl Render for CompletionMenu {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return Empty.into_any_element();
        }

        if self.list.read(cx).delegate().items.is_empty() {
            self.open = false;
            return Empty.into_any_element();
        }

        let Some(editor) = self.editor.upgrade() else {
            return Empty.into_any_element();
        };
        // The anchor is resolved in the popup's deferred prepaint from the
        // input's current-frame geometry; the content below is built there.
        let configured_max = editor.read(cx).lsp().completion_menu.max_width;
        let list = self.list.clone();
        let menu = cx.weak_entity();

        deferred(CaretAnchoredPopup::new(
            self.editor.clone(),
            move |anchor, window, cx| {
                // `anchor` is in window coordinates, from the current frame.
                let max_width = configured_max.min(window.bounds().size.width - anchor.x);
                let vertical_layout =
                    anchor.x + configured_max + POPOVER_GAP + configured_max + POPOVER_GAP
                        > window.bounds().size.width;

                let selected_documentation = list
                    .read(cx)
                    .delegate()
                    .selected_item()
                    .and_then(|item| item.documentation.clone());

                div()
                    .flex()
                    .flex_row()
                    .gap(POPOVER_GAP)
                    .items_start()
                    .when(vertical_layout, |this| this.flex_col())
                    .child(
                        editor_popover("completion-menu", cx)
                            .popover_style(cx)
                            .text_sm()
                            // Each virtual row includes the menu's inter-item gap.
                            .pb_0p5()
                            .test_support()
                            .max_w(max_width)
                            .min_w(px(120.))
                            .child(List::new(&list).max_h(MAX_MENU_HEIGHT).thin_scrollbar(true)),
                    )
                    .when_some(selected_documentation, |this, documentation| {
                        let mut doc = match documentation {
                            lsp_types::Documentation::String(s) => s.clone(),
                            lsp_types::Documentation::MarkupContent(mc) => mc.value.clone(),
                        };
                        if vertical_layout {
                            doc = doc.split("\n").next().unwrap_or_default().to_string();
                        }

                        this.child(
                            div().child(
                                editor_popover("completion-menu", cx)
                                    .popover_style(cx)
                                    .text_sm()
                                    .w(configured_max)
                                    .px_2()
                                    .child(render_markdown("doc", doc, window, cx)),
                            ),
                        )
                    })
                    .on_mouse_down_out({
                        let menu = menu.clone();
                        move |_, _, cx: &mut App| {
                            if let Some(menu) = menu.upgrade() {
                                menu.update(cx, |menu, cx| menu.dismiss(cx));
                            }
                        }
                    })
                    .into_any_element()
            },
        ))
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_file_name_keeps_its_extension_and_its_highlights_move_with_it() {
        let name = "planning-notes-on-the-long-migration-v2.md";
        let width = |limit: usize| move |text: &str| text.chars().count() <= limit;
        assert_eq!(cut_file_name(name, &[0..2], &width(60)), (name.to_string(), vec![0..2]));
        let (cut, moved) = cut_file_name(name, &[0..2], &width(16));
        assert_eq!(cut, "planning-n\u{2026}v2.md");
        assert_eq!(moved, vec![0..2]);
        // A match in the kept tail lands at its new place; one in the cut middle is dropped.
        let (cut, moved) = cut_file_name(name, &[20..24, 37..39], &width(16));
        assert_eq!(moved.len(), 1);
        assert_eq!(&cut[moved[0].clone()], "v2");
        // No extension: cut at the end.
        assert_eq!(cut_file_name("abcdefghij", &[], &width(5)).0, "abcd\u{2026}");
    }

    #[test]
    fn highlight_ranges_are_ordered_and_do_not_overlap() {
        assert_eq!(
            retained_highlight_ranges("flag", [2..3, 0..1]),
            vec![0..1, 2..3]
        );
        assert_eq!(retained_highlight_ranges("flag", [0..3, 2..4]), vec![0..4]);
        assert_eq!(
            retained_highlight_ranges("flag", [0..1, 1..2, 5..9, 2..3]),
            vec![0..3]
        );
        assert_eq!(retained_highlight_ranges("你", [0..1, 0..3]), vec![0..3]);
    }

    #[gpui::test]
    fn hiding_an_old_menu_does_not_dismiss_a_new_completion_response(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::init);
        let window = cx.add_empty_window();
        let (editor, menu) = window.update(|window, cx| {
            let editor = cx.new(|cx| EditorState::new(window, cx).default_value("co"));
            let menu = CompletionMenu::new(editor.clone(), window, cx);
            let old_items = vec![CompletionItem {
                label: "const".into(),
                ..Default::default()
            }];
            editor.update(cx, |editor, cx| {
                editor.present_completion_items(0, "c", old_items.clone(), cx);
            });
            menu.update(cx, |menu, cx| {
                menu.update_query(0, "c");
                menu.show(1, old_items, window, cx);
                // The overlay observes the old request being hidden while a
                // replacement request is pending. Its deferred dismissal must
                // not act on the replacement response that arrives next.
                menu.hide(cx);
            });
            editor.update(cx, |editor, cx| {
                editor.present_completion_items(
                    0,
                    "co",
                    vec![CompletionItem {
                        label: "const".into(),
                        ..Default::default()
                    }],
                    cx,
                );
            });
            (editor, menu)
        });

        cx.run_until_parked();

        cx.read(|cx| {
            let completion = editor.read(cx).completion_menu_state();
            assert!(completion.open, "an old menu dismissed the new response");
            assert_eq!(completion.query, "co");
            assert_eq!(completion.items.len(), 1);
            assert_eq!(completion.items[0].label, "const");
        });
        drop(menu);
    }
}
