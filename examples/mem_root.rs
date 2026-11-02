//! Memory bench: the same window through gpui-kit `open_window` (a Base `Root` around the view) with one text line.
//!
//! `mem_root [width height [redraw_ms]]`: window size in points (default 900 x 700,
//! what `gpui-editor` uses) and, if given, a redraw every `redraw_ms` ms (a
//! blinking cursor asks for one every 500 ms). `MEM_LOG=1` prints the render
//! count once a second to stderr. Measure with `scripts/mem-run.sh`.

use std::time::Duration;

use gpui_kit::*;

struct Line {
    renders: u64,
}

impl Render for Line {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.renders += 1;
        div().size_full().bg(rgb(0xffffff)).text_color(rgb(0x000000)).child("hello")
    }
}

fn main() {
    let args: Vec<f32> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let (w, h) = match args[..] {
        [w, h, ..] => (w, h),
        _ => (900., 700.),
    };
    let redraw_ms = args.get(2).map(|ms| *ms as u64);
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(w), px(h)), cx))),
            ..Default::default()
        };
        let (_, view) = gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| Line { renders: 0 }))
            .expect("failed to open window");
        let view = view.clone();
        cx.spawn(async move |cx| {
            let mut last = 0;
            let mut since_log = Duration::ZERO;
            let tick = Duration::from_millis(redraw_ms.unwrap_or(1000).max(1));
            loop {
                cx.background_executor().timer(tick).await;
                since_log += tick;
                let renders = view
                    .update(cx, |line, cx| {
                        if redraw_ms.is_some() {
                            cx.notify();
                        }
                        line.renders
                    })
                    ;
                if std::env::var_os("MEM_LOG").is_some() && since_log >= Duration::from_secs(1) {
                    eprintln!("renders +{}", renders - last);
                    last = renders;
                    since_log = Duration::ZERO;
                }
            }
        })
        .detach();
    });
}
