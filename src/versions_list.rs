//! The version picker: a float listing the saved versions of one file (`versions::list`, read on
//! the background executor), newest first, each by its friendly time ("Today 14:32", "Yesterday
//! 09:10", "Mon 3 Nov 09:10") and size; a row whose neighbour was saved in the same minute
//! adds its seconds ("Today 14:32:05") so the two read apart. Up, Down, Home, End, Page Up and Page
//! Down move the pick (and keep it in view), Return or a click chooses it
//! (`VersionsListEvent::Pick` with the version's id) and Escape closes (`VersionsListEvent::Close`).
//! The owner draws it, then opens the chosen version in the Compare view.

use std::{path::PathBuf, rc::Rc, time::SystemTime};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{
    scrollbar::{self, Overlay, Scroller},
    drafts::{Local, local},
    theme, tooltip,
    tokens::{
        ALPHA_PRIMARY, ALPHA_SECONDARY, ALPHA_SELECTED, ALPHA_SELECTED_LIGHT, FILTER_PAD_X, FIND_PAD, OPEN_QUICKLY_ROWS, RADIUS_FLOAT_CAPSULE, RADIUS_ROW, ROW_GAP, ROW_HEIGHT, TEXT_BODY, TEXT_BODY_LINE,
    },
    versions::{self, Version},
};

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// What the picker tells its owner.
pub enum VersionsListEvent {
    /// Return or a click on a version: its id, for `versions::read`, and its time for a title.
    Pick { id: String, time: SystemTime },
    /// Escape: leave without choosing.
    Close,
}

impl EventEmitter<VersionsListEvent> for VersionsList {}

pub struct VersionsList {
    /// The versions, newest first; `None` while they are being read.
    versions: Option<Vec<Version>>,
    selected: usize,
    scroll: UniformListScrollHandle,
    /// The scrollbar's hand-scroll clock.
    scroller: Scroller,
    focus: FocusHandle,
    _read: Task<()>,
}

impl VersionsList {
    /// The picker for the versions of the file at `path`, with the keyboard in it. The versions are
    /// read in the background; the list says so until they arrive.
    pub fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let read = cx.spawn(async move |this, cx| {
            let versions = cx.background_executor().spawn(async move { versions::list(&path) }).await;
            this.update(cx, |this, cx| {
                this.versions = Some(versions);
                cx.notify();
            })
            .ok();
        });
        Self { versions: None, selected: 0, scroll: UniformListScrollHandle::new(), scroller: Scroller::default(), focus, _read: read }
    }

    pub fn step(&mut self, down: bool, cx: &mut Context<Self>) {
        self.move_by(if down { 1 } else { -1 }, cx);
    }

    /// Moves the pick `by` rows (clamped to the list) and keeps it in view.
    fn move_by(&mut self, by: isize, cx: &mut Context<Self>) {
        let last = self.versions.as_ref().map_or(0, |versions| versions.len().saturating_sub(1));
        self.move_to(self.selected.saturating_add_signed(by).min(last), cx);
    }

    fn move_to(&mut self, to: usize, cx: &mut Context<Self>) {
        if to != self.selected {
            self.selected = to;
            self.scroll.scroll_to_item(to, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    /// Home and End: the newest and the oldest version.
    pub fn jump(&mut self, end: bool, cx: &mut Context<Self>) {
        let last = self.versions.as_ref().map_or(0, |versions| versions.len().saturating_sub(1));
        self.move_to(if end { last } else { 0 }, cx);
    }

    /// Page Up and Page Down: a screenful of rows (what the list shows at most) up or down.
    pub fn page(&mut self, down: bool, cx: &mut Context<Self>) {
        let rows = self.versions.as_ref().map_or(1, |versions| versions.len().clamp(1, OPEN_QUICKLY_ROWS)) as isize;
        self.move_by(if down { rows } else { -rows }, cx);
    }

    /// The pick's row and how many versions there are, for the control socket.
    pub fn selected(&self) -> (usize, usize) {
        (self.selected, self.versions.as_ref().map_or(0, Vec::len))
    }

    /// Return: chooses the pick; with no versions it beeps and stays.
    pub fn commit(&mut self, cx: &mut Context<Self>) {
        match self.versions.as_ref().and_then(|versions| versions.get(self.selected)) {
            Some(version) => cx.emit(VersionsListEvent::Pick { id: version.id.clone(), time: version.time }),
            None => crate::preview::render::beep(),
        }
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(VersionsListEvent::Close);
    }
}

impl Focusable for VersionsList {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Seconds since the Unix epoch.
fn seconds(time: SystemTime) -> i64 {
    time.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64)
}

/// 0 for Sunday to 6 for Saturday, of a calendar day (days since the epoch, then the epoch's Thursday).
fn weekday(year: i32, month: i32, day: i32) -> usize {
    let year = i64::from(if month <= 2 { year - 1 } else { year });
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted = i64::from((month + 9) % 12);
    let day_of_year = (153 * shifted + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    (days + 4).rem_euclid(7) as usize
}

/// A version's time for its row: `Today 14:32`, `Yesterday 09:10`, else `Mon 3 Nov 09:10`, with
/// the year when it isn't this year's, and `:05` after the clock when `second` is given.
pub fn time_label(then: Local, today: Local, yesterday: Local, second: Option<i64>) -> String {
    let day = |local: Local| (local.year, local.month, local.day);
    let clock = match second {
        Some(second) => format!("{:02}:{:02}:{:02}", then.hour, then.minute, second),
        None => format!("{:02}:{:02}", then.hour, then.minute),
    };
    if day(then) == day(today) {
        format!("Today {clock}")
    } else if day(then) == day(yesterday) {
        format!("Yesterday {clock}")
    } else {
        let name = WEEKDAYS[weekday(then.year, then.month, then.day)];
        let month = MONTHS[(then.month as usize).clamp(1, 12) - 1];
        if then.year == today.year { format!("{name} {} {month} {clock}", then.day) } else { format!("{name} {} {month} {} {clock}", then.day, then.year) }
    }
}

/// `then` as a row's time, against the clock now.
pub fn friendly(then: SystemTime, now: SystemTime) -> String {
    row_time(then, now, false)
}

/// `then` as a row's time, with its seconds when `with_seconds` (a neighbour shares its minute).
fn row_time(then: SystemTime, now: SystemTime, with_seconds: bool) -> String {
    let (then_secs, now) = (seconds(then), seconds(now));
    time_label(local(then_secs), local(now), local(now - 86_400), with_seconds.then(|| then_secs.rem_euclid(60)))
}

/// `812 B`, `14 KB`, `1.5 MB`.
fn size_label(len: u64) -> String {
    match len {
        0..1024 => format!("{len} B"),
        1024..1_048_576 => format!("{} KB", len.div_ceil(1024)),
        _ => format!("{:.1} MB", len as f64 / 1_048_576.),
    }
}

impl Render for VersionsList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let ink = theme.sidebar_foreground;
        let light = theme::is_light(cx);
        let (fill, border) = tooltip::float_fill(theme.background, ink, light);
        let family = theme.font_family.clone();
        let primary = theme::ink(ALPHA_PRIMARY, cx);
        let muted = theme::ink(ALPHA_SECONDARY, cx);
        let wash = ink.opacity(if light { ALPHA_SELECTED_LIGHT } else { ALPHA_SELECTED });
        let now = SystemTime::now();
        let line = |text: &'static str| div().h(ROW_HEIGHT).px(FILTER_PAD_X).flex().items_center().text_color(muted).child(text).into_any_element(); // design: `.px` is the horizontal padding method, the value is a token
        let list = match &self.versions {
            None => line("Reading versions"),
            Some(versions) if versions.is_empty() => line("No earlier versions yet"),
            Some(versions) => {
                let rows = versions.len().min(OPEN_QUICKLY_ROWS);
                let list = uniform_list(
                    "versions-rows",
                    versions.len(),
                    cx.processor(move |this: &mut Self, range: std::ops::Range<usize>, _, cx| {
                        let versions = this.versions.as_deref().unwrap_or_default();
                        range
                            .filter_map(|at| {
                                let version = versions.get(at)?;
                                let picked = at == this.selected;
                                Some(
                                    div()
                                        .id(("versions-row", at))
                                        .h(ROW_HEIGHT)
                                        .px(FILTER_PAD_X) // design: `.px` is the horizontal padding method, the value is a token
                                        .flex()
                                        .items_center()
                                        .rounded(RADIUS_ROW)
                                        .when(picked, |row| row.bg(wash))
                                        .child(div().flex_1().min_w_0().truncate().child({
                                            let minute = |at: usize| versions.get(at).map(|version| seconds(version.time).div_euclid(60));
                                            let mine = minute(at);
                                            let shared = mine.is_some() && (minute(at.wrapping_sub(1)) == mine || minute(at + 1) == mine);
                                            row_time(version.time, now, shared)
                                        }))
                                        .child(div().flex_none().pl(ROW_GAP).text_color(muted).whitespace_nowrap().child(size_label(version.len)))
                                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            this.selected = at;
                                            this.commit(cx);
                                        })),
                                )
                            })
                            .collect()
                    }),
                )
                .track_scroll(&self.scroll)
                .size_full();
                // A wheel or a drag of the knob scrolls the rows; the overlay fades the edges while more wait and paints the knob.
                div()
                    .relative()
                    .h(ROW_HEIGHT * rows as f32)
                    .overflow_hidden()
                    .child(scrollbar::watch(&self.scroller))
                    .child(list)
                    .child(scrollbar::overlay(Overlay { handle: Rc::new(self.scroll.clone()), scroller: self.scroller.clone(), ground: Some(fill) }))
                    .into_any_element()
            }
        };
        tooltip::float(RADIUS_FLOAT_CAPSULE, fill, border, light, false)
            .id("versions-list")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| match event.keystroke.key.as_str() {
                "escape" => this.close(cx),
                "up" => this.step(false, cx),
                "down" => this.step(true, cx),
                "home" => this.jump(false, cx),
                "end" => this.jump(true, cx),
                "pageup" => this.page(false, cx),
                "pagedown" => this.page(true, cx),
                "enter" => this.commit(cx),
                _ => return,
            }))
            .occlude()
            .w_full()
            .p(FIND_PAD)
            .font_family(family)
            .text_size(TEXT_BODY)
            .line_height(TEXT_BODY_LINE)
            .text_color(primary)
            .child(list)
    }
}

#[cfg(test)]
mod tests {
    use super::{Local, size_label, time_label, weekday};

    fn at(year: i32, month: i32, day: i32, hour: i32, minute: i32) -> Local {
        Local { year, month, day, hour, minute }
    }

    #[test]
    fn times_read_today_yesterday_then_the_weekday_and_date() {
        let (today, yesterday) = (at(2026, 11, 5, 16, 0), at(2026, 11, 4, 16, 0));
        assert_eq!(time_label(at(2026, 11, 5, 14, 32), today, yesterday, None), "Today 14:32");
        assert_eq!(time_label(at(2026, 11, 4, 9, 10), today, yesterday, None), "Yesterday 09:10");
        assert_eq!(time_label(at(2026, 11, 3, 9, 10), today, yesterday, None), "Tue 3 Nov 09:10");
        assert_eq!(time_label(at(2025, 12, 2, 9, 0), today, yesterday, None), "Tue 2 Dec 2025 09:00");
        assert_eq!(time_label(at(2026, 11, 5, 14, 32), today, yesterday, Some(5)), "Today 14:32:05");
    }

    #[test]
    fn weekdays_come_from_the_calendar() {
        assert_eq!(weekday(1970, 1, 1), 4);
        assert_eq!(weekday(2026, 11, 1), 0);
        assert_eq!(weekday(2024, 2, 29), 4);
    }

    #[test]
    fn sizes_round_up_to_a_unit() {
        assert_eq!(size_label(812), "812 B");
        assert_eq!(size_label(1025), "2 KB");
        assert_eq!(size_label(1_572_864), "1.5 MB");
    }
}
