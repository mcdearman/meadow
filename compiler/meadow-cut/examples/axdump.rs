//! `cargo run --example axdump -- FILE`: a Cut program lowered to AxCut, as
//! text.

fn main() {
    for path in std::env::args().skip(1) {
        let text = std::fs::read_to_string(&path).expect("a file");
        let p = meadow_cut::parse(&text).expect("a Cut program");
        match meadow_cut::lower::lower(&p) {
            Ok(lowered) => println!("{}", lowered.pretty()),
            Err(e) => println!("{path}: not lowered: {e}"),
        }
    }
}
