//! `cargo run --example cut -- FILE...`: each Cut program read, printed and
//! read back, and run by the reference interpreter -- what it printed, and
//! what it failed with if it did -- and then lowered to AxCut and run by its
//! machine, on a line of its own: what it answered, or why it was not
//! lowered.

use meadow_cut::interp::{Options, run};
use meadow_cut::{parse, print};

fn main() {
    let mut failed = false;
    for path in std::env::args().skip(1) {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("{path}: {e}");
                failed = true;
                continue;
            }
        };
        let p = match parse(&text) {
            Ok(p) => p,
            Err(e) => {
                println!("{path}: does not read: {e}");
                failed = true;
                continue;
            }
        };
        let printed = print::program(&p);
        if parse(&printed).as_ref() != Ok(&p) {
            println!("{path}: does not read back the same once printed");
            failed = true;
        }
        match run(&p, &Options::default()) {
            Ok(out) => println!(
                "{path}: ok, status {}, printed {:?}{}",
                out.status,
                out.output,
                if out.errors.is_empty() {
                    String::new()
                } else {
                    format!(", errors {:?}", out.errors)
                }
            ),
            Err((e, out)) => {
                println!("{path}: fails: {e} (after printing {:?})", out.output);
                failed = true;
            }
        }
        match meadow_cut::lower::lower(&p) {
            Err(e) => println!("{path}: axcut: not lowered: {e}"),
            Ok(lowered) => match meadow_axcut::machine::Machine::run(&lowered, 200_000_000) {
                Ok(meadow_axcut::machine::Value::Str(s)) => {
                    println!("{path}: axcut: answered {:?}", &*s)
                }
                Ok(v) => println!("{path}: axcut: answered {v}"),
                Err(e) => println!("{path}: axcut: fails: {e:?}"),
            },
        }
    }
    std::process::exit(i32::from(failed));
}
