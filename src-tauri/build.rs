fn main() {
    tauri_build::build();

    let git_hash = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=DICTAFLOW_GIT_HASH={git_hash}");
    rerun_on_new_commit();

    link_clang_runtime_on_macos();
}

/// tauri-build emits `rerun-if-changed`, which disables Cargo's default
/// "rerun on any change", so watch the git refs or the embedded hash goes stale.
fn rerun_on_new_commit() {
    let git = std::path::Path::new("../.git");
    let head = git.join("HEAD");
    let Ok(contents) = std::fs::read_to_string(&head) else {
        return;
    };
    // Only emit paths that exist; a missing path makes Cargo rerun every build.
    let mut watched = vec![head, git.join("packed-refs")];
    if let Some(r) = contents.trim().strip_prefix("ref: ") {
        watched.push(git.join(r));
    }
    for path in watched.into_iter().filter(|p| p.exists()) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

/// whisper's Metal backend uses `@available`, which Xcode 27+ compiles to
/// `___isPlatformVersionAtLeast` from clang's runtime. rustc links with
/// `-nodefaultlibs`, so pull `libclang_rt.osx.a` in explicitly.
fn link_clang_runtime_on_macos() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let Some(dir) = std::process::Command::new("clang")
        .arg("--print-runtime-dir")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
    else {
        return;
    };
    let runtime = std::path::Path::new(dir.trim()).join("libclang_rt.osx.a");
    if runtime.exists() {
        println!("cargo:rustc-link-arg={}", runtime.display());
    }
}
