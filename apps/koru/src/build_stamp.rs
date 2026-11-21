// SPDX-License-Identifier: MPL-2.0

//! What this build knows about the tree it came out of.
//!
//! One reader for `KORU_BRANCH`, because three call sites had grown their own
//! and a fourth was about to. `option_env!` resolves in the crate it is written
//! in, so this has to live here — beside `build.rs`, which is what stamps it —
//! rather than in `koru-channel` with the rest of `build_info`.

/// The branch this build came from, when it is a build that has one.
///
/// Stamped by `apps/koru/build.rs`, which emits it for a build made from a
/// working tree and not for one `koru-release` published — the gate is there so
/// a released binary does not carry a name it has decided not to show. So this
/// is `Some` under `cargo run` and `cargo build --release` in a checkout, and
/// `None` in a release, in a source tarball with no git, and on a detached HEAD,
/// which names no branch.
pub(crate) fn branch() -> Option<&'static str> {
    option_env!("KORU_BRANCH")
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

/// The development branch named in the window menu, which is not every branch.
///
/// **This app's judgement, not the pill's.** `koru_shell_chrome::branch` draws
/// whatever name it is given; deciding that `dev` and `main` are unremarkable
/// is a fact about *this repository's* habits, and a crate chartered to hold
/// only rects and drawing primitives may not hold it
/// ([ADR-262](../../docs/decisions/README.md)). The gate lived there as
/// `worth_showing` until then, and it belongs here anyway — beside the
/// `option_env!` that decides what a build is stamped with in the first place.
///
/// The historical function name is retained; the main window now places this
/// read-only information below Feedback rather than reserving title-bar space.
pub(crate) fn caption_branch() -> Option<&'static str> {
    worth_naming(branch())
}

/// Whether this branch is one worth naming in the window menu.
///
/// `dev` and `main` are the ordinary places to be, so a pill on them would be
/// permanent furniture that stops being read — and a label nobody reads is
/// worse than no label, because it still takes the room. What is worth saying
/// is *you are somewhere else*. An empty name is what a released build, a
/// source tarball with no git, and a detached HEAD all answer.
///
/// Split from [`caption_branch`] so the judgement is reachable from a test on
/// whatever branch the suite happens to run on.
fn worth_naming(branch: Option<&str>) -> Option<&str> {
    let name = branch?.trim();
    (!name.is_empty() && name != "dev" && name != "main").then_some(name)
}

#[cfg(test)]
mod tests;
