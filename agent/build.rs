//! Ensures the UI's build output directory exists before the crate is compiled.
//!
//! [`include_dir!`] resolves at compile time, so `ui/dist` has to be present for
//! `web::ui` to build even when the UI itself has not been built — a fresh clone,
//! or any `cargo build` that does not run `trunk` first.
//!
//! That used to be arranged by committing a `.gitkeep` into the directory, but
//! `trunk build` wipes its output directory on every run and took the marker with
//! it, leaving the working tree dirty after an ordinary UI build. Creating the
//! directory here means nothing has to be tracked inside it, so `ui/dist` can be
//! ignored wholesale.

use std::path::Path;

fn main() {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("cargo always sets CARGO_MANIFEST_DIR");
    let dist = Path::new(&manifest_dir).join("../ui/dist");

    if let Err(err) = std::fs::create_dir_all(&dist) {
        // Not fatal on its own: if the directory turns out to be there anyway,
        // the `include_dir!` below it will succeed and this warning is noise the
        // developer can ignore. If it really is missing, the compile error that
        // follows names the path, and this explains why it was not created.
        println!(
            "cargo:warning=Could not create {} ({err}). The UI assets are embedded from there at compile time.",
            dist.display()
        );
    }

    // `include_dir!` does not track its own inputs, so a rebuilt UI would
    // otherwise be silently ignored until something else forced a recompile.
    println!("cargo:rerun-if-changed=../ui/dist");
}
