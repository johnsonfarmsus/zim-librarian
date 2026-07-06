//! Headless launcher: starts the local server and opens the default browser.
//! The Tauri shell (crates/app-tauri) wraps the same server in a native
//! window; this binary is the zero-dependency fallback and dev entry point.

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let data_dir = librarian_core::default_data_dir();
    let app = librarian_core::App::open(data_dir.clone())?;
    let port = std::env::var("ZIM_LIBRARIAN_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0u16);
    let (addr, handle) = librarian_server::serve(app, port).await?;
    let url = format!("http://{addr}");
    eprintln!("ZIM Librarian running at {url}");
    eprintln!("Library data: {}", data_dir.display());
    if std::env::var("ZIM_LIBRARIAN_NO_BROWSER").is_err() {
        let _ = open::that(&url);
    }
    handle.await?;
    Ok(())
}
