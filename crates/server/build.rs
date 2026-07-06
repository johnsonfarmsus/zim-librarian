// rust-embed reads ../../ui at macro expansion time; without this, editing
// UI assets does not rebuild the crate and release binaries serve stale files.
fn main() {
    println!("cargo:rerun-if-changed=../../ui");
}
