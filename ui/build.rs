//! Writes the fonts yewi is set in (Montserrat, Inter, Fira Code) into
//! `fonts/`, where the `copy-dir` link in `index.html` picks them up. They are
//! generated rather than committed so they always match the yewi release in
//! `Cargo.lock`.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    yewi_fonts::write_to("fonts").expect("the font files should be writable to ui/fonts");
}
