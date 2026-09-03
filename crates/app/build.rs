//! Встраивание версии сборки: git-хеш и время — в session.json прогона.
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=SYNERGY_GIT_HASH={hash}");
    // время без chrono: build-скрипту хватает системной даты
    let now = std::process::Command::new("date")
        .arg("+%Y-%m-%d %H:%M:%S")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env=SYNERGY_BUILD_TIME={now}");
}
