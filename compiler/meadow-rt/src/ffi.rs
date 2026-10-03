//! Calling C: what `Std.Ffi` asks of a runtime, the same for both.
//!
//! A library is opened by name, a function found in it by name, and the
//! function called with integers, floats, pointers and strings, answering
//! one of those. There is no description of the function here to check the
//! call against: `Std.Ffi` says what the arguments are and what comes back,
//! and a call that says it wrongly is a C program's undefined behaviour.
//!
//! **How a call is made without knowing its type.** The C conventions of
//! aarch64 and x86-64 (System V) pass the first integers in one set of
//! registers and the first floats in another, each in order, and a function
//! reads only the registers of the parameters it has. So every function is
//! called as one of eight integers and eight floats -- the arguments in the
//! first of each, zeros in the rest -- and what it does not take it does not
//! look at. That holds for up to eight integers (six on x86-64) and eight
//! floats; past that an argument goes on the stack, in an order this cannot
//! arrange, and the call is refused. A struct by value and a variadic
//! function are refused by not being expressible.
//!
//! Memory is C's: [`alloc`] and [`free`] are `malloc` and `free`, and numbers
//! are written to and read from an address as so many bytes each. Nothing
//! here is checked, and nothing could be.
//!
//! Unix only, so far.

/// An argument, as the C function is to be handed it.
pub enum Arg {
    /// An integer or a pointer.
    Int(i64),
    Float(f64),
    /// Text, as a NUL-terminated string that lasts as long as the call.
    Str(String),
}

/// What a call answers, as the C function's return type.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Ret {
    Unit,
    Int,
    Float,
    /// A `const char *`, copied; a null one is the empty string.
    Str,
}

impl Ret {
    /// As `Std.Ffi` numbers them: 0 unit, 1 integer or pointer, 2 float,
    /// 3 string.
    pub fn from_code(code: i64) -> Option<Ret> {
        Some(match code {
            0 => Ret::Unit,
            1 => Ret::Int,
            2 => Ret::Float,
            3 => Ret::Str,
            _ => return None,
        })
    }
}

/// What came back: the integer, the float and the text, of which the call's
/// [`Ret`] says which one means anything.
pub type Answer = (i64, f64, String);

#[cfg(unix)]
mod sys {
    use super::{Answer, Arg, Ret};
    use std::ffi::{CStr, CString, c_char, c_void};

    /// How many integer arguments travel in registers.
    const INTS: usize = if cfg!(target_arch = "x86_64") { 6 } else { 8 };
    const FLOATS: usize = 8;

    fn last_error() -> String {
        // Safety: `dlerror` answers a string of its own, or null.
        unsafe {
            let e = libc::dlerror();
            if e.is_null() {
                "unknown error".to_string()
            } else {
                CStr::from_ptr(e).to_string_lossy().into_owned()
            }
        }
    }

    pub fn open(path: &str) -> Result<i64, String> {
        let name = CString::new(path).map_err(|_| "a NUL in the library's name".to_string())?;
        // Safety: a C string, and flags `dlopen` documents.
        let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            Err(last_error())
        } else {
            Ok(handle as i64)
        }
    }

    pub fn symbol(library: i64, name: &str) -> Result<i64, String> {
        let c = CString::new(name).map_err(|_| "a NUL in the function's name".to_string())?;
        // Safety: a handle `open` answered, and a C string.
        let at = unsafe { libc::dlsym(library as *mut c_void, c.as_ptr()) };
        if at.is_null() {
            Err(format!("no `{name}`: {}", last_error()))
        } else {
            Ok(at as i64)
        }
    }

    pub fn call(function: i64, ret: Ret, args: &[Arg]) -> Result<Answer, String> {
        if function == 0 {
            return Err("a call to a null function".to_string());
        }
        let mut ints = [0i64; 8];
        let mut floats = [0f64; 8];
        let (mut ni, mut nf) = (0, 0);
        // The strings, kept until the call returns.
        let mut texts: Vec<CString> = Vec::new();
        for a in args {
            match a {
                Arg::Float(x) => {
                    if nf == FLOATS {
                        return Err(format!("more than {FLOATS} float arguments"));
                    }
                    floats[nf] = *x;
                    nf += 1;
                }
                Arg::Int(_) | Arg::Str(_) => {
                    if ni == INTS {
                        return Err(format!(
                            "more than {INTS} integer, pointer and string arguments"
                        ));
                    }
                    ints[ni] = match a {
                        Arg::Int(n) => *n,
                        Arg::Str(s) => {
                            let c = CString::new(s.as_str())
                                .map_err(|_| "a NUL in a string argument".to_string())?;
                            texts.push(c);
                            texts.last().expect("just pushed").as_ptr() as i64
                        }
                        Arg::Float(_) => unreachable!(),
                    };
                    ni += 1;
                }
            }
        }
        type Ints = extern "C" fn(
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
            f64,
            f64,
            f64,
            f64,
            f64,
            f64,
            f64,
            f64,
        ) -> i64;
        type Floats = extern "C" fn(
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
            f64,
            f64,
            f64,
            f64,
            f64,
            f64,
            f64,
            f64,
        ) -> f64;
        let [i0, i1, i2, i3, i4, i5, i6, i7] = ints;
        let [f0, f1, f2, f3, f4, f5, f6, f7] = floats;
        // Safety: none that can be checked. The address is one `symbol`
        // answered; that it is a function of these arguments answering this
        // is the caller's word. See the module's note on why the extra
        // registers are harmless.
        let answer = unsafe {
            if ret == Ret::Float {
                let f: Floats = std::mem::transmute(function as *const c_void);
                (
                    0,
                    f(
                        i0, i1, i2, i3, i4, i5, i6, i7, f0, f1, f2, f3, f4, f5, f6, f7,
                    ),
                    String::new(),
                )
            } else {
                let f: Ints = std::mem::transmute(function as *const c_void);
                let n = f(
                    i0, i1, i2, i3, i4, i5, i6, i7, f0, f1, f2, f3, f4, f5, f6, f7,
                );
                match ret {
                    Ret::Str if n != 0 => (
                        0,
                        0.0,
                        CStr::from_ptr(n as *const c_char)
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    Ret::Str | Ret::Unit => (0, 0.0, String::new()),
                    _ => (n, 0.0, String::new()),
                }
            }
        };
        drop(texts);
        Ok(answer)
    }

    pub fn alloc(bytes: i64) -> i64 {
        // Safety: `malloc`, which answers null when it cannot.
        unsafe { libc::calloc(1, bytes.max(1) as usize) as i64 }
    }

    pub fn free(at: i64) {
        // Safety: an address `alloc` or C answered, the caller says.
        unsafe { libc::free(at as *mut c_void) }
    }
}

#[cfg(not(unix))]
mod sys {
    use super::{Answer, Arg, Ret};

    const NOT_YET: &str = "calling C is not supported on this platform yet";

    pub fn open(_path: &str) -> Result<i64, String> {
        Err(NOT_YET.to_string())
    }

    pub fn symbol(_library: i64, _name: &str) -> Result<i64, String> {
        Err(NOT_YET.to_string())
    }

    pub fn call(_function: i64, _ret: Ret, _args: &[Arg]) -> Result<Answer, String> {
        Err(NOT_YET.to_string())
    }

    pub fn alloc(_bytes: i64) -> i64 {
        0
    }

    pub fn free(_at: i64) {}
}

/// Open the library at `path` -- or of that name, wherever the system's
/// loader looks -- answering a handle to it, or why not.
pub fn open(path: &str) -> Result<i64, String> {
    sys::open(path)
}

/// The address of `name` in `library`, or why there is none.
pub fn symbol(library: i64, name: &str) -> Result<i64, String> {
    sys::symbol(library, name)
}

/// Call the C function at `function` with `args`, answering as `ret` says.
pub fn call(function: i64, ret: Ret, args: &[Arg]) -> Result<Answer, String> {
    sys::call(function, ret, args)
}

/// `bytes` bytes of C's memory, zeroed: 0 if there are none to be had.
pub fn alloc(bytes: i64) -> i64 {
    sys::alloc(bytes)
}

/// Give back what [`alloc`], or C, allocated.
pub fn free(at: i64) {
    sys::free(at)
}

/// Write `values` from `at` on, each as an integer of `width` bytes (1, 2, 4
/// or 8), or say the width is none of those.
///
/// # Safety
///
/// `at` must be the address of that many bytes the program may write.
pub unsafe fn write_ints(at: i64, width: i64, values: &[i64]) -> Result<(), String> {
    for (i, v) in values.iter().enumerate() {
        // Safety: the caller's.
        unsafe {
            match width {
                1 => (at as *mut u8).add(i).write_unaligned(*v as u8),
                2 => (at as *mut u16).add(i).write_unaligned(*v as u16),
                4 => (at as *mut u32).add(i).write_unaligned(*v as u32),
                8 => (at as *mut i64).add(i).write_unaligned(*v),
                _ => return Err(format!("an integer of {width} bytes")),
            }
        }
    }
    Ok(())
}

/// Read `count` integers of `width` bytes from `at`, signed when `signed`.
///
/// # Safety
///
/// `at` must be the address of that many bytes the program may read.
pub unsafe fn read_ints(at: i64, width: i64, signed: bool, count: i64) -> Result<Vec<i64>, String> {
    let mut out = Vec::with_capacity(count.max(0) as usize);
    for i in 0..count.max(0) as usize {
        // Safety: the caller's.
        out.push(unsafe {
            match (width, signed) {
                (1, false) => i64::from((at as *const u8).add(i).read_unaligned()),
                (1, true) => i64::from((at as *const i8).add(i).read_unaligned()),
                (2, false) => i64::from((at as *const u16).add(i).read_unaligned()),
                (2, true) => i64::from((at as *const i16).add(i).read_unaligned()),
                (4, false) => i64::from((at as *const u32).add(i).read_unaligned()),
                (4, true) => i64::from((at as *const i32).add(i).read_unaligned()),
                (8, _) => (at as *const i64).add(i).read_unaligned(),
                _ => return Err(format!("an integer of {width} bytes")),
            }
        });
    }
    Ok(out)
}

/// Write `values` from `at` on, each as a float of `width` bytes (4 or 8).
///
/// # Safety
///
/// `at` must be the address of that many bytes the program may write.
pub unsafe fn write_floats(at: i64, width: i64, values: &[f64]) -> Result<(), String> {
    for (i, v) in values.iter().enumerate() {
        // Safety: the caller's.
        unsafe {
            match width {
                4 => (at as *mut f32).add(i).write_unaligned(*v as f32),
                8 => (at as *mut f64).add(i).write_unaligned(*v),
                _ => return Err(format!("a float of {width} bytes")),
            }
        }
    }
    Ok(())
}

/// Read `count` floats of `width` bytes from `at`.
///
/// # Safety
///
/// `at` must be the address of that many bytes the program may read.
pub unsafe fn read_floats(at: i64, width: i64, count: i64) -> Result<Vec<f64>, String> {
    let mut out = Vec::with_capacity(count.max(0) as usize);
    for i in 0..count.max(0) as usize {
        // Safety: the caller's.
        out.push(unsafe {
            match width {
                4 => f64::from((at as *const f32).add(i).read_unaligned()),
                8 => (at as *const f64).add(i).read_unaligned(),
                _ => return Err(format!("a float of {width} bytes")),
            }
        });
    }
    Ok(out)
}

/// The `count` bytes at `at`, as text: what is not UTF-8 is replaced.
///
/// # Safety
///
/// `at` must be the address of that many bytes the program may read.
pub unsafe fn read_text(at: i64, count: i64) -> String {
    // Safety: the caller's.
    let bytes = unsafe { std::slice::from_raw_parts(at as *const u8, count.max(0) as usize) };
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The C library is every process's own: opened by no name.
    fn libc_fn(name: &str) -> i64 {
        // Safety: asking the running program for one of its own symbols.
        let at = unsafe {
            libc::dlsym(
                libc::RTLD_DEFAULT,
                std::ffi::CString::new(name).unwrap().as_ptr(),
            )
        };
        assert!(!at.is_null(), "{name}");
        at as i64
    }

    #[test]
    fn integers_strings_and_floats_cross() {
        let strlen = libc_fn("strlen");
        assert_eq!(
            call(strlen, Ret::Int, &[Arg::Str("héllo".into())])
                .unwrap()
                .0,
            6
        );
        let pow = libc_fn("pow");
        assert_eq!(
            call(pow, Ret::Float, &[Arg::Float(2.0), Arg::Float(10.0)])
                .unwrap()
                .1,
            1024.0
        );
        let ldexp = libc_fn("ldexp");
        assert_eq!(
            call(ldexp, Ret::Float, &[Arg::Float(1.5), Arg::Int(4)])
                .unwrap()
                .1,
            24.0
        );
        let getenv = libc_fn("getenv");
        let path = call(getenv, Ret::Str, &[Arg::Str("PATH".into())])
            .unwrap()
            .2;
        assert!(path.contains('/'), "{path}");
        let none = call(
            getenv,
            Ret::Str,
            &[Arg::Str("MEADOW_NO_SUCH_VARIABLE".into())],
        )
        .unwrap()
        .2;
        assert_eq!(none, "", "a null string is the empty one");
    }

    #[test]
    fn memory_is_written_and_read_back() {
        let at = alloc(64);
        assert_ne!(at, 0);
        // Safety: 64 bytes, ours.
        unsafe {
            write_floats(at, 4, &[1.5, -2.25, 3.0]).unwrap();
            assert_eq!(read_floats(at, 4, 3).unwrap(), vec![1.5, -2.25, 3.0]);
            write_ints(at, 1, &[104, 105, 255]).unwrap();
            assert_eq!(read_ints(at, 1, false, 3).unwrap(), vec![104, 105, 255]);
            assert_eq!(read_ints(at, 1, true, 3).unwrap(), vec![104, 105, -1]);
            assert_eq!(read_text(at, 2), "hi");
            assert!(write_ints(at, 3, &[1]).is_err());
        }
        free(at);
    }

    #[test]
    fn what_cannot_be_called_is_refused() {
        assert!(open("/no/such/library.dylib").is_err());
        assert!(call(0, Ret::Int, &[]).is_err());
        let floats: Vec<Arg> = (0..9).map(|_| Arg::Float(0.0)).collect();
        assert!(call(libc_fn("pow"), Ret::Float, &floats).is_err());
    }
}
