//! Driving a terminal: what `Std.Terminal` asks of a runtime, the same for
//! both.
//!
//! Three things. **Raw mode** ([`raw`]): keys arrive as they are pressed,
//! unechoed, and Ctrl-C is a key rather than a signal -- what a line editor
//! needs. It is undone when the program ends however it ends, since a shell
//! left in raw mode is one nobody can type into. **A key at a time**
//! ([`read_key`]): the bytes a terminal sends decoded here, once, into what
//! was pressed -- a character, a named key, with its modifiers -- and a
//! paste, and a change of size. And **the size** ([`size`]).
//!
//! A key crosses to the program as three words, `(code, text, modifiers)`,
//! which `Std.Terminal` makes a `Key` of: see [`code`] and [`modifier`].
//!
//! Unix only, so far: on Windows [`raw`] answers that it could not, and a
//! program falls back to reading lines.

/// What was pressed, as `Std.Terminal` numbers it.
pub mod code {
    pub const END: i64 = 0;
    /// A character, which is the text.
    pub const CHAR: i64 = 1;
    pub const ENTER: i64 = 2;
    pub const TAB: i64 = 3;
    pub const BACKSPACE: i64 = 4;
    pub const DELETE: i64 = 5;
    pub const ESCAPE: i64 = 6;
    pub const UP: i64 = 7;
    pub const DOWN: i64 = 8;
    pub const LEFT: i64 = 9;
    pub const RIGHT: i64 = 10;
    pub const HOME: i64 = 11;
    pub const END_KEY: i64 = 12;
    pub const PAGE_UP: i64 = 13;
    pub const PAGE_DOWN: i64 = 14;
    pub const INSERT: i64 = 15;
    /// A function key: this and its number, so F5 is 105.
    pub const FUNCTION: i64 = 100;
    /// The terminal changed size.
    pub const RESIZE: i64 = 20;
    /// Text pasted, all of it, as the text.
    pub const PASTE: i64 = 21;
}

/// The keys held with it, added together.
pub mod modifier {
    pub const SHIFT: i64 = 1;
    pub const ALT: i64 = 2;
    pub const CTRL: i64 = 4;
}

/// One event: its [`code`], its text, its [`modifier`]s.
pub type Event = (i64, String, i64);

/// The bytes of `input` from `at` as one event, and how many bytes it took --
/// or `None` when they are the start of something more has to arrive for.
/// `alone` says nothing more is coming just now: an escape with nothing
/// after it is the Escape key, where one with more to come begins a
/// sequence.
pub fn decode(input: &[u8], alone: bool) -> Option<(Event, usize)> {
    use code::*;
    let key = |c: i64, m: i64, n: usize| Some(((c, String::new(), m), n));
    let ch = |t: &str, m: i64, n: usize| Some(((CHAR, t.to_string(), m), n));
    let b = *input.first()?;
    match b {
        0x1b => {
            let Some(&next) = input.get(1) else {
                return if alone { key(ESCAPE, 0, 1) } else { None };
            };
            match next {
                b'[' => csi(input, alone),
                b'O' => match input.get(2) {
                    None if alone => key(ESCAPE, 0, 1),
                    None => None,
                    Some(c) => match c {
                        b'A' => key(UP, 0, 3),
                        b'B' => key(DOWN, 0, 3),
                        b'C' => key(RIGHT, 0, 3),
                        b'D' => key(LEFT, 0, 3),
                        b'H' => key(HOME, 0, 3),
                        b'F' => key(END_KEY, 0, 3),
                        b'P'..=b'S' => {
                            Some(((FUNCTION + i64::from(1 + c - b'P'), String::new(), 0), 3))
                        }
                        _ => key(ESCAPE, 0, 1),
                    },
                },
                // Escape and then a key is that key with Alt.
                _ => {
                    let ((c, text, mods), n) = decode(&input[1..], alone)?;
                    Some(((c, text, mods | modifier::ALT), n + 1))
                }
            }
        }
        b'\r' => key(ENTER, 0, 1),
        b'\t' => key(TAB, 0, 1),
        0x7f => key(BACKSPACE, 0, 1),
        0x08 => key(BACKSPACE, modifier::CTRL, 1),
        0x00 => ch(" ", modifier::CTRL, 1),
        // Ctrl with a letter is the letter's place in the alphabet: Ctrl-J is
        // a line feed, which with input unprocessed Enter is not.
        0x01..=0x1a => ch(&((b'a' + b - 1) as char).to_string(), modifier::CTRL, 1),
        0x1c..=0x1f => ch(&((b + 0x40) as char).to_string(), modifier::CTRL, 1),
        _ => {
            // A character, however many bytes of UTF-8 it is.
            let len = match b {
                0x00..=0x7f => 1,
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf7 => 4,
                _ => 1,
            };
            if input.len() < len {
                return if alone {
                    ch("\u{fffd}", 0, input.len())
                } else {
                    None
                };
            }
            match std::str::from_utf8(&input[..len]) {
                Ok(t) => ch(t, 0, len),
                Err(_) => ch("\u{fffd}", 0, 1),
            }
        }
    }
}

/// `ESC [ parameters final`: the arrows and their kin, a function key, a
/// paste.
fn csi(input: &[u8], alone: bool) -> Option<(Event, usize)> {
    use code::*;
    let body = &input[2..];
    let Some(end) = body.iter().position(|b| (0x40..=0x7e).contains(b)) else {
        return if alone {
            Some(((ESCAPE, String::new(), 0), 1))
        } else {
            None
        };
    };
    let taken = 2 + end + 1;
    let params: Vec<i64> = std::str::from_utf8(&body[..end])
        .unwrap_or("")
        .split(';')
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    let first = params.first().copied().unwrap_or(0);
    // xterm says which keys were held as one more than their sum.
    let mods = params.get(1).map_or(0, |m| (m - 1).clamp(0, 7));
    let key = |c: i64| Some(((c, String::new(), mods), taken));
    match body[end] {
        b'A' => key(UP),
        b'B' => key(DOWN),
        b'C' => key(RIGHT),
        b'D' => key(LEFT),
        b'H' => key(HOME),
        b'F' => key(END_KEY),
        b'Z' => Some(((TAB, String::new(), modifier::SHIFT), taken)),
        b'P'..=b'S' => Some((
            (
                FUNCTION + i64::from(1 + body[end] - b'P'),
                String::new(),
                mods,
            ),
            taken,
        )),
        b'~' => match first {
            1 | 7 => key(HOME),
            2 => key(INSERT),
            3 => key(DELETE),
            4 | 8 => key(END_KEY),
            5 => key(PAGE_UP),
            6 => key(PAGE_DOWN),
            11..=15 => Some((
                (FUNCTION + i64::from(first - 10), String::new(), mods),
                taken,
            )),
            17..=21 => Some((
                (FUNCTION + i64::from(first - 11), String::new(), mods),
                taken,
            )),
            23 | 24 => Some((
                (FUNCTION + i64::from(first - 12), String::new(), mods),
                taken,
            )),
            // A paste: everything up to the mark that ends it, as it is.
            200 => {
                const DONE: &[u8] = b"\x1b[201~";
                let rest = &input[taken..];
                match rest.windows(DONE.len()).position(|w| w == DONE) {
                    Some(at) => Some((
                        (PASTE, String::from_utf8_lossy(&rest[..at]).into_owned(), 0),
                        taken + at + DONE.len(),
                    )),
                    None if alone => Some((
                        (PASTE, String::from_utf8_lossy(rest).into_owned(), 0),
                        input.len(),
                    )),
                    None => None,
                }
            }
            _ => key(ESCAPE),
        },
        _ => key(ESCAPE),
    }
}

#[cfg(unix)]
mod sys {
    use super::{Event, code, decode};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// The terminal as it was before [`raw`] changed it, while it is changed.
    static WAS: Mutex<Option<libc::termios>> = Mutex::new(None);
    /// Bytes read and not yet made events of.
    static PENDING: Mutex<Vec<u8>> = Mutex::new(Vec::new());
    static RESIZED: AtomicBool = AtomicBool::new(false);
    static HOOKED: AtomicBool = AtomicBool::new(false);

    extern "C" fn resized(_: libc::c_int) {
        RESIZED.store(true, Ordering::Relaxed);
    }

    extern "C" fn restore() {
        leave();
    }

    fn leave() -> bool {
        let Some(was) = WAS.lock().unwrap_or_else(|p| p.into_inner()).take() else {
            return true;
        };
        // Pastes are plain input again, and the terminal is as it was.
        write(b"\x1b[?2004l");
        // Safety: standard input, and the settings it gave us.
        unsafe { libc::tcsetattr(0, libc::TCSAFLUSH, &was) == 0 }
    }

    fn write(bytes: &[u8]) {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(bytes);
        let _ = out.flush();
    }

    pub fn raw(on: bool) -> bool {
        if !on {
            return leave();
        }
        let mut was = WAS.lock().unwrap_or_else(|p| p.into_inner());
        if was.is_some() {
            return true;
        }
        // Safety: standard input, asked what it is set to and set again as
        // termios documents; one that is not a terminal fails the first call.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return false;
            }
            let before = t;
            // No echo, no waiting for a line, no signals from keys, no
            // translating what is typed. Output is left as it is: a newline
            // written still starts a line.
            t.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG | libc::IEXTEN);
            t.c_iflag &= !(libc::IXON | libc::ICRNL | libc::BRKINT | libc::INPCK | libc::ISTRIP);
            t.c_cc[libc::VMIN] = 1;
            t.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(0, libc::TCSAFLUSH, &t) != 0 {
                return false;
            }
            *was = Some(before);
            if !HOOKED.swap(true, Ordering::Relaxed) {
                // However the program ends, the terminal is put back; and a
                // change of size interrupts a read, to be said as an event.
                libc::atexit(restore);
                let mut act: libc::sigaction = std::mem::zeroed();
                act.sa_sigaction = resized as usize;
                libc::sigaction(libc::SIGWINCH, &act, std::ptr::null_mut());
            }
        }
        drop(was);
        // A paste arrives marked as one, so its newlines are text.
        write(b"\x1b[?2004h");
        true
    }

    pub fn size() -> (i64, i64) {
        for fd in [1, 0, 2] {
            // Safety: asking a descriptor its window size, which one that is
            // not a terminal declines.
            unsafe {
                let mut w: libc::winsize = std::mem::zeroed();
                if libc::ioctl(fd, libc::TIOCGWINSZ, &mut w) == 0 && w.ws_col > 0 {
                    return (i64::from(w.ws_col), i64::from(w.ws_row));
                }
            }
        }
        (0, 0)
    }

    /// Whether standard input has something to read within `ms`.
    fn ready(ms: i32) -> bool {
        let mut p = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        // Safety: one descriptor, ours.
        unsafe { libc::poll(&mut p, 1, ms) > 0 }
    }

    pub fn read_key() -> Event {
        // What was written before the program waits is what whoever types is
        // waiting for.
        write(b"");
        let mut pending = PENDING.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if RESIZED.swap(false, Ordering::Relaxed) {
                return (code::RESIZE, String::new(), 0);
            }
            if !pending.is_empty() {
                // An escape sequence arrives in one piece or very nearly: a
                // moment with nothing more is the Escape key itself.
                let alone = !ready(if pending[0] == 0x1b { 30 } else { 0 });
                if let Some((event, taken)) = decode(&pending, alone) {
                    pending.drain(..taken);
                    return event;
                }
            }
            let mut buf = [0u8; 4096];
            // Safety: a buffer of that length, ours.
            let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
            match n {
                0 => return (code::END, String::new(), 0),
                n if n < 0 => {
                    // Interrupted, which a change of size does: said above.
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return (code::END, String::new(), 0);
                }
                n => pending.extend_from_slice(&buf[..n as usize]),
            }
        }
    }
}

#[cfg(not(unix))]
mod sys {
    use super::{Event, code};

    pub fn raw(_on: bool) -> bool {
        false
    }

    pub fn size() -> (i64, i64) {
        (0, 0)
    }

    pub fn read_key() -> Event {
        (code::END, String::new(), 0)
    }
}

/// Enter raw mode, or leave it: whether the terminal is now as asked. Not on
/// a terminal, entering answers no and changes nothing.
pub fn raw(on: bool) -> bool {
    sys::raw(on)
}

/// Columns and rows, or zeros where there is no terminal to ask.
pub fn size() -> (i64, i64) {
    sys::size()
}

/// Wait for the next key, paste or change of size.
pub fn read_key() -> Event {
    sys::read_key()
}

#[cfg(test)]
mod tests {
    use super::code::*;
    use super::modifier::*;
    use super::*;

    fn all(mut input: &[u8]) -> Vec<Event> {
        let mut out = Vec::new();
        while let Some((e, n)) = decode(input, true) {
            out.push(e);
            input = &input[n..];
        }
        out
    }

    fn key(c: i64, m: i64) -> Event {
        (c, String::new(), m)
    }

    fn ch(t: &str, m: i64) -> Event {
        (CHAR, t.to_string(), m)
    }

    #[test]
    fn characters_arrive_whole() {
        assert_eq!(
            all("aé→😀".as_bytes()),
            vec![ch("a", 0), ch("é", 0), ch("→", 0), ch("😀", 0)]
        );
        // Half a character waits for the rest, unless nothing is coming.
        assert_eq!(decode(&"é".as_bytes()[..1], false), None);
    }

    #[test]
    fn enter_is_not_ctrl_j() {
        assert_eq!(
            all(b"\r\n\t\x7f"),
            vec![key(ENTER, 0), ch("j", CTRL), key(TAB, 0), key(BACKSPACE, 0)]
        );
        assert_eq!(all(b"\x03\x06"), vec![ch("c", CTRL), ch("f", CTRL)]);
    }

    #[test]
    fn sequences_are_keys() {
        assert_eq!(
            all(b"\x1b[A\x1b[B\x1b[C\x1b[D\x1b[H\x1b[F\x1b[3~\x1b[5~\x1b[6~"),
            vec![
                UP, DOWN, RIGHT, LEFT, HOME, END_KEY, DELETE, PAGE_UP, PAGE_DOWN
            ]
            .into_iter()
            .map(|c| key(c, 0))
            .collect::<Vec<_>>()
        );
        assert_eq!(
            all(b"\x1b[1;5C\x1b[1;2A\x1b[Z"),
            vec![key(RIGHT, CTRL), key(UP, SHIFT), key(TAB, SHIFT)]
        );
        assert_eq!(
            all(b"\x1bOP\x1b[15~"),
            vec![
                (FUNCTION + 1, String::new(), 0),
                (FUNCTION + 5, String::new(), 0)
            ]
        );
    }

    #[test]
    fn escape_alone_is_escape_and_before_a_key_is_alt() {
        assert_eq!(all(b"\x1b"), vec![key(ESCAPE, 0)]);
        assert_eq!(decode(b"\x1b", false), None);
        assert_eq!(all(b"\x1bf\x1b\r"), vec![ch("f", ALT), key(ENTER, ALT)]);
    }

    #[test]
    fn a_paste_is_one_event() {
        assert_eq!(
            all(b"\x1b[200~let x =\n  1\x1b[201~z"),
            vec![(PASTE, "let x =\n  1".into(), 0), ch("z", 0)]
        );
        assert_eq!(decode(b"\x1b[200~half", false), None);
    }
}
