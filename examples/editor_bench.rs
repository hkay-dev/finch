//! Bench for GPUI Kit's `Editor` on big files (gpui-editor#xs3g). Uses only `gpui_kit`.
//!
//! `editor_bench <path>` opens the file in an `EditorState` the way `src/main.rs` does.
//! Everything else is behind env vars (stderr output, one `EB` prefixed line per fact):
//!
//! - `EB_WRAP=0`     soft wrap off (the app's default is on)
//! - `EB_SCROLL=1`   after layout, inject wheel events in-process (no OS input), `EB_HZ` events
//!                   a second (165), `EB_STEP` px each (30), `EB_SECS` seconds each way (3):
//!                   frame gaps, offset moved against sent, per-frame step
//! - `EB_RESIZE=1`   resize the window over 7 widths (`EB_RESIZE_N` takes the first n), log first-frame latency and the longest
//!                   main-thread stall (a 1 ms foreground ticker's lateness) after each
//! - `EB_HOLD=<s>`   after layout, stay open <s> seconds (for `footprint`), then quit
//! - `EB_QUIT=1`     quit once the first frame is drawn (open time only)
//! - `EB_SMOOTH=0`   the wheel jumps by each event's delta (before gpui-editor#ases); default 1, eased
//! - `EB_JITTER=1`   wheel events at uneven 6 to 14 ms gaps (a smoothing mouse driver), same px per ms
//! - `EB_SETTLE=1`   after the legs, count frames drawn 0.5 to 5.5 s after the last wheel event (must be 0)
//!
//! `ZED_MEASUREMENTS=1` adds GPUI's own `frame duration` lines.

use std::{
    cell::RefCell,
    sync::LazyLock,
    time::{Duration, Instant},
};

use gpui_kit::component::{
    ActiveTheme as _,
    input::{Editor, EditorState},
};
use gpui_kit::*;

#[path = "../src/window_look.rs"]
mod window_look;

static T0: LazyLock<Instant> = LazyLock::new(Instant::now);

thread_local! {
    /// One entry per painted frame: when, and the editor's scroll offset y.
    static FRAMES: RefCell<Vec<(Instant, f32)>> = const { RefCell::new(Vec::new()) };
    /// Foreground ticker: (time it should have run, how late it ran).
    static LATE: RefCell<Vec<(Instant, Duration)>> = const { RefCell::new(Vec::new()) };
}

fn env_f(key: &str, default: f32) -> f32 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn env_on(key: &str) -> bool {
    std::env::var(key).is_ok_and(|v| v != "0" && !v.is_empty())
}

struct BenchWindow {
    editor: Entity<EditorState>,
}

impl Render for BenchWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = self.editor.clone();
        div()
            .size_full()
            .bg(cx.theme().background)
            .child(Editor::new(&self.editor).bordered(false).size_full())
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, _, cx| {
                        let y = f32::from(editor.read(cx).scroll_offset().y);
                        FRAMES.with(|f| f.borrow_mut().push((Instant::now(), y)));
                    },
                )
                .absolute()
                .w(px(1.))
                .h(px(1.)),
            )
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.
}

fn pct(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.;
    }
    sorted[((sorted.len() as f64 - 1.) * q).round() as usize]
}

fn frames_between(a: Instant, b: Instant) -> Vec<(Instant, f32)> {
    FRAMES.with(|f| f.borrow().iter().filter(|(t, _)| *t >= a && *t <= b).copied().collect())
}

fn report_frames(label: &str, frames: &[(Instant, f32)], wall: Duration) {
    let mut gaps: Vec<f64> = frames.windows(2).map(|w| ms(w[1].0 - w[0].0)).collect();
    // Speed in px per ms for each frame pair, to see jitter in the per-frame step.
    let speed: Vec<f64> = frames
        .windows(2)
        .map(|w| (w[1].1 - w[0].1) as f64 / ms(w[1].0 - w[0].0).max(0.001))
        .collect();
    // Step ratio: each frame's speed over the one before, away from the ends of the leg (a steady
    // wheel has speed ratio 1 between frames; an uneven one swings from half to double).
    let inner = &speed[speed.len() / 6..speed.len() * 5 / 6];
    let mut ratios: Vec<f64> = inner.windows(2).filter(|w| w[0].abs() > 0.01).map(|w| w[1] / w[0]).collect();
    ratios.sort_by(f64::total_cmp);
    let outside = ratios.iter().filter(|r| **r < 0.5 || **r > 2.).count();
    let mut speed = speed;
    let zero_steps = frames.windows(2).filter(|w| w[1].1 == w[0].1).count();
    gaps.sort_by(f64::total_cmp);
    speed.sort_by(f64::total_cmp);
    let n = frames.len();
    eprintln!(
        "EB {label} frames={n} fps={:.1} gap_ms p50={:.2} p90={:.2} p99={:.2} max={:.2} over9.1={} over16.7={} zero_step_frames={zero_steps}",
        n as f64 / wall.as_secs_f64(),
        pct(&gaps, 0.5),
        pct(&gaps, 0.9),
        pct(&gaps, 0.99),
        pct(&gaps, 1.0),
        gaps.iter().filter(|g| **g > 9.1).count(),
        gaps.iter().filter(|g| **g > 16.7).count(),
    );
    eprintln!(
        "EB {label} step_px_per_ms p5={:.2} p50={:.2} p95={:.2}",
        pct(&speed, 0.05),
        pct(&speed, 0.5),
        pct(&speed, 0.95)
    );
    eprintln!(
        "EB {label} step_ratio p5={:.2} p50={:.2} p95={:.2} outside_half_to_double={outside} of {}",
        pct(&ratios, 0.05),
        pct(&ratios, 0.5),
        pct(&ratios, 0.95),
        ratios.len()
    );
}

fn main() {
    LazyLock::force(&T0);
    let path = std::env::args_os().nth(1).expect("usage: editor_bench <path>");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("can't read {path:?} (args {:?}): {err}", std::env::args_os().collect::<Vec<_>>()));
    let lines = text.lines().count();
    let bytes = text.len();
    let wrap = std::env::var("EB_WRAP").map_or(true, |v| v != "0");

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        // Launched by scripts/editor-bench.sh through an LSUIElement bundle; GPUI would make it Regular.
        cx.set_activation_policy(gpui_kit::ActivationPolicy::Accessory);
        cx.on_window_closed(|cx, _| cx.quit()).detach();

        let (w, h) = (900., 700.);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(w), px(h)), cx))),
            titlebar: Some(TitlebarOptions { title: Some("editor_bench".into()), ..Default::default() }),
            focus: false,
            // Unshown: `free_frame_pacing` below puts it on screen at the back, never in front.
            show: false,
            ..Default::default()
        };
        let opened = T0.elapsed();
        let (handle, editor) = gpui_kit::open_window(options, cx, |window, cx| {
            let smooth = env_f("EB_SMOOTH", 1.) != 0.;
            let editor = cx.new(|cx| EditorState::new(window, cx).soft_wrap(wrap).smooth_wheel(smooth).default_value(text));
            // The window the app opens: display sync off on its layer, and drawing while covered.
            window_look::free_frame_pacing(window, true);
            editor.update(cx, |state, cx| state.focus(window, cx));
            cx.new(|_| BenchWindow { editor })
        })
        .expect("failed to open the editor window");
        let editor = editor.read(cx).editor.clone();
        eprintln!(
            "EB open lines={lines} bytes={bytes} wrap={wrap} open_window_returned_ms={:.1} (from main start, includes read+rope build)",
            ms(opened.max(T0.elapsed()))
        );

        // Foreground ticker: how late a 1 ms timer resumes on the main thread.
        cx.spawn(async move |cx: &mut AsyncApp| {
            let ex = cx.background_executor().clone();
            loop {
                let due = Instant::now() + Duration::from_millis(1);
                ex.timer(Duration::from_millis(1)).await;
                let late = Instant::now().saturating_duration_since(due);
                LATE.with(|l| l.borrow_mut().push((due, late)));
            }
        })
        .detach();

        cx.spawn(async move |cx: &mut AsyncApp| {
            let ex = cx.background_executor().clone();
            // Wait for the first laid-out frame.
            let waited = Instant::now();
            let first = loop {
                if let Some(f) = FRAMES.with(|f| f.borrow().first().copied()) {
                    break Some(f);
                }
                if waited.elapsed() > Duration::from_secs(150) {
                    break None;
                }
                ex.timer(Duration::from_millis(5)).await;
            };
            let Some((first, _)) = first else {
                eprintln!("EB NO FRAMES in 150 s: the window never drew (covered window?)");
                cx.update(|cx| cx.quit());
                return;
            };
            eprintln!("EB first_frame_ms={:.1} (from main start)", ms(first.duration_since(*T0)));
            if env_on("EB_QUIT") {
                ex.timer(Duration::from_millis(200)).await;
                cx.update(|cx| cx.quit());
                return;
            }
            ex.timer(Duration::from_millis(1500)).await;
            let line_height = cx.update(|cx| editor.read(cx).line_height().map(f32::from));
            let rows = cx.update(|cx| editor.read(cx).visible_row_range());
            eprintln!("EB laid_out line_height={line_height:?} visible_rows={rows:?}");
            let hold = env_f("EB_HOLD", 0.);
            if hold > 0. {
                // Stay open so `footprint` can read the process (gpui-editor#8mgd).
                ex.timer(Duration::from_secs_f32(hold)).await;
                cx.update(|cx| cx.quit());
                return;
            }

            let mut last_end = Instant::now();
            if env_on("EB_SCROLL") {
                let hz = env_f("EB_HZ", 165.);
                let step = env_f("EB_STEP", 30.);
                let secs = env_f("EB_SECS", 3.);
                let gap = Duration::from_secs_f32(1. / hz);
                let at = point(px(w / 2.), px(h / 2.));
                for (name, dir) in [("down", -1f32), ("up", 1.)] {
                    let before = cx.update(|cx| f32::from(editor.read(cx).scroll_offset().y));
                    // Event times (ms from the leg's start) and each event's gap. With EB_JITTER the
                    // gaps run uneven between 6 and 14 ms and the delta follows the gap, as a mouse
                    // driver's smoothing posts them (the same px per ms either way).
                    let nominal = 1000. / hz as f64;
                    let mut rng = 0x2545_F491u32;
                    let mut times = Vec::new();
                    let mut deltas = Vec::new();
                    let mut t = 0f64;
                    while t < secs as f64 * 1000. {
                        let g = if env_on("EB_JITTER") {
                            rng ^= rng << 13;
                            rng ^= rng >> 17;
                            rng ^= rng << 5;
                            6. + 8. * (rng as f64 / u32::MAX as f64)
                        } else {
                            nominal
                        };
                        times.push(t);
                        // EB_UP=<0..1> scales the up leg, so it can stop short of the top (an edge stretch is its own motion).
                        let leg = if dir > 0. { env_f("EB_UP", 1.) } else { 1. };
                        deltas.push(step * dir * leg * (g / nominal) as f32);
                        t += g;
                    }
                    let count = times.len();
                    let began = Instant::now();
                    let mut sent = 0f32;
                    let mut late_sends = 0;
                    let mut dispatch = Vec::new();
                    for i in 0..count {
                        let due = began + Duration::from_secs_f64(times[i] / 1000.);
                        let now = Instant::now();
                        if due > now {
                            ex.timer(due - now).await;
                        }
                        if Instant::now() > due + gap {
                            late_sends += 1;
                        }
                        let phase = if i == 0 {
                            TouchPhase::Started
                        } else if i + 1 == count {
                            TouchPhase::Ended
                        } else {
                            TouchPhase::Moved
                        };
                        let delta = deltas[i];
                        let event = ScrollEvent {
                            position: at,
                            delta: ScrollDelta::Pixels(point(px(0.), px(delta))),
                            modifiers: Modifiers::default(),
                            touch_phase: phase,
                            source: ScrollSource::Pointer,
                        };
                        let t = Instant::now();
                        let _ = handle.update(cx, |_, window, cx| window.dispatch_event(PlatformInput::Scroll(event), cx));
                        dispatch.push(ms(t.elapsed()));
                        sent += delta;
                    }
                    let end = Instant::now();
                    last_end = end;
                    let epoch = |i: Instant| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64() * 1000. - ms(Instant::now() - i);
                    eprintln!("EB legtime {name} {:.3} {:.3}", epoch(began), epoch(end));
                    ex.timer(Duration::from_millis(300)).await;
                    let after = cx.update(|cx| f32::from(editor.read(cx).scroll_offset().y));
                    let wall = end - began;
                    dispatch.sort_by(f64::total_cmp);
                    eprintln!(
                        "EB scroll {name} sent_events={count} sent_px={sent:.0} moved_px={:.0} wall_s={:.2} sends_over_1_gap_late={late_sends} dispatch_ms p50={:.2} p99={:.2} max={:.2}",
                        after - before,
                        wall.as_secs_f64(),
                        pct(&dispatch, 0.5),
                        pct(&dispatch, 0.99),
                        pct(&dispatch, 1.0),
                    );
                    report_frames(&format!("scroll {name}"), &frames_between(began, end), wall);
                    let late: Vec<f64> = LATE.with(|l| l.borrow().iter().filter(|(t, _)| *t >= began && *t <= end).map(|(_, d)| ms(*d)).collect());
                    let max_late = late.iter().copied().fold(0., f64::max);
                    eprintln!("EB scroll {name} main_thread_ticker_late_ms max={max_late:.1} over8={}", late.iter().filter(|d| **d > 8.).count());
                }
            }

            // Nothing may ask for a frame once the scroll has settled: count the frames drawn
            // from 0.5 s to 5.5 s after the last wheel event (the glide's tail is over by then).
            if env_on("EB_SETTLE") {
                let from = last_end + Duration::from_millis(500);
                let to = last_end + Duration::from_millis(5500);
                let now = Instant::now();
                if to > now {
                    ex.timer(to - now).await;
                }
                let frames = frames_between(from, to);
                eprintln!("EB settle frames_in_5s_after_motion={}", frames.len());
                let at: Vec<String> = frames.iter().map(|(t, _)| format!("{:.0}", ms(*t - last_end))).collect();
                eprintln!("EB settle frame_ms_after_last_wheel [{}]", at.join(" "));
            }

            if env_on("EB_RESIZE") {
                let widths = [700., 1100., 500., 900., 640., 1000., 900.];
                for width in widths.into_iter().take(env_f("EB_RESIZE_N", 7.) as usize) {
                    let at = Instant::now();
                    let _ = handle.update(cx, |_, window, _| window.resize(size(px(width), px(h))));
                    ex.timer(Duration::from_millis(1500)).await;
                    let frames = frames_between(at, at + Duration::from_millis(1500));
                    let first_after = frames.first().map(|(t, _)| ms(*t - at));
                    let max_gap = frames.windows(2).map(|w| ms(w[1].0 - w[0].0)).fold(0., f64::max);
                    let late: Vec<f64> = LATE.with(|l| l.borrow().iter().filter(|(t, _)| *t >= at && *t <= at + Duration::from_millis(1500)).map(|(_, d)| ms(*d)).collect());
                    let max_late = late.iter().copied().fold(0., f64::max);
                    eprintln!(
                        "EB resize to={width} frames={} first_frame_after_ms={first_after:.1?} max_frame_gap_ms={max_gap:.1} main_thread_stall_ms={max_late:.1}",
                        frames.len()
                    );
                }
            }

            eprintln!("EB total_frames={}", FRAMES.with(|f| f.borrow().len()));
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
}
