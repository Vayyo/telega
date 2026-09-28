fn main() {
    tdlib_rs::build::build(None);
    embed_api_keys();
}

/// Embeds `TG_API_ID` / `TG_API_HASH` from `.env` (or the build
/// environment) into the binary, so a built client runs anywhere without
/// the file. The runtime environment can still override them.
fn embed_api_keys() {
    // Cargo treats a missing rerun-if-changed path as always stale: without
    // this guard a build with keys only in the environment (.env absent,
    // gitignored) would rerun this script, and rebuild the crate, every time.
    if std::path::Path::new(".env").exists() {
        println!("cargo:rerun-if-changed=.env");
    }
    for key in ["TG_API_ID", "TG_API_HASH"] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    let from_file = std::fs::read_to_string(".env").unwrap_or_default();
    for key in ["TG_API_ID", "TG_API_HASH"] {
        let value = std::env::var(key).ok().or_else(|| {
            from_file.lines().find_map(|line| {
                let (k, v) = line.split_once('=')?;
                (k.trim() == key).then(|| v.trim().trim_matches('"').to_owned())
            })
        });
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            println!("cargo:rustc-env={key}={value}");
        }
    }
}
