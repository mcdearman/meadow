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
use crate::vm::{Error, Vm, err};
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
    /// An `Array` of `UInt8`, a byte to an element.
    Bytes(Vec<u8>),
    /// A `Std.Collections.Vector`, in the shape `Vector.fromArray` gives one --
    /// see [`vector_shape`].
    Vector(Vec<Build>),
}

/// How `Vector.fromArray` lays out `n` elements: nothing, one chunk, or a
/// Read exactly `n` bytes of `input`, growing the buffer as they arrive, or
/// `None` if it ends first. Reading in chunks means a length that lies about
/// how much follows costs nothing until the bytes come, rather than a
/// preallocation of `n` that aborts the process. See `Console.readExact`.
fn read_exact_bounded<R: std::io::Read>(input: &mut R, n: i64) -> std::io::Result<Option<String>> {
    let mut remaining = n.max(0) as usize;
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 65536];
    while remaining > 0 {
        let want = remaining.min(chunk.len());
        match input.read(&mut chunk[..want]) {
            Ok(0) => return Ok(None),
            Ok(got) => {
                buf.extend_from_slice(&chunk[..got]);
                remaining -= got;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

/// radix-balanced tree of chunks of [`VECTOR_WIDTH`] under a `Full` with empty
/// side buffers. Returns the shift and, level by level from the leaves up, how
/// many nodes each level has.
///
/// This is `vBuildTree` in `Std.Collections.Vector`, and has to stay it: a
/// vector built here is taken apart by that module's code.
pub(crate) fn vector_shape(n: usize) -> (i64, Vec<usize>) {
    let mut levels = vec![n.div_ceil(VECTOR_WIDTH)];
    let mut shift = 5;
    while *levels.last().expect("never empty") > VECTOR_WIDTH {
        levels.push(levels.last().expect("never empty").div_ceil(VECTOR_WIDTH));
        shift += 5;
    }
    (shift, levels)
}

/// `vWidth` in `Std.Collections.Vector`.
pub(crate) const VECTOR_WIDTH: usize = 32;

impl Build {
    fn unit() -> Build {
        Build::At(Value::Unit)
    }

    fn int(n: i64) -> Build {
        Build::At(Value::Int(n))
    }

    fn ok(v: Build) -> Build {
        Build::Data("Result.Ok", vec![v])
    }

    fn error(msg: String) -> Build {
        Build::Data("Result.Err", vec![Build::Str(msg)])
    }

    /// How many heap slots the whole tree needs.
    fn slots(&self) -> usize {
        let size = crate::heap::Heap::size_of;
        match self {
            Build::At(_) => 0,
            Build::Str(s) => crate::heap::Heap::packed_slots(s.len()),
            Build::Data(_, xs) | Build::Tuple(xs) => {
                size(Kind::Data, xs.len()) + xs.iter().map(Build::slots).sum::<usize>()
            }
            Build::Record(fs) => {
                size(Kind::Record, 2 * fs.len()) + fs.iter().map(|(_, b)| b.slots()).sum::<usize>()
            }
            Build::Bytes(b) => crate::heap::Heap::packed_slots(b.len()) + size(Kind::Array, 0),
            Build::Vector(xs) => {
                let inner = xs.iter().map(Build::slots).sum::<usize>();
                if xs.len() <= VECTOR_WIDTH {
                    // `Single` and its array, or a bare `Empty`.
                    return size(Kind::Data, 1) + size(Kind::Array, xs.len()) + inner;
                }
                let (_, levels) = vector_shape(xs.len());
                // `Full` and its four empty buffers; every leaf a `Leaf` and an
                // array of elements; every branch a `Branch`, a `None` and an
                // array of children.
                let leaves = levels[0];
                let branches: usize = levels[1..].iter().sum::<usize>() + 1;
                let children: usize = levels.iter().sum::<usize>();
                size(Kind::Data, 7)
                    + 4 * size(Kind::Array, 0)
                    + leaves * (size(Kind::Data, 1) + size(Kind::Array, 0))
                    + xs.len()
                    + branches * (size(Kind::Data, 2) + size(Kind::Data, 0) + size(Kind::Array, 0))
                    + children
                    + inner
            }
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
            Build::Str(s) => Value::Obj(self.heap.alloc_str(s.as_bytes())),
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
            Build::Bytes(b) => Value::Obj(self.heap.alloc_bytes(&b)),
            Build::Vector(xs) => {
                let items: Vec<Value> = xs.into_iter().map(|x| self.build_here(x)).collect();
                self.vector_here(items)
            }
        }
    }

    // --- reading an argument ---------------------------------------------

    fn str_arg(&self, what: &str, v: Value) -> Result<String, Error> {
        self.text(v, what)
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

    fn vector_arg(&self, what: &str, v: Value) -> Result<Vec<Value>, Error> {
        match self.vector_elems(v) {
            Some(xs) => Ok(xs),
            None => err(format!("{what}: expected a Vector, got {}", self.show(v))),
        }
    }

    /// Lay `items` out as `Vector.fromArray` would. Room must already have
    /// been made -- see [`Build::slots`].
    fn vector_here(&mut self, items: Vec<Value>) -> Value {
        let data = |vm: &mut Vm, name: &str, fields: &[Value]| {
            let tag = vm.ctor_tag(name);
            Value::Obj(vm.heap.alloc(Kind::Data, tag, fields))
        };
        let array = |vm: &mut Vm, xs: &[Value]| Value::Obj(vm.heap.alloc(Kind::Array, 0, xs));
        if items.is_empty() {
            return data(self, "Vector.Empty", &[]);
        }
        if items.len() <= VECTOR_WIDTH {
            let a = array(self, &items);
            return data(self, "Vector.Single", &[a]);
        }
        let n = items.len();
        let (shift, _) = vector_shape(n);
        let mut nodes: Vec<Value> = items
            .chunks(VECTOR_WIDTH)
            .map(|chunk| {
                let a = array(self, chunk);
                data(self, "VNode.Leaf", &[a])
            })
            .collect();
        loop {
            let group = |vm: &mut Vm, kids: &[Value]| {
                let none = data(vm, "Maybe.None", &[]);
                let a = array(vm, kids);
                data(vm, "VNode.Branch", &[none, a])
            };
            if nodes.len() <= VECTOR_WIDTH {
                let root = group(self, &nodes);
                let empties: Vec<Value> = (0..4).map(|_| array(self, &[])).collect();
                return data(
                    self,
                    "Vector.Full",
                    &[
                        Value::Int(n as i64),
                        Value::Int(shift),
                        empties[0],
                        empties[1],
                        root,
                        empties[2],
                        empties[3],
                    ],
                );
            }
            nodes = nodes
                .chunks(VECTOR_WIDTH)
                .map(|kids| group(self, kids))
                .collect();
        }
    }

    /// `Just x` / `None`.
    fn maybe_arg(&self, what: &str, v: Value) -> Result<Option<Value>, Error> {
        let Some(a) = v.addr().filter(|a| self.heap.kind(*a) == Kind::Data) else {
            return err(format!("{what}: expected a Maybe, got {}", self.show(v)));
        };
        match self
            .program
            .ctor(self.heap.meta(a))
            .as_deref()
            .map(|s| s.to_string())
        {
            Some(n) if n == "Maybe.None" && self.heap.len(a) == 0 => Ok(None),
            Some(n) if n == "Maybe.Just" && self.heap.len(a) == 1 => {
                Ok(Some(self.heap.field(a, 0)))
            }
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
            "Console" => self.native_console(op, arg)?,
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
        let two = |vm: &Vm, v: Value| -> Result<(String, String), Error> {
            let t = vm.tuple_arg(&what, v, 2)?;
            Ok((vm.str_arg(&what, t[0])?, vm.str_arg(&what, t[1])?))
        };

        Ok(Some(match op {
            "readToString" => match fs::read_to_string(&*one(self, arg)?) {
                Ok(c) => Build::ok(Build::Str(c)),
                Err(e) => ioerr(e),
            },
            "readBytes" => match fs::read(&*one(self, arg)?) {
                Ok(b) => Build::ok(Build::Bytes(b)),
                Err(e) => ioerr(e),
            },
            "writeString" => {
                let (p, c) = two(self, arg)?;
                unit(fs::write(&*p, c.as_bytes()))
            }
            "writeBytes" => {
                let t = self.tuple_arg(&what, arg, 2)?;
                let path = self.str_arg(&what, t[0])?;
                let bytes = self.bytes(t[1], &what)?;
                unit(fs::write(&*path, bytes))
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
                            Ok(en) => names
                                .push(Build::Str(en.file_name().to_string_lossy().into_owned())),
                            Err(e) => return Ok(Some(ioerr(e))),
                        }
                    }
                    Build::ok(Build::Vector(names))
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
            for a in vm.vector_arg(&what, t[1])? {
                cmd.arg(&*vm.str_arg(&what, a)?);
            }
            if let Some(dir) = vm.maybe_arg(&what, t[2])? {
                cmd.current_dir(&*vm.str_arg(&what, dir)?);
            }
            for e in vm.vector_arg(&what, t[3])? {
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
            "isTerminal" => match arg {
                Value::Int(fd) => Build::At(Value::Bool(is_terminal(fd))),
                other => {
                    return err(format!(
                        "Process.isTerminal: expected an Int, got {}",
                        self.show(other)
                    ));
                }
            },
            "argv" => Build::Vector(
                meadow_core::args::get()
                    .into_iter()
                    .map(Build::Str)
                    .collect(),
            ),
            "getEnv" => match std::env::var(&*self.str_arg(&what, arg)?) {
                Ok(v) => Build::Data("Maybe.Just", vec![Build::Str(v)]),
                Err(_) => Build::Data("Maybe.None", vec![]),
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
            "nextFloat" => Build::At(Value::Float((next() >> 11) as f64 / (1u64 << 53) as f64)),
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

    /// Real standard output and input. Output goes wherever [`Vm::io`] sends it,
    /// which is how a debugger shows a program's printing in its console.
    ///
    /// `readLine` strips the line terminator, including a `\r\n` pair, so a
    /// program reading a file piped in on Windows sees the same lines as one
    /// reading a terminal. End of input is `None` rather than an error: a loop
    /// over stdin ends by matching it, which is not an exceptional thing to do.
    fn native_console(&mut self, op: &str, arg: Value) -> Result<Option<Build>, Error> {
        use std::io::BufRead;

        Ok(Some(match op {
            "writeOutput" => {
                let s = self.text(arg, "Console.writeOutput")?;
                self.write_out(&s);
                Build::unit()
            }
            "readLine" => {
                let _ = arg;
                if let Some(input) = &mut self.io.input {
                    return Ok(Some(match input() {
                        Some(line) => Build::Data("Maybe.Just", vec![Build::Str(line)]),
                        None => Build::Data("Maybe.None", vec![]),
                    }));
                }
                // What was written before the program waits on its input is
                // what whoever gives the input may be waiting for -- a prompt,
                // or a reply the next request depends on.
                {
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                }
                let mut line = String::new();
                match std::io::stdin().lock().read_line(&mut line) {
                    // Zero bytes is end of input, not an empty line: an empty
                    // line still carries its terminator.
                    Ok(0) => Build::Data("Maybe.None", vec![]),
                    Ok(_) => {
                        let line = line.strip_suffix('\n').unwrap_or(&line);
                        let line = line.strip_suffix('\r').unwrap_or(line);
                        Build::Data("Maybe.Just", vec![Build::Str(line.to_string())])
                    }
                    Err(e) => return err(format!("Console.readLine: {e}")),
                }
            }
            "readExact" => {
                let n = match arg {
                    Value::Int(n) => n,
                    other => {
                        return err(format!("Console.readExact: expected an Int, got {other:?}"));
                    }
                };
                let read = {
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                    // Reads in chunks, so a hostile length costs nothing until
                    // the bytes actually arrive -- never a preallocation of `n`.
                    read_exact_bounded(&mut std::io::stdin().lock(), n)
                };
                match read {
                    Ok(Some(text)) => Build::Data("Maybe.Just", vec![Build::Str(text)]),
                    Ok(None) => Build::Data("Maybe.None", vec![]),
                    Err(e) => return err(format!("Console.readExact: {e}")),
                }
            }
            _ => return Ok(None),
        }))
    }

    fn native_time(&mut self, op: &str, arg: Value) -> Result<Option<Build>, Error> {
        use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
        // A process-wide origin, so `monotonic` is a small number that fits an
        // `Int` and counts from the program's own start -- one origin for every
        // OS thread, since a green thread can start a measurement on one worker
        // and finish it on another.
        static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

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
            "monotonic" => Build::int(ORIGIN.get_or_init(Instant::now).elapsed().as_nanos() as i64),
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

/// Whether standard input (0), output (1) or error (2) is a terminal:
/// `Process.isTerminal`, for a program choosing whether to colour what it
/// writes.
fn is_terminal(fd: i64) -> bool {
    use std::io::IsTerminal;
    match fd {
        0 => std::io::stdin().is_terminal(),
        1 => std::io::stdout().is_terminal(),
        2 => std::io::stderr().is_terminal(),
        _ => false,
    }
}
