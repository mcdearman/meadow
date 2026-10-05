//! **A document, and how it is laid out within a width** -- the half of the
//! formatter that knows nothing of Meadow.
//!
//! This is Wadler's pretty printer as Prettier has it. A [`Doc`] says what
//! there is to print and where a line *may* end; [`print`] decides where
//! each does. The rule is one: a [`Doc::Group`] goes on one line if it fits
//! in what is left of the width, and otherwise every [`Doc::Line`] directly
//! in it is a new line. Groups nest, and the outer one breaks first, so a
//! thing is cut at its outermost joint before any inner one.
//!
//! What comes out depends on the document and the width and nothing else,
//! which is the point of it: the document is built from a file's tokens, so
//! two files with the same tokens are printed the same.

use std::collections::HashMap;

pub enum Doc {
    /// Printed as it is. A string literal that runs over several lines is
    /// one of these with the newlines in it, and they are not indented.
    Text(String),
    /// A space, or a new line where its group does not fit.
    Line,
    /// Nothing, or a new line where its group does not fit.
    Soft,
    /// A new line, which makes every group around it one that does not fit.
    Hard,
    /// An empty line and then a new one.
    Blank,
    Cat(Vec<Doc>),
    /// What is inside is indented this much further on the lines it starts.
    Nest(usize, Box<Doc>),
    /// What is inside is indented to the column it starts in.
    Align(Box<Doc>),
    Group {
        /// A name to ask after with [`Doc::IfBreak`].
        id: Option<u32>,
        /// Laid out over lines whether or not it fits.
        broken: bool,
        doc: Box<Doc>,
    },
    /// One thing where the group named was laid out over lines, another
    /// where it was not, or has not been laid out yet.
    IfBreak {
        id: u32,
        broken: Box<Doc>,
        flat: Box<Doc>,
    },
    /// A thing and the last of what it is applied to: `f a b` and `(\x ->
    /// …)`. On one line if that fits. Otherwise, where `hug` allows it and
    /// the first line of `last` fits after `init`, `init` stays on its line
    /// and `last` is laid out from the end of it; and otherwise `last` goes
    /// on a line of its own, `indent` in.
    Hug {
        init: Box<Doc>,
        last: Box<Doc>,
        hug: bool,
        indent: usize,
        /// A comment in `init` ends its line, so `last` cannot be on it:
        /// [`Doc::settle`] finds that out.
        split: bool,
    },
    /// A space and then this, where this fits on the line whole; a new
    /// line and then this where it does not. A run of these is things run
    /// on, as many to a line as fit.
    Fit(Box<Doc>),
    /// A comment that ends the line it is on: written where the line ends,
    /// whatever else is put on the line after it.
    Suffix(String),
}

pub fn text(s: impl Into<String>) -> Doc {
    Doc::Text(s.into())
}

pub fn cat(docs: Vec<Doc>) -> Doc {
    Doc::Cat(docs)
}

pub fn nest(n: usize, doc: Doc) -> Doc {
    Doc::Nest(n, Box::new(doc))
}

pub fn align(doc: Doc) -> Doc {
    Doc::Align(Box::new(doc))
}

pub fn group(doc: Doc) -> Doc {
    Doc::Group {
        id: None,
        broken: false,
        doc: Box::new(doc),
    }
}

/// A group that is laid out over lines whether or not it fits.
pub fn broken(doc: Doc) -> Doc {
    Doc::Group {
        id: None,
        broken: true,
        doc: Box::new(doc),
    }
}

/// A group that is one line only where that line is short: `doc` as a
/// [`group`] if it is no more than `limit` columns on one line, and laid
/// out over lines otherwise, however much room there is.
///
/// For what reads as a line only while it is small -- a `match` and two
/// arms, a `let` and its body -- as rustfmt has a width for an `if` and its
/// `else` on one line.
pub fn small(doc: Doc, limit: usize) -> Doc {
    if doc.flat_width().is_some_and(|w| w <= limit) {
        group(doc)
    } else {
        broken(doc)
    }
}

impl Doc {
    /// How wide this is on one line, if it can be on one.
    pub fn flat_width(&self) -> Option<usize> {
        Some(match self {
            Doc::Text(s) if s.contains('\n') => return None,
            Doc::Text(s) => s.chars().count(),
            Doc::Line => 1,
            Doc::Soft => 0,
            Doc::Hard | Doc::Blank | Doc::Suffix(_) => return None,
            Doc::Cat(docs) => {
                let mut sum = 0;
                for d in docs {
                    sum += d.flat_width()?;
                }
                sum
            }
            Doc::Nest(_, d) | Doc::Align(d) => d.flat_width()?,
            Doc::Fit(d) => 1 + d.flat_width()?,
            Doc::Group { broken: true, .. } => return None,
            Doc::Group { doc, .. } => doc.flat_width()?,
            Doc::IfBreak { flat, .. } => flat.flat_width()?,
            Doc::Hug { init, last, .. } => init.flat_width()? + 1 + last.flat_width()?,
        })
    }

    /// Mark every group that cannot be one line as one that does not fit:
    /// one with a [`Doc::Hard`] in it, and one with a comment in it and
    /// something after the comment, which a line has to end between.
    ///
    /// A comment with nothing after it in a group forces nothing there: the
    /// group ends with it, and where the line ends is for what is around
    /// the group to say.
    pub fn settle(&mut self) {
        self.settled(&mut 0);
    }

    fn settled(&mut self, at: &mut usize) -> Settled {
        *at += 1;
        let here = *at;
        match self {
            Doc::Text(s) => Settled {
                last_text: (!s.is_empty()).then_some(here),
                ..Settled::default()
            },
            Doc::Line | Doc::Soft => Settled::default(),
            Doc::Hard | Doc::Blank => Settled {
                hard: true,
                ..Settled::default()
            },
            Doc::Suffix(_) => Settled {
                first_suffix: Some(here),
                ..Settled::default()
            },
            Doc::Cat(docs) => docs
                .iter_mut()
                .fold(Settled::default(), |all, d| all.then(d.settled(at))),
            Doc::Nest(_, d) | Doc::Align(d) | Doc::Fit(d) => d.settled(at),
            Doc::Group { broken, doc, .. } => {
                let inside = doc.settled(at);
                *broken |= inside.hard || inside.cut();
                Settled {
                    hard: *broken,
                    ..inside
                }
            }
            // Which side is printed is not known yet, and neither forces
            // the group around it: the group it asks after already did.
            Doc::IfBreak { broken, flat, .. } => {
                let (a, b) = (broken.settled(at), flat.settled(at));
                Settled {
                    hard: false,
                    ..a.then(b)
                }
            }
            Doc::Hug {
                init, last, split, ..
            } => {
                let (a, b) = (init.settled(at), last.settled(at));
                *split = a.first_suffix.is_some() && b.last_text.is_some();
                a.then(b)
            }
        }
    }

    /// Whether this must be laid out over lines: see [`Doc::settle`], which
    /// has to have run.
    fn must_break(&self) -> bool {
        match self {
            Doc::Text(_) | Doc::Line | Doc::Soft | Doc::IfBreak { .. } | Doc::Suffix(_) => false,
            Doc::Hard | Doc::Blank => true,
            Doc::Cat(docs) => docs.iter().any(Doc::must_break),
            Doc::Nest(_, d) | Doc::Align(d) | Doc::Fit(d) => d.must_break(),
            Doc::Group { broken, .. } => *broken,
            Doc::Hug {
                init, last, split, ..
            } => *split || init.must_break() || last.must_break(),
        }
    }
}

/// What [`Doc::settle`] found in a document: whether a line must end in it,
/// and where its first comment and its last text are, counted in the order
/// they are printed.
#[derive(Default, Clone, Copy)]
struct Settled {
    hard: bool,
    first_suffix: Option<usize>,
    last_text: Option<usize>,
}

impl Settled {
    /// This and then `next`.
    fn then(self, next: Settled) -> Settled {
        Settled {
            hard: self.hard || next.hard,
            first_suffix: self.first_suffix.or(next.first_suffix),
            last_text: next.last_text.or(self.last_text),
        }
    }

    /// Whether a comment in it has text after it.
    fn cut(&self) -> bool {
        matches!((self.first_suffix, self.last_text), (Some(c), Some(t)) if c < t)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Flat,
    Break,
}

type Cmd<'a> = (usize, Mode, &'a Doc);

struct Printer<'a> {
    width: usize,
    out: String,
    /// The column the next character goes in.
    col: usize,
    /// The indent owed to the line just started, written when something is
    /// put on it: an empty line has no spaces on it.
    owed: Option<usize>,
    /// Comments waiting for the line they are on to end.
    suffixes: Vec<&'a str>,
    /// How each named group was laid out.
    modes: HashMap<u32, Mode>,
}

/// `doc` laid out within `width` columns, where there is a way to.
pub fn print(doc: &mut Doc, width: usize) -> String {
    doc.settle();
    let mut p = Printer {
        width,
        out: String::new(),
        col: 0,
        owed: None,
        suffixes: Vec::new(),
        modes: HashMap::new(),
    };
    let mut cmds: Vec<Cmd<'_>> = vec![(0, Mode::Break, doc)];
    while let Some((indent, mode, doc)) = cmds.pop() {
        match doc {
            Doc::Text(s) => p.write(s),
            Doc::Line | Doc::Soft => match mode {
                Mode::Flat => {
                    if matches!(doc, Doc::Line) {
                        p.write(" ");
                    }
                }
                Mode::Break => p.newline(indent),
            },
            Doc::Hard => p.newline(indent),
            Doc::Blank => {
                p.newline(indent);
                p.newline(indent);
            }
            Doc::Cat(docs) => cmds.extend(docs.iter().rev().map(|d| (indent, mode, d))),
            Doc::Nest(n, d) => cmds.push((indent + n, mode, d)),
            Doc::Align(d) => cmds.push((p.column(), mode, d)),
            Doc::Fit(d) => {
                static SPACE: Doc = Doc::Line;
                let fits = mode == Mode::Flat
                    || (!d.must_break()
                        && p.fits(
                            &[(indent, Mode::Flat, d), (indent, Mode::Flat, &SPACE)],
                            &cmds,
                            false,
                        ));
                if fits {
                    p.write(" ");
                } else {
                    p.newline(indent);
                }
                cmds.push((indent, mode, d));
            }
            Doc::Group { id, broken, doc } => {
                let chosen = if mode == Mode::Flat && !*broken {
                    Mode::Flat
                } else if !*broken && p.fits(&[(indent, Mode::Flat, doc)], &cmds, false) {
                    Mode::Flat
                } else {
                    Mode::Break
                };
                if let Some(id) = id {
                    p.modes.insert(*id, chosen);
                }
                cmds.push((indent, chosen, doc));
            }
            Doc::IfBreak { id, broken, flat } => {
                let side = match p.modes.get(id) {
                    Some(Mode::Break) => broken,
                    _ => flat,
                };
                cmds.push((indent, mode, side));
            }
            Doc::Hug {
                init,
                last,
                hug,
                indent: by,
                split,
            } => {
                static SPACE: Doc = Doc::Line;
                let whole = !*split
                    && !init.must_break()
                    && !last.must_break()
                    && (mode == Mode::Flat
                        || p.fits(
                            &[
                                (indent, Mode::Flat, last),
                                (indent, Mode::Flat, &SPACE),
                                (indent, Mode::Flat, init),
                            ],
                            &cmds,
                            false,
                        ));
                if whole {
                    cmds.push((indent, Mode::Flat, last));
                    cmds.push((indent, Mode::Flat, &SPACE));
                    cmds.push((indent, Mode::Flat, init));
                } else if *hug
                    && !*split
                    && !init.must_break()
                    && p.fits(
                        &[
                            (indent, Mode::Break, last),
                            (indent, Mode::Flat, &SPACE),
                            (indent, Mode::Flat, init),
                        ],
                        &cmds,
                        true,
                    )
                {
                    cmds.push((indent, Mode::Break, last));
                    cmds.push((indent, Mode::Flat, &SPACE));
                    cmds.push((indent, Mode::Flat, init));
                } else {
                    cmds.push((indent + by, Mode::Break, last));
                    cmds.push((indent + by, Mode::Break, &SPACE));
                    cmds.push((indent, Mode::Break, init));
                }
            }
            Doc::Suffix(s) => p.suffixes.push(s),
        }
    }
    p.end_line();
    // One newline at the end, and no empty lines before it or at the start.
    let body = p.out.trim_matches('\n');
    if body.is_empty() {
        String::new()
    } else {
        format!("{body}\n")
    }
}

impl<'a> Printer<'a> {
    fn column(&self) -> usize {
        self.owed.unwrap_or(self.col)
    }

    fn write(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if let Some(indent) = self.owed.take() {
            self.out.extend(std::iter::repeat_n(' ', indent));
            self.col = indent;
        }
        self.out.push_str(s);
        self.col = match s.rfind('\n') {
            Some(at) => s[at + 1..].chars().count(),
            None => self.col + s.chars().count(),
        };
    }

    /// The comments that end this line, and the spaces nothing follows.
    fn end_line(&mut self) {
        let suffixes = std::mem::take(&mut self.suffixes);
        for s in suffixes {
            if self.owed.is_some() || self.out.is_empty() || self.out.ends_with('\n') {
                self.write(s);
            } else {
                let kept = self.out.trim_end_matches(' ').len();
                self.out.truncate(kept);
                self.out.push(' ');
                self.out.push_str(s);
            }
        }
        let kept = self.out.trim_end_matches([' ', '\t']).len();
        self.out.truncate(kept);
    }

    fn newline(&mut self, indent: usize) {
        self.end_line();
        self.out.push('\n');
        self.col = 0;
        self.owed = Some(indent);
    }

    /// Whether `next`, and then what of `rest` is on the same line as its
    /// end, fits in what is left of this line. `next` is given last first,
    /// as `rest` is.
    ///
    /// With `first_line`, a line that ends inside `next` because its group
    /// is laid out over lines counts as fitting: what is asked is whether
    /// the line as far as that fits.
    fn fits(&self, next: &[Cmd<'a>], rest: &[Cmd<'a>], first_line: bool) -> bool {
        let mut left = self.width as isize - self.column() as isize;
        let mut rest_at = rest.len();
        let mut cmds: Vec<Cmd<'a>> = next.to_vec();
        loop {
            if left < 0 {
                return false;
            }
            let (indent, mode, doc) = match cmds.pop() {
                Some(cmd) => cmd,
                None => {
                    if rest_at == 0 {
                        return true;
                    }
                    rest_at -= 1;
                    rest[rest_at]
                }
            };
            match doc {
                Doc::Text(s) => match s.find('\n') {
                    Some(at) => return left >= s[..at].chars().count() as isize,
                    None => left -= s.chars().count() as isize,
                },
                Doc::Line | Doc::Soft => match mode {
                    Mode::Flat => {
                        if matches!(doc, Doc::Line) {
                            left -= 1;
                        }
                    }
                    Mode::Break => return true,
                },
                Doc::Hard | Doc::Blank => return true,
                Doc::Cat(docs) => cmds.extend(docs.iter().rev().map(|d| (indent, mode, d))),
                Doc::Nest(_, d) | Doc::Align(d) => cmds.push((indent, mode, d)),
                Doc::Fit(d) => match mode {
                    Mode::Flat => {
                        left -= 1;
                        cmds.push((indent, mode, d));
                    }
                    Mode::Break => return true,
                },
                Doc::Group { broken, doc, .. } => {
                    if *broken && mode == Mode::Flat && !first_line {
                        return false;
                    }
                    let mode = if *broken { Mode::Break } else { mode };
                    cmds.push((indent, mode, doc));
                }
                Doc::IfBreak { id, broken, flat } => {
                    let side = match self.modes.get(id) {
                        Some(Mode::Break) => broken,
                        _ => flat,
                    };
                    cmds.push((indent, mode, side));
                }
                Doc::Hug {
                    init, last, hug, ..
                } => {
                    static SPACE: Doc = Doc::Line;
                    match mode {
                        Mode::Flat => {
                            cmds.push((indent, Mode::Flat, last));
                            cmds.push((indent, Mode::Flat, &SPACE));
                            cmds.push((indent, Mode::Flat, init));
                        }
                        // As it would be printed there: its last part from
                        // the end of the line if it may be, and otherwise a
                        // line ends before it.
                        Mode::Break => {
                            if *hug {
                                cmds.push((indent, Mode::Break, last));
                                cmds.push((indent, Mode::Flat, &SPACE));
                                cmds.push((indent, Mode::Flat, init));
                            } else {
                                cmds.push((indent, Mode::Break, &SPACE));
                                cmds.push((indent, Mode::Break, init));
                            }
                        }
                    }
                }
                Doc::Suffix(_) => {}
            }
        }
    }
}
