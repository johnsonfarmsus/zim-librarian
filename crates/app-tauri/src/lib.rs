//! Tauri shell: boots the same localhost server used by the headless binary,
//! then opens a native window (or mobile webview) on it. All app logic lives
//! in the web UI and the HTTP API, so desktop and mobile shells stay
//! paper-thin.

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // Desktop shares the headless binary's data dir; mobile lives in
            // the app sandbox Tauri resolves for us.
            let data_dir = if cfg!(any(target_os = "ios", target_os = "android")) {
                app.path().app_data_dir()?
            } else {
                librarian_core::default_data_dir()
            };
            install_bundled_models(app.handle(), &data_dir);

            eprintln!("[shell] data dir: {}", data_dir.display());
            let core = librarian_core::App::open(data_dir)?;
            // Start the embedded server on an OS-assigned localhost port
            // before the UI loads.
            let rt = tokio::runtime::Runtime::new()?;
            let (addr, _handle) = rt.block_on(librarian_server::serve(core, 0))?;
            // Keep the runtime alive for the life of the process.
            std::mem::forget(rt);
            eprintln!("[shell] serving on http://{addr}");

            let url: tauri::Url = format!("http://{addr}").parse()?;
            #[allow(unused_mut)]
            let mut win = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                .title("ZIM Librarian");
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            {
                win = win.inner_size(1180.0, 800.0).min_inner_size(420.0, 500.0);
            }
            win.build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running application");
}

/// Copy (or hard-link) any GGUF the installer bundled into the user's models
/// folder, once. This is how "the app ships with a model pre-installed"
/// works: `librarian_core::App::open` then selects it on a fresh install.
fn install_bundled_models(app: &tauri::AppHandle, data_dir: &std::path::Path) {
    let Ok(res_dir) = app.path().resource_dir() else { return };
    let src = res_dir.join("resources");
    let models = data_dir.join("models");
    let Ok(rd) = std::fs::read_dir(&src) else { return };
    for e in rd.filter_map(|e| e.ok()) {
        let name = e.file_name();
        if !name.to_string_lossy().to_ascii_lowercase().ends_with(".gguf") {
            continue;
        }
        let dest = models.join(&name);
        if dest.exists() {
            continue;
        }
        let _ = std::fs::create_dir_all(&models);
        // Hard link avoids duplicating a ~1 GB file; fall back to a real
        // copy across volumes (e.g. translocated apps).
        if std::fs::hard_link(e.path(), &dest).is_err() {
            if std::fs::copy(e.path(), &dest).is_err() {
                let _ = std::fs::remove_file(&dest);
            }
        }
    }
}
