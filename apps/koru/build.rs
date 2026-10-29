// SPDX-License-Identifier: MPL-2.0

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    stamp_branch();

    println!("cargo:rerun-if-env-changed=KORU_VERSION");
    println!("cargo:rerun-if-env-changed=KORU_BUILD_NUMBER");

    let assets = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("assets");

    // **"Koru Workspace", and it is what Task Manager shows.** The three
    // binaries a person can see listed together were "Koru", "Koru Agent" and
    // "Koru Store", where the bare one is the window and the other two are its
    // services — a distinction the shortest name made hardest to read. Naming
    // the thing it opens says which is which.
    //
    // It carries **no version**: `koru-winres` takes the name and the version
    // separately so the two cannot be concatenated by accident, because the
    // notification area remembers "always show this icon" against
    // `FileDescription`. Its module documentation records that bug in full.
    // Nothing keys off this string here — the shell creates no tray icon
    // (ADR-41, the agent owns the only one) and the taskbar pins on the
    // AppUserModelID — so the rename costs nothing already remembered.
    //
    // Resource 2 is the taskbar's Recent list: a shell link's icon is a file
    // path plus an index, so a font glyph cannot be handed to Windows directly
    // — this is the Phosphor document mark rasterised by
    // `scripts/icons/make-document-icon.py`. Ordinal 1 stays the application icon.
    let resource = koru_winres::Resource::new(
        "Koru Workspace",
        "koru.exe",
        koru_winres::channel_icon(&assets),
    )
    .icon(2, assets.join("document.ico"))
    .versioned(version(), build_number());
    koru_winres::embed("koru", &resource);
}

/// The version a release stamps in, or the package's own.
fn version() -> String {
    env::var("KORU_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| env::var("CARGO_PKG_VERSION").expect("package version"))
}

fn build_number() -> u64 {
    env::var("KORU_BUILD_NUMBER")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
}

/// The branch this build came from, stamped in as `KORU_BRANCH` for Welcome to
/// show beside the name — because several worktrees of this repo are open at
/// once and their windows are otherwise identical on screen.
///
/// **Only a build nobody published gets one.** `koru-release` sets
/// `KORU_RELEASE_CHANNEL`, and a build carrying that is already identified by
/// its version, channel and commit; a branch name on it would be noise nobody
/// can act on. Everything else — `cargo run`, `cargo build --release`, a `dist`
/// bundle — came out of somebody's working tree, which is the case the name
/// answers. That gate lives here rather than in the view, so a released binary
/// does not carry a branch name it has decided not to show.
///
/// The rerun is keyed on the git directory's `HEAD` **file**, which is what a
/// checkout rewrites and — because it holds `ref: refs/heads/<name>` rather
/// than a commit id — what an ordinary commit leaves alone. Without it the
/// stamp freezes at whatever branch was current the first time this script ran
/// and then quietly lies. It is `--absolute-git-dir` rather than a literal
/// `.git`, because this tree may be a worktree, whose HEAD lives in
/// `<main>/.git/worktrees/<name>/HEAD` and whose own `.git` is a file.
fn stamp_branch() {
    println!("cargo:rerun-if-env-changed=KORU_RELEASE_CHANNEL");
    if env::var_os("KORU_RELEASE_CHANNEL").is_some() {
        return;
    }
    let Some(manifest) = env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from) else {
        return;
    };
    let Some(git_dir) = git(&manifest, &["rev-parse", "--absolute-git-dir"]) else {
        return;
    };
    println!(
        "cargo:rerun-if-changed={}",
        Path::new(&git_dir).join("HEAD").display()
    );
    match git(&manifest, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        // A detached HEAD answers with "HEAD", which names no branch at all.
        Some(branch) if !branch.is_empty() && branch != "HEAD" => {
            println!("cargo:rustc-env=KORU_BRANCH={branch}");
        }
        _ => {}
    }
}

/// One git call whose failure is never a build failure: no git on PATH, no
/// repository, or a source tarball all mean "no branch to show" rather than
/// "cannot build". Nothing here is load-bearing enough to stop a build over.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}
