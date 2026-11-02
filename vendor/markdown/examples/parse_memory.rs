//! Standalone parse-only memory measurement: run under `/usr/bin/time -l`.
//! Set `MallocSpaceEfficient=1` to match Finch. Pass `--dump` to compare mdast.
use std::{env, fs, time::Instant};

fn main() {
    let mut args = env::args().skip(1);
    let input = fs::read_to_string(args.next().expect("Markdown file argument")).unwrap();
    let mut options = markdown::ParseOptions::gfm();
    options.constructs.math_flow = true;
    options.constructs.math_text = true;
    let start = Instant::now();
    let tree = markdown::to_mdast(&input, &options).unwrap();
    eprintln!(
        "parse: {} bytes, {:.6} s",
        input.len(),
        start.elapsed().as_secs_f64()
    );
    if args.next().as_deref() == Some("--dump") {
        println!("{tree:?}");
    }
    std::hint::black_box(&tree);
}
