fn main() {
    // tauri.conf.json omits `version` so Cargo.toml stays the single source; feed it to
    // tauri-build so the exe's VERSIONINFO resource carries it too.
    if std::env::var_os("TAURI_CONFIG").is_none() {
        let patch = format!(r#"{{"version":"{}"}}"#, env!("CARGO_PKG_VERSION"));
        std::env::set_var("TAURI_CONFIG", patch);
    }
    tauri_build::build();
}
