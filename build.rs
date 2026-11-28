//! Compile-time provenance for /api/health.
//!
//! The deploy script's attempt to stage a BUILD_INFO file into the image never reached the running
//! container (the active build path does not run that section), so `commit:"unknown"` was the honest
//! answer for several deploys. The SOURCE TREE always has the information: cargo compiles this app from
//! the repo, which is a git checkout. Reading it here embeds the commit in the BINARY itself, so the
//! answer travels with the artifact no matter which path builds or ships it.
//!
//! Never guesses: with no git available it emits "unknown", so a deploy can never look verified while
//! saying nothing.
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads/main");
    // A rollback must be visible, so a dirty tree is stated rather than smoothed over.
    let commit = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let dirty = git(&["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    let stamp = git(&["log", "-1", "--format=%cI"]).unwrap_or_else(|| "unknown".to_string());
    println!(
        "cargo:rustc-env=BUILD_COMMIT={}{}",
        commit,
        if dirty { "-dirty" } else { "" }
    );
    println!("cargo:rustc-env=BUILD_TIME={stamp}");
}
