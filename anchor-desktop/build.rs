use std::process::Command;

fn main() {
    tauri_build::build();

    // Embed git commit hash at compile time
    let output = Command::new("git").args(["rev-parse", "--short", "HEAD"]).output();

    let hash = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => "unknown".to_string(),
    };

    println!("cargo:rustc-env=GIT_HASH={}", hash);
    // Re-run if HEAD changes (new commit)
    println!("cargo:rerun-if-changed=../.git/HEAD");
    // Prevent cargo from scanning the entire project directory (avoids
    // permission errors on makepkg's pkg/ dir)
    println!("cargo:rerun-if-changed=build.rs");
}
