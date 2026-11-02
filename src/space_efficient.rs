//! Runs the app with macOS malloc's space-efficient mode, so freed heap goes back to the system
//! (gpui-editor#9z0j).
//!
//! macOS's xzone malloc hands empty heap pages to the kernel's deferred reclaim, which takes them
//! back only under memory pressure, and its `malloc_zone_pressure_relief` does nothing. So after
//! closing a 20 MB file about 220 MB of empty pages still counted against the app, a minute
//! later too (measured). `MallocSpaceEfficient=1` turns the deferral off and frees empty
//! pages at once. libmalloc reads it only as the process starts: the shipped bundle sets it in
//! its Info.plist (`LSEnvironment`, `scripts/ship.sh`) for launches from the Dock or Finder, and
//! `ensure` covers any other start by running the binary again with it set (about 6 ms).

use std::os::unix::process::CommandExt;

const KEY: &str = "MallocSpaceEfficient";

/// Without the setting, replaces this process with the same binary, arguments and environment
/// plus the setting, and doesn't return; if that fails it carries on as it is. Programs this one
/// starts (git, a detached copy of itself) inherit the setting.
pub fn ensure() {
    if std::env::var_os(KEY).is_some_and(|value| value == "1") {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let mut args = std::env::args_os();
    let mut command = std::process::Command::new(exe);
    if let Some(arg0) = args.next() {
        command.arg0(arg0);
    }
    let error = command.args(args).env(KEY, "1").exec();
    eprintln!("finch: running without {KEY}: {error}");
}
