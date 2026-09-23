//! The fingerprint of the sources that decide the ABI between the code this
//! crate emits and the runtime it links against, as `MEADOW_AOT_FINGERPRINT`:
//! this crate's, and `aot`'s. The runtime exports a symbol named after it
//! (`aot/build.rs` hashes the same trees with the same function,
//! `rts/fingerprint.rs`), and the emitted module refers to that symbol, so a
//! program links only with a runtime built from the same sources.

use std::path::Path;

include!("../../rts/fingerprint.rs");

fn main() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let roots = [here.join("src"), here.join("../../aot/src")];
    println!(
        "cargo:rustc-env=MEADOW_AOT_FINGERPRINT={}",
        fingerprint(&roots)
    );
}
