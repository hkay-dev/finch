---
title: Following a log file
feature: log-follow
type: guide
summary: A file another program is writing keeps its end in view, and a log shows its errors and warnings at a glance.
covers:
  - src/log_follow.rs
  - src/workspace.rs
  - src/document.rs
  - src/encoding.rs
  - src/scrollbar.rs
  - vendor/gpui-base/src/input/base/state.rs
since: 2026-11-01
---

Open a log that another program is writing, put the caret on its last line, and the new lines
arrive as they are written while the view glides down to keep them in sight. A small chip in the
top right corner of the editor, styled like the Find count, says **Following**. It fades in with a
small rise and fades out when following ends for good, and when you let go it eases to **Follow**
and back when you follow again, turning around from where it is if you change your mind midway.

The first lines to arrive after a quiet spell (two seconds with nothing new) wash once, softly,
the way a reload does. A log that keeps writing never washes: each line restarts the quiet clock.

## Start and stop

Following starts by itself the first time the file grows on disk while the caret is on its last
line. Click at the end of the file, or press Cmd-Down, and leave the window alone.

- **Let go:** scroll up with the wheel or trackpad, type, undo, or move the caret off the last
  line. The chip stays, resting, and reads **Follow**. The lines keep arriving, but the view and the
  caret stay where you left them.
- **Follow again:** click the chip, press End or Cmd-Down, or click at the very end of the text. The
  caret goes to the end and the view comes down to it.

Nothing about following is saved: a file you reopen starts with no chip until it grows again with
the caret on its last line. The chip goes with the tab.

## What it costs

When the file only grew (its length is at least what Finch holds and the old bytes still match),
Finch reads just the new bytes in the background and adds them at the end of the text on the main
thread. It does not lose your undo history. How the old bytes are checked depends on the size:
a file of 8 MB or less, or one not yet followed, has every old byte compared with the disk, so a
change anywhere in it is seen. A followed file over 8 MB has every old byte compared once, when
it is first read whole or compared; after that only its last 4 KB are, and the file must still be
the same file on disk (same device and inode). An edit in the middle of such a big file that keeps
its length and leaves the end alone is not seen until the next whole read. Anything that fails the
check, such as a file that was cut short, rotated, replaced or edited where it is compared, is read
whole like any other change from outside. A file that keeps growing is read at most four times a
second, one read at a time.

Edits you have not saved are never read over: while the tab has unsaved edits, new lines wait on
disk and the usual conflict mark shows when you save. A file in another encoding than UTF-8, and a
file with old-Mac line ends or a NUL, take the whole read. A file that grows past 4 MB loses its
syntax colors; it keeps everything else.

To see each read, start Finch with `FINCH_FOLLOW_TRACE=1`: every read prints `follow tail N bytes
read` (only the appended bytes) or `follow full N bytes`.

## Logs at a glance

A file named `.log`, or one whose lines mostly start with a timestamp (the first 200 lines, four
in five, once it has five), looks like a log:

- Lines with ERROR, FATAL, CRITICAL, SEVERE or PANIC get a light red tint, and WARN or WARNING a
  light amber one. The word must be capitals, or sit between brackets or after `level=`, near the
  start of the line (`[error]`, `level=warn`, `"level":"error"`).
- A timestamp at the start of a line dims: `2024-05-01 12:00:00,123`, `[2024-05-01T12:00:00Z]`,
  `12:00:00.250`, or syslog's `May  1 12:00:00`.
- The scrollbar's track shows a red or amber tick for each stretch of flagged lines, whole file
  long, up to the last 50,000 flagged lines, for files up to 64 MB. The ticks catch up about a third
  of a second after the file changes, and a tick for a line that has just arrived fades in over a
  fifth of a second (only the new ones; opening a log or a catch-up of old lines brings none).

Only the lines on screen and a screenful each way are tinted, so a very large log costs no more
to show than a small one.
