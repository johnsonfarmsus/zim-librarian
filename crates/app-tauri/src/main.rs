//! Desktop entry point; all logic lives in lib.rs (shared with mobile).

#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

fn main() {
    zim_librarian_lib::run();
}
