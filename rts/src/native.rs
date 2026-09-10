//! What an *unhandled* effect operation means.
//!
//! `Std.Fs`, `Std.Process`, `Std.Random` and `Std.Time` each pair an effect
//! declaration with a handler that fakes it. Reaching one with no handler
//! installed is a request for the real thing, and this is where the runtime
//! provides it — the same set, and the same answers, as
//! `meadow_eval`'s natives.
//!
//! `Std.Test.fail` is the other unhandled operation with a meaning, and it lives
//! in the VM proper because it is a failure rather than a value.
//!
//! # Building results
//!
//! The awkward part of porting these was allocation. A native answer is a tree —
//! `Ok(Just(#[1, 2, 3]))`, a record of file metadata, a list of directory names
//! — and building it one object at a time would mean holding an address across
//! an allocation, which a copying collector makes wrong.
//!
//! So a native describes what it wants as a [`Build`], which is an ordinary Rust
//! value with no addresses in it. [`Vm::build`] then measures the whole tree,
//! makes room once, and materialises it bottom-up with no collection possible in
//! between. The natives end up reading almost exactly like the CEK's.

use crate::heap::Kind;
use crate::value::Value;
use crate::vm::{err, Error, Vm};
use meadow_intern::InternedString;

/// A result to build, described before any of it exists.
///
/// Deliberately contains no [`Value::Obj`]: the whole point is that nothing here
/// can be invalidated by the collection that making room for it may trigger.
/// Immediates ([`Build::At`]) are fine, since they are not addresses.
pub enum Build {
    At(Value),
    Str(String),
    Data(&'static str, Vec<Build>),
    Tuple(Vec<Build>),
    Record(Vec<(&'static str, Build)>),
    /// A `Cons`/`Nil` chain, in order.
    List(Vec<Build>),
}

impl Build {
    fn unit() -> Build {
        Build::At(Value::Unit)
    }

    fn int(n: i64) -> Build {
        Build::At(Value::Int(n))
    }

    fn ok(v: Build) -> Build {
        Build::Data("Ok", vec![v])
    }

    fn error(msg: String) -> Build {
        Build::Data("Err", vec![Build::Str(msg)])
    }

    /// How many heap slots the whole tree needs.
    fn slots(&self) -> usize {
        match self {
            Build::At(_) | Build::Str(_) => 0,
            Build::Data(_, xs) | Build::Tuple(xs) => {
                1 + xs.len() + xs.iter().map(Build::slots).sum::<usize>()
            }
            Build::Record(fs) => {
                1 + 2 * fs.len() + fs.iter().map(|(_, b)| b.slots()).sum::<usize>()
            }
            // One `Cons` of two fields per element, plus the `Nil`.
            Build::List(xs) => 1 + 3 * xs.len() + xs.iter().map(Build::slots).sum::<usize>(),
        }
    }
}

impl Vm<'_> {
    /// Materialise a [`Build`]. Makes room once, then allocates with no
    /// collection possible in between — which is what lets it hold addresses.
    pub(crate) fn build(&mut self, b: Build) -> Value {
        self.ensure(b.slots());
        self.build_here(b)
    }

    fn build_here(&mut self, b: Build) -> Value {
        match b {
            Build::At(v) => v,
            Build::Str(s) => Value::Str(InternedString::from(s)),
            Build::Data(name, xs) => {
                let fields: Vec<Value> = xs.into_iter().map(|x| self.build_here(x)).collect();
                let tag = self.ctor_tag(name);
                Value::Obj(self.heap.alloc(Kind::Data, tag, &fields))
            }
            Build::Tuple(xs) => {
                let fields: Vec<Value> = xs.into_iter().map(|x| self.build_here(x)).collect();
                let tag = self.ctor_tag("#tuple");
                Value::Obj(self.heap.alloc(Kind::Data, tag, &fields))
            }
            Build::Record(fs) => {
                let mut pairs: Vec<(InternedString, Value)> = fs
                    .into_iter()
                    .map(|(l, b)| (InternedString::from(l), self.build_here(b)))
                    .collect();
                pairs.sort_by_key(|(l, _)| *l);
                let mut fields = Vec::with_capacity(pairs.len() * 2);
                for (l, v) in pairs {
                    fields.push(Value::Str(l));
                    fields.push(v);
                }
                Value::Obj(self.heap.alloc(Kind::Record, 0, &fields))
            }
            Build::List(xs) => {
                let items: Vec<Value> = xs.into_iter().map(|x| self.build_here(x)).collect();
                let nil = self.ctor_tag("Nil");
                let cons = self.ctor_tag("Cons");
                let mut tail = Value::Obj(self.heap.alloc(Kind::Data, nil, &[]));
                for head in items.into_iter().rev() {
                    tail = Value::Obj(self.heap.alloc(Kind::Data, cons, &[head, tail]));
                }
                tail
            }
        }
    }

    // --- reading an argument ---------------------------------------------

    fn str_arg(&self, what: &str, v: Value) -> Result<InternedString, Error> {
        match v {
            Value::Str(s) => Ok(s),
            other => err(format!("{what}: expected a String, got {}", self.show(other))),
        }
    }

    /// The fields of a tuple of exactly `n`.
    fn tuple_arg(&self, what: &str, v: Value, n: usize) -> Result<Vec<Value>, Error> {
        let ok = v
            .addr()
            .filter(|a| self.heap.kind(*a) == Kind::Data && self.heap.len(*a) == n)
            .filter(|a| {
                self.program
                    .ctor(self.heap.meta(*a))
                    .is_some_and(|c| &*c == "#tuple")
            });
        match ok {
            Some(a) => Ok(self.heap.fields(a)),
            None => err(format!(
                "{what}: expected a tuple of {n}, got {}",
                self.show(v)
            )),
        }
    }

    fn list_arg(&self, what: &str, v: Value) -> Result<Vec<Value>, Error> {
        match self.list_items(v) {
            Some(xs) => Ok(xs),
            None => err(format!("{what}: expected a List, got {}", self.show(v))),
        }
    }

    /// `Just x` / `None`.
    fn maybe_arg(&self, what: &str, v: Value) -> Result<Option<Value>, Error> {
        let Some(a) = v.addr().filter(|a| self.heap.kind(*a) == Kind::Data) else {
            return err(format!("{what}: expected a Maybe, got {}", self.show(v)));
        };
        match self.program.ctor(self.heap.meta(a)).as_deref().map(|s| s.to_string()) {
            Some(n) if n == "None" && self.heap.len(a) == 0 => Ok(None),
            Some(n) if n == "Just" && self.heap.len(a) == 1 => Ok(Some(self.heap.field(a, 0))),
            _ => err(format!("{what}: expected a Maybe, got {}", self.show(v))),
        }
    }

    // --- the natives ------------------------------------------------------

    /// Discharge `effect.op` against the real world, or say it cannot.
    pub(crate) fn native(
        &mut self,
        effect: &str,
        op: &str,
        arg: Value,
    ) -> Result<Option<Value>, Error> {
        let built = match effect {
            "Fs" => self.native_fs(op, arg)?,
            "Process" => return self.native_process(op, arg),
            "Random" => self.native_random(op, arg)?,
            "Time" => self.native_time(op, arg)?,
            _ => return Ok(None),
        };
        Ok(match built {
            Some(b) => Some(self.build(b)),
            None => None,
        })
    }

    fn native_fs(&mut self, op: &str, arg: Value) -> Result<Option<Build>, Error> {
        use std::fs;
        use std::path::Path;

        let what = format!("Fs.{op}");
        let ioerr = |e: std::io::Error| Build::error(e.to_string());
        let unit = |r: std::io::Result<()>| match r {
            Ok(()) => Build::ok(Build::unit()),
            Err(e) => ioerr(e),
        };
        let one = |vm: &Vm, v: Value| vm.str_arg(&what, v);
        let two = |vm: &Vm, v: Value| -> Result<(InternedString, InternedString), Error> {
            let t = vm.tuple_arg(&what, v, 2)?;
            Ok((vm.str_arg(&what, t[0])?, vm.str_arg(&what, t[1])?))
        };

        Ok(Some(match op {
            "readToString" => match fs::read_to_string(&*one(self, arg)?) {
                Ok(c) => Build::ok(Build::Str(c)),
                Err(e) => ioerr(e),
            },
            "readBytes" => match fs::read(&*one(self, arg)?) {
                Ok(b) => Build::ok(Build::List(
                    b.into_iter().map(|x| Build::int(i64::from(x))).collect(),
                )),
                Err(e) => ioerr(e),
            },
            "writeString" => {
                let (p, c) = two(self, arg)?;
                unit(fs::write(&*p, c.as_bytes()))
            }
            "appendString" => {
                use std::io::Write;
                let (p, c) = two(self, arg)?;
                let r = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&*p)
                    .and_then(|mut f| f.write_all(c.as_bytes()));
                unit(r)
            }
            "removeFile" => unit(fs::remove_file(&*one(self, arg)?)),
            "createDir" => unit(fs::create_dir(&*one(self, arg)?)),
            "createDirAll" => unit(fs::create_dir_all(&*one(self, arg)?)),
            "removeDir" => unit(fs::remove_dir(&*one(self, arg)?)),
            "removeDirAll" => unit(fs::remove_dir_all(&*one(self, arg)?)),
            "rename" => {
                let (a, b) = two(self, arg)?;
                unit(fs::rename(&*a, &*b))
            }
            "copy" => {
                let (a, b) = two(self, arg)?;
                match fs::copy(&*a, &*b) {
                    Ok(n) => Build::ok(Build::int(n as i64)),
                    Err(e) => ioerr(e),
                }
            }
            "readDir" => match fs::read_dir(&*one(self, arg)?) {
                Ok(entries) => {
                    let mut names = Vec::new();
                    for e in entries {
                        match e {
                            Ok(en) => {
                                names.push(Build::Str(en.file_name().to_string_lossy().into_owned()))
                            }
                            Err(e) => return Ok(Some(ioerr(e))),
                        }
                    }
                    Build::ok(Build::List(names))
                }
                Err(e) => ioerr(e),
            },
            "metadata" => match fs::metadata(&*one(self, arg)?) {
                Ok(md) => Build::ok(Build::Record(vec![
                    ("isFile", Build::At(Value::Bool(md.is_file()))),
                    ("isDir", Build::At(Value::Bool(md.is_dir()))),
                    ("len", Build::int(md.len() as i64)),
                    (
                        "readonly",
                        Build::At(Value::Bool(md.permissions().readonly())),
                    ),
                ])),
                Err(e) => ioerr(e),
            },
            "exists" => Build::At(Value::Bool(Path::new(&*one(self, arg)?).exists())),
            "isFile" => Build::At(Value::Bool(Path::new(&*one(self, arg)?).is_file())),
            "isDir" => Build::At(Value::Bool(Path::new(&*one(self, arg)?).is_dir())),
            _ => return Ok(None),
        }))
    }

    /// A `Command` is `(program, args, cwd, envVars)`; an `Output` is
    /// `(status, stdout, stderr)`.
    fn native_process(&mut self, op: &str, arg: Value) -> Result<Option<Value>, Error> {
        use std::process::Command as Proc;
        let what = format!("Process.{op}");

        // Read the whole command out before anything is built, so no address is
        // held across the allocation that follows.
        let command = |vm: &Vm, v: Value| -> Result<Proc, Error> {
            let t = vm.tuple_arg(&what, v, 4)?;
            let program = vm.str_arg(&what, t[0])?;
            let mut cmd = Proc::new(&*program);
            for a in vm.list_arg(&what, t[1])? {
                cmd.arg(&*vm.str_arg(&what, a)?);
            }
            if let Some(dir) = vm.maybe_arg(&what, t[2])? {
                cmd.current_dir(&*vm.str_arg(&what, dir)?);
            }
            for e in vm.list_arg(&what, t[3])? {
                let kv = vm.tuple_arg(&what, e, 2)?;
                cmd.env(&*vm.str_arg(&what, kv[0])?, &*vm.str_arg(&what, kv[1])?);
            }
            Ok(cmd)
        };

        let built = match op {
            "spawn" => match command(self, arg)?.output() {
                Ok(out) => Build::ok(Build::Tuple(vec![
                    Build::int(out.status.code().unwrap_or(-1) as i64),
                    Build::Str(String::from_utf8_lossy(&out.stdout).into_owned()),
                    Build::Str(String::from_utf8_lossy(&out.stderr).into_owned()),
                ])),
                Err(e) => Build::error(e.to_string()),
            },
            "status" => match command(self, arg)?.status() {
                Ok(st) => Build::ok(Build::int(st.code().unwrap_or(-1) as i64)),
                Err(e) => Build::error(e.to_string()),
            },
            "exit" => match arg {
                Value::Int(code) => std::process::exit(code as i32),
                other => {
                    return err(format!(
                        "Process.exit: expected an Int, got {}",
                        self.show(other)
                    ));
                }
            },
            "currentPid" => Build::int(std::process::id() as i64),
            "argv" => Build::List(
                std::env::args()
                    .skip(1)
                    .map(Build::Str)
                    .collect::<Vec<_>>(),
            ),
            "getEnv" => match std::env::var(&*self.str_arg(&what, arg)?) {
                Ok(v) => Build::Data("Just", vec![Build::Str(v)]),
                Err(_) => Build::Data("None", vec![]),
            },
            "setEnv" => {
                let t = self.tuple_arg(&what, arg, 2)?;
                let (k, v) = (self.str_arg(&what, t[0])?, self.str_arg(&what, t[1])?);
                unsafe {
                    std::env::set_var(&*k, &*v);
                }
                Build::unit()
            }
            "removeEnv" => {
                let k = self.str_arg(&what, arg)?;
                unsafe {
                    std::env::remove_var(&*k);
                }
                Build::unit()
            }
            _ => return Ok(None),
        };
        Ok(Some(self.build(built)))
    }

    /// SplitMix64, seeded once per process from the clock. Not cryptographic and
    /// no promise of reproducibility — a program that wants repeatable numbers
    /// should `handle` the effect with `Std.Random`'s own pure generator, which
    /// is the whole reason that generator is in the library.
    fn native_random(&mut self, op: &str, arg: Value) -> Result<Option<Build>, Error> {
        use std::cell::Cell;
        thread_local! {
            static STATE: Cell<u64> = const { Cell::new(0) };
        }

        let next = || {
            STATE.with(|s| {
                let mut x = s.get();
                if x == 0 {
                    x = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0x9E37_79B9_7F4A_7C15)
                        | 1;
                }
                x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
                s.set(x);
                let mut z = x;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                z ^ (z >> 31)
            })
        };

        Ok(Some(match op {
            "nextInt" | "nextSeed" => Build::int(next() as i64),
            // [0, 1) from the top 53 bits, which is what an f64 holds exactly.
            "nextFloat" => Build::At(Value::Float(
                (next() >> 11) as f64 / (1u64 << 53) as f64,
            )),
            "intBetween" => {
                let t = self.tuple_arg(&format!("Random.{op}"), arg, 2)?;
                match (t[0], t[1]) {
                    (Value::Int(lo), Value::Int(hi)) => {
                        if hi <= lo {
                            Build::int(lo)
                        } else {
                            let span = (hi - lo) as u64;
                            Build::int(lo + (next() % span) as i64)
                        }
                    }
                    _ => {
                        return err(format!(
                            "Random.{op}: expected two Ints, got {}",
                            self.show(arg)
                        ));
                    }
                }
            }
            _ => return Ok(None),
        }))
    }

    fn native_time(&mut self, op: &str, arg: Value) -> Result<Option<Build>, Error> {
        use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
        // A process-wide origin, so `monotonic` is a small number that fits an
        // `Int` and counts from the program's own start.
        thread_local! {
            static ORIGIN: std::cell::OnceCell<Instant> = const { std::cell::OnceCell::new() };
        }

        Ok(Some(match op {
            // Wall clock, milliseconds since the Unix epoch.
            "now" => Build::int(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0),
            ),
            // Monotonic, for measuring a duration: unaffected by the clock
            // changing under it.
            "monotonic" => Build::int(ORIGIN.with(|o| {
                o.get_or_init(Instant::now).elapsed().as_nanos() as i64
            })),
            "sleep" => match arg {
                Value::Int(ms) if ms > 0 => {
                    std::thread::sleep(Duration::from_millis(ms as u64));
                    Build::unit()
                }
                Value::Int(_) => Build::unit(),
                other => {
                    return err(format!(
                        "Time.sleep: expected an Int, got {}",
                        self.show(other)
                    ));
                }
            },
            _ => return Ok(None),
        }))
    }
}
