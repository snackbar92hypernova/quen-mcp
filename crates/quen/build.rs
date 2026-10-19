//! Stamps the git commit quen is built from into `quen --version` (QUEN_COMMIT), for bug reports.

use std::process::Command;

fn main() {
    let git = |args: &[&str]| {
        let out = Command::new("git").args(args).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    // No git (a source zip): "unknown".
    let commit = git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=QUEN_COMMIT={commit}");
    // A branch switch rewrites HEAD; a commit, merge, pull or reset rewrites the index (this worktree's own).
    // ponytail: misses `reset --soft` and uncommitted edits (no -dirty); release.ps1 checks the stamp against HEAD.
    for path in ["HEAD", "index"] {
        if let Some(p) = git(&["rev-parse", "--git-path", path]) {
            println!("cargo:rerun-if-changed={p}");
        }
    }
}
