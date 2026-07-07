//! Inspect a ZIM file: header info and an entry histogram by namespace and
//! MIME type. Usage: cargo run -p zimlib --example inspect -- <file.zim>

use std::collections::HashMap;

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("usage: inspect <file.zim>");
    let z = zimlib::Zim::open(&path)?;
    println!(
        "major={} minor={} entries={} clusters={} article_ns={:?}",
        z.header.major_version,
        z.header.minor_version,
        z.entry_count(),
        z.header.cluster_count,
        z.article_namespace() as char,
    );
    let mut hist: HashMap<(char, String), u32> = HashMap::new();
    let mut sample: HashMap<(char, String), String> = HashMap::new();
    for i in 0..z.entry_count() {
        let Ok(e) = z.entry_at(i) else { continue };
        let mime = match &e.kind {
            zimlib::EntryKind::Redirect { .. } => "<redirect>".to_string(),
            zimlib::EntryKind::Other => "<other>".to_string(),
            _ => e.mime.clone().unwrap_or_else(|| "<none>".into()),
        };
        let key = (e.namespace as char, mime);
        *hist.entry(key.clone()).or_default() += 1;
        sample.entry(key).or_insert(e.path.clone());
    }
    let mut rows: Vec<_> = hist.into_iter().collect();
    rows.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for ((ns, mime), n) in rows {
        let eg = &sample[&(ns, mime.clone())];
        println!("{n:>8}  ns={ns:?}  {mime}  e.g. {eg}");
    }
    if let Some(main) = z.main_page() {
        println!("main page: ns={:?} path={}", main.namespace as char, main.path);
    }
    Ok(())
}
