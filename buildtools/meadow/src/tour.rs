//! A guided tour of the language, taken at the prompt.
//!
//! `:tour` shows a step and then gets out of the way: the prompt afterwards is
//! the ordinary one, so anything can be typed, run and undone before `:next`
//! moves on. That is the whole design. A tutorial you read teaches you what
//! someone else typed; this one is the thing itself, with the explanation
//! beside it.
//!
//! Each step carries an example, and `:try` puts it *on the prompt* rather than
//! running it -- so it can be read, edited and then run, which is how anyone
//! learns what a piece of code does.

/// Which part of the tour a step belongs to.
///
/// The basics are what `:tour` walks by default; the rest is there for when
/// someone wants it. A tour that insists on showing you transactional memory
/// before you have written a function is not a tour, it is a manual.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Section {
    /// The language: enough to write something.
    Basics,
    /// Threads, transactions, mutation, failure -- what the language has that
    /// is worth a section of its own.
    Advanced,
}

impl Section {
    pub fn title(self) -> &'static str {
        match self {
            Section::Basics => "the basics",
            Section::Advanced => "going further",
        }
    }

    /// What `:tour basics` and `:tour advanced` answer to.
    pub fn named(word: &str) -> Option<Section> {
        match word {
            "basics" | "basic" | "1" => Some(Section::Basics),
            "advanced" | "more" | "2" => Some(Section::Advanced),
            _ => None,
        }
    }

    /// Where this section starts, if it has any steps.
    pub fn first(self) -> Option<usize> {
        STEPS.iter().position(|s| s.section == self)
    }
}

/// One step: what it is about, what it says, and what to type.
pub struct Step {
    pub title: &'static str,
    pub section: Section,
    /// Lines, already wrapped: a terminal is not always wide, and a paragraph
    /// rewrapped by the terminal reads worse than one broken where it means to
    /// break.
    pub text: &'static [&'static str],
    /// What to type, in order -- `:try` offers the next one each time.
    ///
    /// A sequence rather than a single example, because a definition on its
    /// own only ever answers `<closure>`. A step that defines something and
    /// then *runs* it shows what it does, which is the whole point of taking
    /// the tour at a prompt instead of reading it.
    pub code: &'static [&'static str],
}

/// What a line of a step is, which decides how the test below compiles it.
pub enum Line {
    /// A `:` command -- the REPL's, not the language's.
    Command,
    /// Something that adds a name: the steps after it may use it.
    Decl,
    /// Something with a value, which is what a step ends on.
    Expr,
}

/// What `line` is. Case carries meaning here as everywhere: a declaration
/// starts with the word that declares it.
pub fn kind_of(line: &str) -> Line {
    let word = line.trim_start();
    if word.starts_with(':') {
        Line::Command
    } else if [
        "use ", "def ", "fun ", "data ", "record ", "effect ", "type ", "mod ", "@",
    ]
    .iter()
    .any(|w| word.starts_with(w))
    {
        Line::Decl
    } else {
        Line::Expr
    }
}

pub const STEPS: &[Step] = &[
    Step {
        title: "Expressions",
        section: Section::Basics,
        text: &[
            "Everything here is an expression, and the prompt evaluates one.",
            "There are no statements and nothing to end a line with.",
            "",
            "`:t` answers with a type instead of a value, which is the quickest",
            "way to ask what something is.",
        ],
        code: &[
            "1 + 2 * 3",
            ":t 1 + 2 * 3",
            "\"hello\" ++ \", \" ++ \"world\"",
        ],
    },
    Step {
        title: "Definitions",
        section: Section::Basics,
        text: &[
            "`def` names a value and `fun` names a function. Application is",
            "juxtaposition -- `f x`, no brackets -- so brackets are only ever",
            "for grouping.",
            "",
            "Types are inferred. Writing one down is allowed and never needed.",
        ],
        code: &["fun double n = n * 2", "double 21", "map double [1, 2, 3]"],
    },
    Step {
        title: "Data types",
        section: Section::Basics,
        text: &[
            "A `data` declaration lists what something can be. A constructor",
            "belongs to its type, so it is `Shape.Circle` unless you bring it",
            "into scope with `use Shape.*`.",
            "",
            "The last line makes one and shows it.",
        ],
        code: &[
            "data Shape = Circle Int | Rect Int Int",
            "use Shape.*",
            "Rect 3 4",
        ],
    },
    Step {
        title: "Matching",
        section: Section::Basics,
        text: &[
            "One `match`, one arm per case. The compiler checks the arms cover",
            "the type -- leave one out and it says which.",
            "",
            "Try deleting an arm from the definition and see.",
        ],
        code: &[
            "fun area s = match s with | Circle r -> 3 * r * r | Rect w h -> w * h",
            "area (Rect 3 4)",
            "map area [Circle 1, Rect 3 4]",
        ],
    },
    Step {
        title: "Collections and the pipe",
        section: Section::Basics,
        text: &[
            "`[1, 2, 3]` is a vector: indexed, sliced and appended cheaply.",
            "`[1; 2; 3]` is a list, which is a chain of conses.",
            "",
            "`|>` passes a value to a function, so a pipeline reads in the",
            "order it happens.",
        ],
        code: &[
            "[1..10]",
            "[1..10] |> filter (\\n -> n % 2 == 0)",
            "[1..10] |> filter (\\n -> n % 2 == 0) |> map double |> sum",
        ],
    },
    Step {
        title: "Records",
        section: Section::Basics,
        text: &[
            "A record is written `{ name = \"ada\", age = 36 }` and read with a",
            "dot. Its type is its fields, so nothing has to be declared first.",
            "",
            "`{ r | age = 37 }` is `r` with one field changed -- a new record,",
            "not a mutation, as the third line shows.",
        ],
        code: &[
            "def ada = { name = \"ada\", age = 36 }",
            "{ ada | age = 37 }",
            "(ada.age, ada.name)",
        ],
    },
    Step {
        title: "Nothing, and what went wrong",
        section: Section::Basics,
        text: &[
            "`Maybe a` is a value or `None`. `Result e a` is a value or an",
            "error -- the error first, so `Result String Int` is a number or a",
            "complaint about one.",
            "",
            "Both are ordinary data types, and `match` takes them apart like",
            "anything else.",
        ],
        code: &[
            "find (\\n -> n > 2) [1, 2, 3]",
            "find (\\n -> n > 9) [1, 2, 3]",
            "match find (\\n -> n > 2) [1, 2, 3] with | Just n -> n | None -> 0",
        ],
    },
    Step {
        title: "Effects",
        section: Section::Basics,
        text: &[
            "What a function *does* is in its type: `! { Console }` means it",
            "prints. A function that performs nothing says nothing.",
            "",
            "`:t` on the second line is the interesting part -- the effect is",
            "there in the type, not in a comment.",
        ],
        code: &[
            "fun greet name = println \"hello ${name}\"",
            ":t greet",
            "greet \"ada\"",
        ],
    },
    Step {
        title: "Modules",
        section: Section::Basics,
        text: &[
            "`use` brings names in: `use Std.Collections.List (fromVector)`",
            "takes one, `use Std.Maybe as M` puts a module behind `M.`, and a",
            "bare `use` takes everything it exports.",
            "",
            "`:module` lists what you have defined here.",
        ],
        code: &[
            "use Std.String as S",
            "S.join \", \" [\"a\", \"b\", \"c\"]",
            "S.toUpper \"shout\"",
        ],
    },
    Step {
        title: "Where to go next",
        section: Section::Basics,
        text: &[
            "`meadow init myApp` writes a package you can build and run, and",
            "`meadow test` runs the `@test` functions in it.",
            "",
            "docs/TUTORIAL.md is the long form of all of this.",
            "",
            "That is the language. `:tour advanced` carries on with what it",
            "does about threads, shared state, mutation and failure -- worth",
            "reading when you want them, and not before.",
        ],
        code: &[":module"],
    },
    // --- going further -------------------------------------------------------
    Step {
        title: "Threads",
        section: Section::Advanced,
        text: &[
            "`Thread.spawn` starts a thread and hands back something to",
            "`await`. A thread is a value like any other, so a vector of them",
            "is a vector of work in flight, and `awaitAll` collects it.",
            "",
            "`Thread.both` is the two-thing case, which is most of them.",
            "",
            "`fib` is here to be slow on purpose: the steps after this one need",
            "something that actually takes a moment.",
        ],
        code: &[
            "use Std.Thread as Thread",
            "fun fib (n : Int) = if n < 2 then n else fib (n - 1) + fib (n - 2)",
            "Thread.await (Thread.spawn (\\() -> fib 20))",
            "Thread.both (\\() -> fib 24) (\\() -> fib 25)",
            "fun inFlight () = Thread.awaitAll (map (\\n -> Thread.spawn (\\() -> fib n)) [24, 25, 26])",
            "inFlight ()",
        ],
    },
    Step {
        title: "Parallelism",
        section: Section::Advanced,
        text: &[
            "Concurrency is waiting on several things; parallelism is doing",
            "several at once. `Thread.parMap` is the second: a thread per",
            "element, awaited together.",
            "",
            "Eight of these take about seven hundred milliseconds one after",
            "another. Spread across the cores they take a fraction of that --",
            "how large a fraction is how many cores you have. Same answer",
            "either way.",
            "",
            "Run each `:time` twice; the first pays for compiling.",
        ],
        code: &[
            "def work : [Int] = [31, 31, 31, 31, 31, 31, 31, 31]",
            ":time map fib work",
            ":time Thread.parMap fib work",
        ],
    },
    Step {
        title: "When it does not help",
        section: Section::Advanced,
        text: &[
            "A thread costs something to start, so parallelism pays only when",
            "each piece of work is real. One multiplication is not.",
            "",
            "`:time` both of these. Twenty thousand threads that each double a",
            "number take half again as long as doing it in one -- all of it",
            "spent starting and awaiting them.",
            "",
            "Worth seeing once, so the reach for `parMap` is a decision rather",
            "than a habit: split the work until each piece is worth a thread,",
            "and no further.",
        ],
        code: &[
            "fun tiny () = [1..20000]",
            ":time sum (map double (tiny ()))",
            ":time sum (Thread.parMap double (tiny ()))",
        ],
    },
    Step {
        title: "Channels",
        section: Section::Advanced,
        text: &[
            "A channel carries values between threads: `send` puts one in and",
            "`receive` takes one out, waiting if there is nothing yet.",
            "",
            "Which is how work is handed to a pool of threads and results are",
            "gathered back -- fan out, fan in, with nothing shared. Note what",
            "`gather` answers: values leave the channel in the order they were",
            "finished, not the order they were asked for, and `fib 10` beats",
            "`fib 30` home.",
        ],
        code: &[
            "fun handOff n = let ch = Thread.newChannel () in let _ = Thread.spawn (\\() -> Thread.send ch (fib n)) in Thread.receive ch",
            "handOff 20",
            "fun gather ns = let ch = Thread.newChannel () in let _ = map (\\n -> Thread.spawn (\\() -> Thread.send ch (fib n))) ns in map (\\_ -> Thread.receive ch) ns",
            "gather [30, 20, 10]",
        ],
    },
    Step {
        title: "Shared state, without locks",
        section: Section::Advanced,
        text: &[
            "Threads share nothing that changes -- except a `TVar`, which only",
            "changes inside `Stm.atomically`. A transaction reads and writes as",
            "if it were alone; if another commits something it read, it quietly",
            "runs again. Nothing observes it half done.",
            "",
            "Two threads each add fifty, and the total is right whichever order",
            "they land in. Then five hundred threads add to the same `TVar` at",
            "once, which is where a program with locks starts losing updates:",
            "the answer is 125250 every time, because it is the only answer a",
            "transaction can commit.",
        ],
        code: &[
            "use Std.Stm as Stm",
            "fun deposits () = let account = Stm.newTVarIO 100 in let _ = Thread.parMap (\\n -> Stm.atomically (\\() -> Stm.modifyTVar account (\\b -> b + n))) [50, 50] in Stm.atomically (\\() -> Stm.readTVar account)",
            "deposits ()",
            "fun contended () = let total = Stm.newTVarIO 0 in let _ = Thread.parMap (\\n -> Stm.atomically (\\() -> Stm.modifyTVar total (\\b -> b + n))) [1..500] in Stm.atomically (\\() -> Stm.readTVar total)",
            "contended ()",
            "contended ()",
        ],
    },
    Step {
        title: "Waiting for a condition",
        section: Section::Advanced,
        text: &[
            "`Stm.check` holds a transaction until what it read changes. A",
            "transfer that needs funds simply sleeps until the money is there,",
            "and nothing has to poll or hold a lock.",
            "",
            "The withdrawal below waits; the deposit releases it.",
        ],
        code: &[
            "fun waited () = let account = Stm.newTVarIO 100 in let pending = Thread.spawn (\\() -> Stm.atomically (\\() -> let _ = Stm.check (Stm.readTVar account >= 500) in Stm.modifyTVar account (\\b -> b - 500))) in let _ = Stm.atomically (\\() -> Stm.modifyTVar account (\\b -> b + 400)) in let _ = Thread.await pending in Stm.atomically (\\() -> Stm.readTVar account)",
            "waited ()",
        ],
    },
    Step {
        title: "Mutation that cannot escape",
        section: Section::Advanced,
        text: &[
            "`runSt` is a region where cells and arrays are written in place.",
            "Nothing made inside one can leave it -- the checker gives each",
            "`runSt` a state type of its own and refuses anything mentioning it",
            "back out.",
            "",
            "So `sumTo` is pure, whatever it did inside: its type says `Int`,",
            "not `Int ! { Mut }`.",
        ],
        code: &[
            "use Std.St as St",
            "fun sumTo n = runSt (\\() -> let total = St.newRef 0 in let _ = St.forRange 1 (n + 1) (\\i -> St.modifyRef total (\\t -> t + i)) in St.getRef total)",
            "sumTo 100",
            ":t sumTo",
        ],
    },
    Step {
        title: "Failure that travels",
        section: Section::Advanced,
        text: &[
            "`Result` is right when the caller should look at the failure.",
            "`Exn` is right when it should travel a long way untouched: `raise`",
            "unwinds to the nearest handler, and nothing in between mentions",
            "it.",
            "",
            "`toResult` converts at the boundary, so the two meet where you",
            "decide they do.",
        ],
        code: &[
            "use Std.Exn as Exn",
            "fun half n = if n % 2 == 0 then n / 2 else Exn.raise \"odd\"",
            "Exn.toResult (\\() -> half 8)",
            "Exn.toResult (\\() -> half 7)",
        ],
    },
    Step {
        title: "Effects of your own",
        section: Section::Advanced,
        text: &[
            "`effect` declares operations, calling one performs it, and",
            "`handle` decides what it means. Everything above is built this way",
            "-- `Exn`, `St`, `Stm` are effects, not compiler magic.",
            "",
            "The same `asked` runs twice below and answers differently, because",
            "the handler is what decides.",
        ],
        code: &[
            "effect Ask { ask : () -> Int }",
            "fun asked () = ask () + ask ()",
            "handle asked () with { ask u k -> k 21, return x -> x }",
            "handle asked () with { ask u k -> k 100, return x -> x }",
        ],
    },
    Step {
        title: "That is the tour",
        section: Section::Advanced,
        text: &[
            "`examples/` has a package for each of these -- `Stm`,",
            "`Concurrency`, `Parallel`, `Effects` -- written to be read and",
            "run.",
            "",
            "docs/RUNTIME.md explains what runs underneath: the bytecode VM,",
            "the JIT, and the collector.",
        ],
        code: &[":module"],
    },
];

/// Where a step is in the tour, for its heading.
pub fn heading(at: usize) -> String {
    format!("Step {} of {} — {}", at + 1, STEPS.len(), STEPS[at].title)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every line of every step, compiled in the order the tour gives them --
    /// which is the order someone taking it would type them, so a step may
    /// rely on what came before.
    ///
    /// A tour whose examples do not run is worse than no tour: the first thing
    /// it teaches is that the thing in front of you is broken.
    #[test]
    fn every_line_runs() {
        let mut decls = String::new();
        for (i, step) in STEPS.iter().enumerate() {
            for line in step.code {
                let source = match kind_of(line) {
                    // A `:` command is the REPL's, and there is nothing here
                    // that could compile it.
                    Line::Command => continue,
                    Line::Decl => format!("{decls}{line}\n\ndef main = 1\n"),
                    Line::Expr => format!("{decls}def main = {line}\n"),
                };
                let (_, diags) =
                    crate::pipeline::compile_str_with_std("tour", &source, crate::Options::debug());
                assert!(
                    diags.is_empty(),
                    "step {} ({}): `{line}` does not compile: {:?}",
                    i + 1,
                    step.title,
                    diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
                );
                if matches!(kind_of(line), Line::Decl) {
                    decls.push_str(line);
                    decls.push_str("\n\n");
                }
            }
        }
    }

    /// Every step has something to say and something to type, and ends on
    /// something with a value: a step that only defines things answers
    /// `<closure>`, which teaches nobody anything.
    #[test]
    fn every_step_ends_on_something_to_look_at() {
        for (i, step) in STEPS.iter().enumerate() {
            assert!(!step.title.is_empty(), "step {i} has no title");
            assert!(!step.text.is_empty(), "step {i} says nothing");
            assert!(!step.code.is_empty(), "step {i} has nothing to type");
            let last = step.code[step.code.len() - 1];
            assert!(
                !matches!(kind_of(last), Line::Decl),
                "step {} ({}) ends on a definition: {last}",
                i + 1,
                step.title
            );
        }
    }

    /// A line that does not fit an eighty-column terminal is wrapped by the
    /// terminal, wherever that lands.
    #[test]
    fn nothing_is_too_wide_for_a_narrow_terminal() {
        for step in STEPS {
            for line in step.text {
                assert!(line.len() <= 72, "too wide, and it will wrap: {line:?}");
            }
        }
    }

    /// The basics come first and are not interleaved with what follows: a
    /// section is a stretch of the tour, which is what makes `:tour advanced`
    /// mean something.
    #[test]
    fn each_section_is_one_stretch() {
        let mut seen = Vec::new();
        for step in STEPS {
            if seen.last() != Some(&step.section) {
                assert!(
                    !seen.contains(&step.section),
                    "{} is in two places",
                    step.section.title()
                );
                seen.push(step.section);
            }
        }
        assert_eq!(
            seen.first(),
            Some(&Section::Basics),
            "the basics come first"
        );
        assert_eq!(seen.len(), 2, "two sections");
    }

    #[test]
    fn a_section_can_be_asked_for_by_name() {
        assert_eq!(Section::named("advanced"), Some(Section::Advanced));
        assert_eq!(Section::named("basics"), Some(Section::Basics));
        assert_eq!(Section::named("sideways"), None);
        assert!(Section::Advanced.first().is_some(), "and it has steps");
    }

    #[test]
    fn a_line_is_read_for_what_it_is() {
        assert!(matches!(kind_of(":module"), Line::Command));
        assert!(matches!(kind_of("fun f x = x"), Line::Decl));
        assert!(matches!(kind_of("use Std.String as S"), Line::Decl));
        assert!(matches!(kind_of("@pub def x = 1"), Line::Decl));
        assert!(matches!(kind_of("1 + 1"), Line::Expr));
        assert!(matches!(kind_of("map double [1]"), Line::Expr));
    }

    #[test]
    fn the_heading_says_where_you_are() {
        assert!(heading(0).starts_with("Step 1 of "));
        assert!(heading(STEPS.len() - 1).contains(&format!("of {}", STEPS.len())));
    }
}
