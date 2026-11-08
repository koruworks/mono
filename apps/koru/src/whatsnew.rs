// SPDX-License-Identifier: MPL-2.0

//! What a build says about itself, the first time you see it.
//!
//! An update used to finish in silence. The banner went away, the windows came
//! back, and the only evidence that anything had happened was a version string
//! in a tray submenu nobody opens — after the person had agreed to two prompts
//! and watched their work close. So a build that has just been installed opens
//! one tab and says so.
//!
//! ## The notes are on the disk, not on the wire
//!
//! `UpdateManifest.release_notes` carries a title, a few summary lines and a
//! URL, and never the body: the long Markdown is rendered to HTML once, by
//! `koru-release`, and lives on R2 as an immutable page. That page is right for
//! a link somebody shares and useless here.
//!
//! So `koru-release` writes the same Markdown into the package as
//! [`RELEASE_NOTES_FILE`], beside the four executables, and this reads it from
//! beside *its own* executable. The build that is running always has the notes
//! for the build that is running — no network, no manifest change, and no
//! version to match up, because a file cannot be beside the wrong binary.
//!
//! **Absence is ordinary.** A `cargo run` build was never released and a
//! `dist-local` bundle usually has no descriptor, so there is frequently no file
//! at all. The greeting stands on its own and nothing logs a complaint.

use koru_channel::RELEASE_NOTES_FILE;
use koru_workspace_host::{RichBlock, parse_markdown};

/// The GitHub releases list, including release discussions and reactions.
pub const RELEASES_URL: &str = "https://github.com/koruworks/mono/releases";

/// Match the release tool's tags: moving `nightly`, otherwise `v<version>`.
/// Internal builds have no published release, so use the releases list.
pub fn release_url(version: &str, channel: koru_channel::ReleaseChannel) -> String {
    use koru_channel::ReleaseChannel;
    match channel {
        ReleaseChannel::Internal => RELEASES_URL.to_owned(),
        ReleaseChannel::Nightly => format!("{RELEASES_URL}/tag/nightly"),
        ReleaseChannel::Stable | ReleaseChannel::Beta => format!("{RELEASES_URL}/tag/v{version}"),
    }
}

/// This build's release notes, if it shipped with any.
///
/// **Beside the executable, never through a search path.** The rule the four
/// binaries already follow, for the reason it exists: they are built, signed and
/// shipped as one unit, and a `RELEASE_NOTES.md` found anywhere else describes
/// some other build.
///
/// Every failure is `None` — no executable path, no parent, no file, unreadable,
/// not UTF-8. None of them is worth a word to anybody: the tab still opens and
/// still says what happened.
///
/// **Read once, when the tab is created**, which is once per install. Not on a
/// timer and not per frame, so it is outside everything the responsiveness rules
/// are about.
pub fn notes_beside_the_exe() -> Option<String> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    // Beside the exe on Windows; in the bundle's Resources/ on macOS, where
    // codesign's seal keeps data out of Contents/MacOS. Still this build's
    // own notes either way — both paths are inside the shipped unit.
    std::fs::read_to_string(dir.join(RELEASE_NOTES_FILE))
        .ok()
        .or_else(|| {
            std::fs::read_to_string(dir.parent()?.join("Resources").join(RELEASE_NOTES_FILE)).ok()
        })
}

/// The first thing the tab says, as Markdown.
///
/// **A link rather than a bare address**, so clicking it in the page opens the
/// releases site. In-app Feedback is the overflow-menu destination.
///
/// English, like the notes underneath it. `koru-i18n` covers the first-run
/// screens and nothing else, and a translated sentence sitting on top of
/// untranslated release notes reads worse than a page that is consistently one
/// language.
pub fn greeting(version: &str) -> String {
    format!(
        "Updated to Koru {version}. See what changed on the [releases page]({url}). \
         Feedback is always welcome — use Feedback in the app menu.",
        url = "https://www.koru.do/releases",
    )
}

/// The whole page: the greeting, then this build's notes if it has any.
///
/// **Two parses, concatenated — not one string parsed once.** Joining them with
/// a blank line and handing the result to one parser is the obvious
/// simplification and it is wrong for a handful of openings: notes that begin
/// with a setext underline (`===`) would retroactively turn the greeting's
/// paragraph into a heading, and a body opening inside a fence would swallow it
/// whole. The greeting is ours and the notes are the release author's; parsing
/// them separately is what keeps one from reinterpreting the other.
pub fn blocks(version: &str, notes: Option<&str>) -> Vec<RichBlock> {
    let mut blocks = parse_markdown(&greeting(version));
    if let Some(notes) = notes {
        blocks.extend(parse_markdown(notes));
    }
    blocks
}

/// The notes' section headings, in order — the `## ` lines of the shipped
/// body. The help popover shows the first three as *What's new*
/// (`docs/features/workspace-foundation/TECHNICAL.final.md` §5.10): the
/// shipped file is one version's notes with no dates and no earlier
/// versions, so its sections are the honest list.
pub fn headlines(notes: &str) -> Vec<String> {
    notes
        .lines()
        .filter_map(|line| {
            let line = line.trim_end();
            let rest = line.strip_prefix("## ")?;
            let rest = rest.trim();
            (!rest.is_empty()).then(|| rest.to_owned())
        })
        .collect()
}

/// Read the shipped notes **off the event thread**, once, and hand back
/// their headlines — `None` when no notes shipped beside this build. The
/// window keeps the answer; the `?` never reads the disk.
pub fn read_headlines_async() -> std::sync::mpsc::Receiver<Option<Vec<String>>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("koru-whatsnew-headlines".into())
        .spawn(move || {
            let answer = notes_beside_the_exe().map(|notes| headlines(&notes));
            let _ = tx.send(answer);
        })
        .ok();
    rx
}

#[cfg(test)]
mod tests;
