//! Tauri shell: boots the same localhost server used by the headless binary,
//! then opens a native window on it. All app logic lives in the web UI and
//! the HTTP API, so desktop and mobile shells stay paper-thin.

#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

use tauri::{WebviewUrl, WebviewWindowBuilder};

fn main() {
    let data_dir = librarian_core::default_data_dir();
    let app = librarian_core::App::open(data_dir).expect("opening library");

    // Start the embedded server on an OS-assigned port before the UI loads.
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let (addr, _handle) = rt
        .block_on(librarian_server::serve(app, 0))
        .expect("starting local server");
    // Keep the runtime alive for the life of the process.
    std::mem::forget(rt);

    let url: tauri::Url = format!("http://{addr}").parse().expect("server url");

    tauri::Builder::default()
        .setup(move |app| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                .title("ZIM Librarian")
                .inner_size(1180.0, 800.0)
                .min_inner_size(420.0, 500.0)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running application");
}
