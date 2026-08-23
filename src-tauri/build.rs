use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Build stamp: the running app must be able to say which commit it came from
/// ("is my fix in this build?" — the ? cheat sheet footer). Best-effort: a
/// build outside git still compiles, stamped "unknown".
fn stamp() {
    let mut commit = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    // Uncommitted changes baked in: mark the hash so the stamp never claims
    // the build IS that commit when it only started from it.
    if git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty()) {
        commit.push('+');
    }
    println!("cargo:rustc-env=EMBER_GIT_COMMIT={commit}");
    let built = Command::new("date")
        .args(["+%Y-%m-%d %H:%M"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env=EMBER_BUILD_TIME={built}");
    // Re-stamp when HEAD moves. A commit updates the branch ref (or, after a
    // gc, packed-refs); a checkout updates HEAD. Best-effort staleness guard —
    // any real code change recompiles anyway.
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
        println!("cargo:rerun-if-changed={dir}/packed-refs");
        if let Some(branch) = git(&["rev-parse", "--symbolic-full-name", "HEAD"]) {
            println!("cargo:rerun-if-changed={dir}/{branch}");
        }
    }
}

fn main() {
    stamp();
    tauri_build::build()
}
