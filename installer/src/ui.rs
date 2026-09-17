//! What `meadowup` says while it works, in the manner of rustup:
//!
//! ```text
//! info: latest release is v0.2.0
//! info: updating meadow 0.1.0 -> 0.2.0
//! info: downloading toolchain for aarch64-apple-darwin
//! [=========================>        ]   9.1 MiB /  12.4 MiB ( 73 %)   6.2 MiB/s  ETA  1s
//! info: installing component 'meadow' (12.1 MiB)
//! info: installing component 'meadowup' (0.4 MiB)
//! ```
//!
//! Everything goes to stderr, as rustup's does, so that what a command
//! *answers* -- `meadowup which` -- is all there is on stdout.
//!
//! The bar is drawn only on a terminal. Anywhere else -- a pipe, a CI log --
//! it would be a screenful of carriage returns, so the finished line is written
//! once instead. Colour follows the same rule, and `NO_COLOR` turns it off.

use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

/// Whether stderr is a terminal a person is watching.
pub fn interactive() -> bool {
    std::io::stderr().is_terminal()
}

/// Whether to colour what is written.
///
/// Not on a Windows console that may predate ANSI escapes, unless it is
/// Windows Terminal, which says so in `WT_SESSION`.
fn colour() -> bool {
    interactive()
        && std::env::var_os("NO_COLOR").is_none()
        && (!cfg!(windows) || std::env::var_os("WT_SESSION").is_some())
}

fn label(word: &str, code: &str) -> String {
    if colour() {
        format!("\x1b[1;{code}m{word}\x1b[0m")
    } else {
        word.to_string()
    }
}

/// `info: …`, bold as rustup's is.
pub fn info(msg: impl AsRef<str>) {
    eprintln!("{} {}", label("info:", "39"), msg.as_ref());
}

/// `warning: …`, in bold yellow.
pub fn warn(msg: impl AsRef<str>) {
    eprintln!("{} {}", label("warning:", "33"), msg.as_ref());
}

/// `error: …`, in bold red.
pub fn error(msg: impl AsRef<str>) {
    eprintln!("{} {}", label("error:", "31"), msg.as_ref());
}

/// `text` in bold, for the name of the thing a line is about.
pub fn bold(text: impl AsRef<str>) -> String {
    if colour() {
        format!("\x1b[1m{}\x1b[0m", text.as_ref())
    } else {
        text.as_ref().to_string()
    }
}

/// A size as a person reads one: `512 B`, `8.2 KiB`, `12.4 MiB`.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// A duration to the second: `4s`, `1m 05s`.
fn seconds(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else {
        format!("{}m {:02}s", s / 60, s % 60)
    }
}

/// How wide the bar itself is, in characters.
const BAR: usize = 30;

/// One line describing a download `done` bytes in, of `total` if that is
/// known, `elapsed` since it started.
///
/// Kept apart from the drawing so that it can be tested.
pub fn render(
    done: u64,
    total: Option<u64>,
    elapsed: Duration,
    finished: bool,
    colour: bool,
) -> String {
    let secs = elapsed.as_secs_f64();
    let rate = if secs > 0.05 { done as f64 / secs } else { 0.0 };
    let speed = format!("{:>10}/s", size(rate as u64));

    let Some(total) = total.filter(|&t| t > 0) else {
        // No length to measure against: say how much, and how fast.
        let tail = if finished {
            format!("in {}", seconds(elapsed))
        } else {
            String::new()
        };
        return format!("{:>10}  {speed}  {tail}", size(done))
            .trim_end()
            .to_string();
    };

    let done = done.min(total);
    let fraction = done as f64 / total as f64;
    let filled = (fraction * BAR as f64).round() as usize;
    let (head, rest) = if filled >= BAR {
        ("=".repeat(BAR), String::new())
    } else {
        // The `>` is the last filled cell, not one past it.
        let filled = filled.max(1);
        (
            format!("{}>", "=".repeat(filled - 1)),
            " ".repeat(BAR - filled),
        )
    };
    // The filled part in bold green, as a finished step is in cargo's output;
    // cyan while it is still moving.
    let bar = if colour {
        let code = if finished { "32" } else { "36" };
        format!("\x1b[1;{code}m{head}\x1b[0m{rest}")
    } else {
        format!("{head}{rest}")
    };
    let tail = if finished {
        format!("in {}", seconds(elapsed))
    } else if rate > 0.0 {
        let left = (total - done) as f64 / rate;
        format!("ETA {:>3}", seconds(Duration::from_secs_f64(left)))
    } else {
        String::new()
    };
    format!(
        "[{bar}] {:>10} / {:>10} ({:>3} %) {speed}  {tail}",
        size(done),
        size(total),
        (fraction * 100.0).floor() as u64,
    )
    .trim_end()
    .to_string()
}

/// A progress bar on stderr, redrawn in place.
pub struct Progress {
    total: Option<u64>,
    started: Instant,
    drawn: Option<Instant>,
    /// How long the last line drawn was, so the next can cover it.
    width: usize,
    live: bool,
}

impl Progress {
    pub fn new(total: Option<u64>) -> Progress {
        Progress {
            total,
            started: Instant::now(),
            drawn: None,
            width: 0,
            live: interactive(),
        }
    }

    /// Show that `done` bytes have arrived. Redrawn at most ten times a second:
    /// more is flicker, not information.
    pub fn update(&mut self, done: u64) {
        if !self.live {
            return;
        }
        if self
            .drawn
            .is_some_and(|t| t.elapsed() < Duration::from_millis(100))
        {
            return;
        }
        self.drawn = Some(Instant::now());
        let line = render(done, self.total, self.started.elapsed(), false, colour());
        self.draw(&line);
    }

    /// The finished line, which stays on screen.
    pub fn finish(mut self, done: u64) {
        let total = self.total.or(Some(done));
        let line = render(done, total, self.started.elapsed(), true, colour());
        if self.live {
            self.draw(&line);
            eprintln!();
        } else {
            eprintln!("{line}");
        }
    }

    /// Take the bar off the screen, for when the download failed and an error
    /// is about to be written where it was.
    pub fn abandon(mut self) {
        if self.live && self.width > 0 {
            self.draw("");
        }
    }

    /// Overwrite the current line with `line`. Padding with spaces rather than
    /// an erase escape, which an old Windows console would print as text.
    fn draw(&mut self, line: &str) {
        let shown = visible(line);
        let pad = self.width.saturating_sub(shown);
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "\r{line}{}", " ".repeat(pad));
        if line.is_empty() {
            let _ = write!(err, "\r");
        }
        let _ = err.flush();
        self.width = shown;
    }
}

/// How many columns `line` takes: its characters, less any colour escapes.
fn visible(line: &str) -> usize {
    let mut n = 0;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_as_a_person_would_write_them() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1023), "1023 B");
        assert_eq!(size(1024), "1.0 KiB");
        assert_eq!(size(8 * 1024 * 1024 + 204_800), "8.2 MiB");
        assert_eq!(size(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }

    #[test]
    fn a_bar_half_way_is_half_full_and_says_how_long_is_left() {
        let line = render(50, Some(100), Duration::from_secs(5), false, false);
        assert!(
            line.starts_with("[==============>               ]"),
            "{line}"
        );
        assert!(line.contains("( 50 %)"), "{line}");
        // 50 bytes in 5 seconds leaves 50 bytes, 5 more seconds.
        assert!(line.ends_with("ETA  5s"), "{line}");
    }

    #[test]
    fn a_finished_bar_is_full_and_says_how_long_it_took() {
        let line = render(100, Some(100), Duration::from_secs(3), true, false);
        assert!(
            line.starts_with(&format!("[{}]", "=".repeat(BAR))),
            "{line}"
        );
        assert!(line.contains("(100 %)"), "{line}");
        assert!(line.ends_with("in 3s"), "{line}");
    }

    #[test]
    fn a_bar_never_runs_past_its_end() {
        // A server that sends more than it said it would must not break the
        // drawing.
        let line = render(150, Some(100), Duration::from_secs(1), false, false);
        assert!(line.contains("(100 %)"), "{line}");
        assert!(
            line.starts_with(&format!("[{}]", "=".repeat(BAR))),
            "{line}"
        );
    }

    #[test]
    fn with_no_length_the_amount_and_speed_are_still_shown() {
        let line = render(2048, None, Duration::from_secs(2), false, false);
        assert!(!line.contains('['), "{line}");
        assert!(line.contains("2.0 KiB"), "{line}");
        assert!(line.contains("1.0 KiB/s"), "{line}");
    }

    #[test]
    fn nothing_yet_is_not_a_division_by_zero() {
        let line = render(0, Some(100), Duration::ZERO, false, false);
        assert!(line.contains("(  0 %)"), "{line}");
        assert!(!line.contains("ETA"), "{line}");
    }

    #[test]
    fn a_coloured_bar_takes_the_same_columns_as_a_plain_one() {
        // The escapes must not count, or redrawing would leave debris behind.
        let plain = render(40, Some(100), Duration::from_secs(2), false, false);
        let coloured = render(40, Some(100), Duration::from_secs(2), false, true);
        assert_ne!(plain, coloured);
        assert!(coloured.contains("\x1b[1;36m"), "{coloured:?}");
        assert_eq!(visible(&coloured), plain.chars().count());
    }

    #[test]
    fn a_finished_bar_turns_green() {
        let line = render(100, Some(100), Duration::from_secs(1), true, true);
        assert!(line.contains("\x1b[1;32m"), "{line:?}");
    }

    #[test]
    fn long_waits_read_in_minutes() {
        assert_eq!(seconds(Duration::from_secs(65)), "1m 05s");
        assert_eq!(seconds(Duration::from_secs(9)), "9s");
    }
}
