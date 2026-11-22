// SPDX-License-Identifier: MPL-2.0

//! Before there is a window: which profile, and whether it opened cleanly.
//!
//! Two jobs, both of which have to happen before `App::new` can build anything:
//!
//! - **Deciding which profile folder.** Still resolved here for the transitional
//!   shell — `--profile <name|id|path>`, the Shift/`--pick-profile` picker, the
//!   `last-selected` marker, the most-recent profile, or a machine-local
//!   scaffold on a first start. **Content itself lives in the Koru Store**; the
//!   folder is only what settings and a few leftover call sites hang off until
//!   cutover step E deletes it. A plain launch therefore never asks where.
//! - **Saying what happened.** A recovery, a read-only document, a skipped peer
//!   log, or a failure to open at all. All four used to be an `eprintln!` into a
//!   console nobody was looking at.
//!
//! Native dialogs rather than in-app UI, because all of this happens before
//! there is an app to put UI in. `rfd` is already here for the image picker.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::cli;
use koru_docstore::profilefolder::{self, FolderKind};
use koru_docstore::registry;
use koru_ui::keys::debug as inputdebug;
use koru_workspace::session::Report;

/// Say where this run's content is, on **stdout**, every launch.
///
/// Always, and not on stderr: the one question a confused user asks — *which
/// folder am I actually looking at?* — otherwise has no answer anywhere in the
/// program. A wrong folder is permanent and invisible: every launch opens the
/// same empty profile looking like a fresh install.
fn announce_location(dir: &Path) {
    println!("[Koru] local settings: {}", dir.display());
    if !holds_a_workspace(dir) {
        // Content opens from the store; an empty scaffold is normal, not a
        // promise that writing will land here.
        println!("[Koru] content opens from the Koru Store");
    }
    inputdebug::announce();
}

/// Whether `dir` already holds a workspace, rather than being an empty folder
/// that is about to get one.
fn holds_a_workspace(dir: &Path) -> bool {
    std::fs::read_dir(dir.join("workspaces")).is_ok_and(|mut d| d.next().is_some())
}

/// Machine-local folder the transitional shell still opens while content lives
/// in the store.
///
/// Settings and a few leftover call sites still hang off a path; nothing the
/// person writes goes here once `App::new` has swapped onto the store. Lives
/// beside `profiles.conf` so a portable install and a Program Files install
/// each keep their own, the same way the registry does.
fn local_scaffold_folder() -> Option<PathBuf> {
    let path = registry_path()?.parent()?.join("local-profile");
    if let Err(e) = std::fs::create_dir_all(&path) {
        fatal(&format!(
            "Koru could not create its local settings folder:\n\n{}\n\n{e}",
            path.display()
        ));
        return None;
    }
    Some(path)
}

/// Profile-independent settings namespace for an ordinary Store launch.
///
/// Resolving it is read-only: the directory is created only by Profile-bound
/// persistence after a real Store Profile exists. Rooted runs stay beneath
/// their supplied root; ordinary runs use the existing install-local registry
/// base without creating the retired `local-profile` scaffold.
fn store_shell_folder() -> Option<PathBuf> {
    if let Some(root) = cli::root_from_args() {
        return Some(root.join("shell"));
    }
    Some(registry_path()?.parent()?.join("shell"))
}

/// Settings path for a window launch.
///
/// Explicit legacy folder selection remains supported. A plain Store launch
/// uses the Profile-independent shell namespace and never resolves or creates
/// `local-profile`.
pub fn launch_settings_folder() -> Option<PathBuf> {
    if cli::profile_from_args().is_some() || wants_profile_picker(false) {
        profile_folder()
    } else {
        store_shell_folder()
    }
}

/// Answer "which folder am I using?" without opening a window — `--where`.
///
/// The question that prompted this: a remembered path is otherwise only
/// visible on stderr, which a windowed build usually has nowhere to put.
pub fn report_location() {
    // What a plain launch would open: the last-selected marker, or the most
    // recently opened profile. Read-only — this never asks.
    let dir = selected_folder().or_else(|| {
        registry::most_recent(&profiles())
            .filter(|r| r.dir.exists())
            .map(|r| r.dir.clone())
    });
    match dir {
        Some(dir) => {
            println!("profile:   {}", dir.display());
            // Read out of `profile.conf` where there is one, and only decoded
            // from the snapshot when there is not — this runs with no window
            // and must not need a CRDT to answer "whose folder is this?".
            match koru_workspace::open::describe(&dir) {
                Some((id, name, email)) => {
                    println!("  name:        {name}  <{}>", id.to_storage());
                    if let Some(e) = email {
                        println!("  email:       {e}");
                    }
                    let listed = koru_workspace::open::peek_workspaces(&dir);
                    println!("  workspaces:  {}", listed.len());
                    for w in listed {
                        println!("    {}  <{}>", w.name, w.id.to_storage());
                    }
                }
                None => println!(
                    "  {}",
                    match profilefolder::inspect(&dir) {
                        FolderKind::Legacy =>
                            "none yet - this folder predates profiles and will be adopted",
                        FolderKind::Profile { .. } => "here, but it could not be read",
                        FolderKind::Empty => "EMPTY - a new profile will be created here",
                    }
                ),
            }
        }
        None => println!("profile:   not chosen yet - Koru will ask on the next launch"),
    }
    if let Some(p) = registry_path() {
        println!("list:      {}", p.display());
        for r in registry::by_recency(&profiles()) {
            println!(
                "  {}  {}  {}",
                r.name,
                r.dir.display(),
                if r.dir.exists() { "present" } else { "MISSING" }
            );
        }
    }
}

/// Tell the user about anything unusual in how the workspace opened.
///
/// Quiet by design in the ordinary case — this fires on a recovery, a read-only
/// document, or a skipped peer log, and on nothing else.
pub fn announce(dir: &Path, report: &Report) {
    if report.is_quiet() {
        return;
    }
    let mut body = String::new();

    if let Some(r) = &report.recovery {
        body.push_str(&format!("{}.\n\n", r.problem));
        match &r.from {
            Some(p) => body.push_str(&format!("Recovered from a backup:\n{}\n\n", p.display())),
            None => body.push_str(
                "There was no usable backup. What is open was rebuilt from the \
                 peer update logs, and may be incomplete.\n\n",
            ),
        }
        if let Some(q) = &r.quarantined {
            body.push_str(&format!(
                "A copy of the unreadable file was kept:\n{}\n\n",
                q.display()
            ));
        }
    }

    if let Some(why) = &report.read_only {
        body.push_str(&format!(
            "{why}.\n\nThis session will not save. Open it with a newer build, \
             or the newer build's changes would be written back by one that \
             cannot see them.\n\n",
        ));
    }

    if !report.damaged.is_empty() {
        body.push_str(&format!(
            "{} peer update file(s) would not decode and were skipped:\n{}\n\n\
             Your own work is unaffected; changes from those peers may be missing.\n\n",
            report.damaged.len(),
            report.damaged.join("\n")
        ));
    }

    body.push_str(&format!("Workspace: {}", dir.display()));

    // Also on stderr: a dialog is dismissed and gone, and this is exactly the
    // text somebody will want to paste into a bug report an hour later.
    eprintln!("[Koru] {}", body.replace('\n', " "));
    rfd::MessageDialog::new()
        .set_title("Koru")
        .set_level(rfd::MessageLevel::Warning)
        .set_description(&body)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

/// `--backup [dest]`: copy the whole profile folder somewhere safe, and print
/// where it went.
///
/// It lives in the binary rather than in the runner scripts so it resolves the
/// profile folder with [`profile_folder`] — the same code, in the same order,
/// as a normal launch. A shell script that reimplemented that would eventually
/// back up the wrong folder, and would do it quietly.
///
/// Returns whether it worked.
pub fn run_backup(dest: Option<PathBuf>) -> bool {
    let Some(dir) = profile_folder() else {
        eprintln!("[Koru] no workspace folder chosen; nothing was backed up");
        return false;
    };
    let dest = dest.unwrap_or_else(|| {
        // Beside the data directory rather than inside it: a backup within the
        // thing being backed up is one `rm -rf` away from being no backup.
        let parent = dir.parent().unwrap_or(&dir).to_path_buf();
        parent
            .join("koru-backups")
            .join(koru_docstore::backup::stamp_now())
    });
    match koru_docstore::backup::copy_tree(&dir, &dest) {
        Ok(n) => {
            println!("[Koru] backed up {n} file(s)");
            println!("  from  {}", dir.display());
            println!("  to    {}", dest.display());
            true
        }
        Err(e) => {
            eprintln!("error: could not back up {} ({e})", dir.display());
            false
        }
    }
}

/// Opening failed and there is nothing to show. Say so and let the caller exit.
///
/// **Never "carry on in memory".** A workspace that cannot be saved is
/// indistinguishable from one that can until it is closed, and by then a day of
/// typing is gone.
/// Whether this executable runs from a launch location that *competes with*
/// a real installation instead of being one: an App-Translocated mirror
/// (macOS runs a quarantined app from a randomized read-only copy) or a
/// read-only volume — the mounted disk image itself. Pure so the
/// classification is testable; the caller says whether the executable's
/// directory accepts a write, which is what tells the mounted image apart
/// from an app legitimately installed on an external drive.
pub fn is_disk_image_launch(exe: &Path, exe_dir_writable: bool) -> bool {
    let path = exe.to_string_lossy();
    path.contains("/AppTranslocation/") || (path.starts_with("/Volumes/") && !exe_dir_writable)
}

/// What the copy in `/Applications/Koru.app` says about itself, read from its
/// `Info.plist`: the build number `koru-release` stamps as `CFBundleVersion`
/// and the display version it writes as `CFBundleGetInfoString` ("Koru 0.1.2",
/// "Koru 0.0.2-beta.8"). Either may be missing from a bundle somebody else
/// assembled; a missing build number is "unknown", never zero.
#[derive(Debug, PartialEq, Eq)]
pub struct InstalledCopy {
    pub label: String,
    pub build_number: Option<u64>,
}

/// Parse the two values out of an `Info.plist` without a plist parser: the
/// packager writes `<key>…</key><string>…</string>` on one line, and a
/// re-serialized plist puts whitespace between them, so the match tolerates
/// any whitespace and nothing else between the key and its string.
pub fn parse_installed_copy(info_plist: &str) -> InstalledCopy {
    fn string_after(plist: &str, key: &str) -> Option<String> {
        let marker = format!("<key>{key}</key>");
        let rest = &plist[plist.find(&marker)? + marker.len()..];
        let rest = rest.trim_start().strip_prefix("<string>")?;
        let end = rest.find("</string>")?;
        Some(rest[..end].trim().to_owned())
    }
    let label = string_after(info_plist, "CFBundleGetInfoString")
        .or_else(|| {
            string_after(info_plist, "CFBundleShortVersionString")
                .map(|core| format!("Koru {core}"))
        })
        .unwrap_or_else(|| "an unknown version of Koru".to_owned());
    let build_number = string_after(info_plist, "CFBundleVersion").and_then(|s| s.parse().ok());
    InstalledCopy {
        label,
        build_number,
    }
}

/// What the disk-image dialog offers when `/Applications/Koru.app` exists.
#[derive(Debug, PartialEq, Eq)]
pub enum InstalledOffer {
    /// The installed copy is this build or a newer one: open it instead.
    Open,
    /// The installed copy is older than the one on the image. Opening it
    /// would start the old version — seen 2026-10-18, when the dialog sent a
    /// person from the 0.1.2 image straight into a 0.0.2 beta still sitting
    /// in Applications, whose failure then read as the release being broken.
    /// The image's copy must replace it, not defer to it.
    Replace,
}

/// Build numbers are monotonic across every channel of the product (the
/// publisher's shared counter), so one comparison answers for stable, beta
/// and nightly alike. A copy that states no build number, or a build of ours
/// that has none (a local `dist-local` bundle), cannot be ranked and keeps
/// the original offer rather than accusing an installation it cannot read.
pub fn installed_offer(ours: u64, installed: Option<u64>) -> InstalledOffer {
    match installed {
        Some(theirs) if ours > 0 && theirs < ours => InstalledOffer::Replace,
        _ => InstalledOffer::Open,
    }
}

/// The disk-image escape hatch, macOS only. Answers whether launch should
/// continue: `false` means this process must exit — either the person chose
/// the installed copy, which has been asked to open, or they chose to quit
/// and replace an older installed copy by hand — before it competes with
/// that copy for the channel's agent, gateway and store.
///
/// Running from the image *works*; that is the trap. Every such launch is an
/// anonymous extra installation sharing the channel's state with the real
/// one, which is exactly how "multiple versions competing with each other"
/// feels from the outside (docs/architecture/api/MAC.md, 2026-08-24). With an
/// installed copy present the dialog offers it — unless that copy is older
/// than this one, when it says so and asks for the drag instead; without one
/// it explains the drag and lets the person continue deliberately.
pub fn continue_past_disk_image_check() -> bool {
    if !cfg!(target_os = "macos") {
        return true;
    }
    let Ok(exe) = std::env::current_exe() else {
        return true;
    };
    let dir_writable = exe.parent().is_some_and(writable);
    if !is_disk_image_launch(&exe, dir_writable) {
        return true;
    }
    let installed = Path::new("/Applications/Koru.app");
    if installed.exists() {
        let ours = koru_channel::build_info();
        let copy = std::fs::read_to_string(installed.join("Contents/Info.plist"))
            .map(|plist| parse_installed_copy(&plist))
            .unwrap_or_else(|_| parse_installed_copy(""));
        match installed_offer(ours.build_number, copy.build_number) {
            InstalledOffer::Replace => {
                tracing::warn!(
                    target: "koru::startup",
                    installed = %copy.label,
                    installed_build = ?copy.build_number,
                    ours = ours.version,
                    ours_build = ours.build_number,
                    "launched from the disk image beside an older installed Koru"
                );
                let quit_label = "Quit".to_string();
                let result = rfd::MessageDialog::new()
                    .set_title("Koru")
                    .set_level(rfd::MessageLevel::Warning)
                    .set_description(format!(
                        "Koru is running from the disk image, and the copy in \
                         Applications is older: it is {}, this is Koru {}.\n\n\
                         Drag this Koru to Applications, choose Replace, eject the \
                         image and open it from there. Opening the installed copy \
                         would start the old version.",
                        copy.label, ours.version
                    ))
                    .set_buttons(rfd::MessageButtons::OkCancelCustom(
                        quit_label.clone(),
                        "Continue here".into(),
                    ))
                    .show();
                if matches!(result, rfd::MessageDialogResult::Custom(label) if label == quit_label)
                {
                    return false;
                }
            }
            InstalledOffer::Open => {
                let open_label = "Open the installed Koru".to_string();
                let result = rfd::MessageDialog::new()
                    .set_title("Koru")
                    .set_level(rfd::MessageLevel::Warning)
                    .set_description(
                        "Koru is running from the disk image, not from Applications.\n\n\
                         Two copies of the same channel compete for one set of data. Use \
                         the installed Koru instead — or drag this one to Applications \
                         first and eject the image.",
                    )
                    .set_buttons(rfd::MessageButtons::OkCancelCustom(
                        open_label.clone(),
                        "Continue here".into(),
                    ))
                    .show();
                if matches!(result, rfd::MessageDialogResult::Custom(label) if label == open_label)
                {
                    let _ = std::process::Command::new("open").arg(installed).spawn();
                    return false;
                }
            }
        }
    } else {
        rfd::MessageDialog::new()
            .set_title("Koru")
            .set_level(rfd::MessageLevel::Warning)
            .set_description(
                "Koru is running from the disk image.\n\n\
                 Drag Koru to Applications and open it from there: a copy run \
                 from the image is temporary, and it competes for this \
                 channel's data with any installed one.",
            )
            .set_buttons(rfd::MessageButtons::Ok)
            .show();
    }
    true
}

pub fn fatal(message: &str) {
    // **To the log as well as the dialog, and that is not belt-and-braces.**
    // A dialog is dismissed and gone; a release build has no console to have
    // read the `eprintln` in. So a launch that died here left the log ending
    // mid-startup with no reason in it anywhere — which is exactly how a real
    // report arrived: three failed launches, and the last line of all three
    // was the GPU coming up.
    tracing::error!(target: "koru::startup", "launch failed: {message}");
    eprintln!("error: {message}");
    rfd::MessageDialog::new()
        .set_title("Koru")
        .set_level(rfd::MessageLevel::Error)
        .set_description(message)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

/// Say something went wrong, when there is no window to say it in.
///
/// The fallback behind [`ErrorReport`](crate::ErrorReport): a report goes to the
/// pane, which can show the details and offer a way out, and only lands here if
/// no pane would take it. Same dialog as [`fatal`] and a different name on
/// purpose — this one is not about a launch that cannot continue.
pub fn warn(message: &str) {
    tracing::warn!(target: "koru::startup", "{message}");
    eprintln!("warning: {message}");
    rfd::MessageDialog::new()
        .set_title("Koru")
        .set_level(rfd::MessageLevel::Warning)
        .set_description(message)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The packager's one-line `<key>…</key><string>…</string>` and a
    /// re-serialized plist with whitespace between them both parse; a bundle
    /// without the stamps is "unknown", never a build of zero.
    #[test]
    fn an_installed_copy_is_read_from_its_info_plist() {
        let packaged = "<dict>\n    <key>CFBundleShortVersionString</key><string>0.0.2</string>\n    \
                        <key>CFBundleVersion</key><string>35</string>\n    \
                        <key>CFBundleGetInfoString</key><string>Koru 0.0.2-beta.8</string>\n</dict>";
        assert_eq!(
            parse_installed_copy(packaged),
            InstalledCopy {
                label: "Koru 0.0.2-beta.8".into(),
                build_number: Some(35)
            }
        );
        let reserialized = "<key>CFBundleVersion</key>\n\t<string>106</string>\n\
                            <key>CFBundleShortVersionString</key>\n\t<string>0.1.2</string>";
        assert_eq!(
            parse_installed_copy(reserialized),
            InstalledCopy {
                label: "Koru 0.1.2".into(),
                build_number: Some(106)
            }
        );
        assert_eq!(
            parse_installed_copy("<key>CFBundleName</key><string>Koru</string>"),
            InstalledCopy {
                label: "an unknown version of Koru".into(),
                build_number: None
            }
        );
    }

    /// Only an installed copy that is provably older is refused; equal, newer
    /// and unrankable ones keep the offer to open them.
    #[test]
    fn an_older_installed_copy_is_replaced_rather_than_opened() {
        assert_eq!(installed_offer(106, Some(35)), InstalledOffer::Replace);
        assert_eq!(installed_offer(106, Some(106)), InstalledOffer::Open);
        assert_eq!(installed_offer(106, Some(120)), InstalledOffer::Open);
        assert_eq!(installed_offer(106, None), InstalledOffer::Open);
        // A local bundle has no build number of its own to rank against.
        assert_eq!(installed_offer(0, Some(35)), InstalledOffer::Open);
    }

    /// The translocated mirror and the mounted image are launch locations, not
    /// installations; an app on a writable external drive is an installation.
    #[test]
    fn a_disk_image_launch_is_recognized_and_an_installed_one_is_not() {
        let translocated = Path::new(
            "/private/var/folders/x/T/AppTranslocation/ABC/d/Koru.app/Contents/MacOS/koru",
        );
        assert!(is_disk_image_launch(translocated, false));
        // Translocation alone decides: the mirror is read-only anyway, but the
        // path is the stronger signal.
        assert!(is_disk_image_launch(translocated, true));
        let mounted = Path::new("/Volumes/Koru/Koru.app/Contents/MacOS/koru");
        assert!(is_disk_image_launch(mounted, false));
        // An external drive mounts under /Volumes too — writability is what
        // tells a real installation there from the read-only image.
        assert!(!is_disk_image_launch(mounted, true));
        assert!(!is_disk_image_launch(
            Path::new("/Applications/Koru.app/Contents/MacOS/koru"),
            true
        ));
        assert!(!is_disk_image_launch(
            Path::new("/Users/dev/my-project/target/debug/koru"),
            true
        ));
    }

    /// Development registry selection keeps worktree runs isolated. Release
    /// builds bypass this helper and always use the per-user location.
    #[test]
    fn the_registry_picks_the_first_writable_candidate() {
        let exe = PathBuf::from("C:/app/profiles.conf");
        let local = PathBuf::from("C:/Local/koru/profiles.conf");
        let candidates = vec![exe.clone(), local.clone()];
        // Exe dir writable -> exe-level wins.
        assert_eq!(pick_registry_path(&candidates, |_| true), Some(exe));
        // Exe dir not writable -> the fallback wins.
        assert_eq!(
            pick_registry_path(&candidates, |d| d != Path::new("C:/app")),
            Some(local)
        );
        // Nothing writable -> the first candidate anyway, deterministically.
        assert_eq!(
            pick_registry_path(&candidates, |_| false),
            Some(PathBuf::from("C:/app/profiles.conf"))
        );
    }

    #[test]
    fn registry_migration_merges_without_replacing_a_newer_record() {
        let id = koru_core::ProfileId::new();
        let another = koru_core::ProfileId::new();
        let current = registry::ProfileRecord {
            id,
            name: "Current".into(),
            email: None,
            last_opened: Some("20260804-120000".into()),
            dir: PathBuf::from("C:/Current"),
        };
        let stale = registry::ProfileRecord {
            id,
            name: "Stale".into(),
            email: None,
            last_opened: Some("20260801-120000".into()),
            dir: PathBuf::from("C:/Stale"),
        };
        let old_only =
            registry::ProfileRecord::new(another, "Legacy", None, Path::new("C:/Legacy"));
        let merged = merge_registry_text(
            &registry::body(&[current], Some(id)),
            &registry::body(&[stale, old_only], Some(another)),
        );
        let records = registry::parse(&merged);
        assert_eq!(registry::find_by_id(&records, id).unwrap().name, "Current");
        assert!(registry::find_by_id(&records, another).is_some());
        assert_eq!(registry::selected_id(&merged), Some(id));
    }

    /// `--profile <path>` adopts a folder that is already there; a bare name or a
    /// missing path is left for the name/id lookup.
    #[test]
    fn a_profile_flag_pointing_at_a_folder_adopts_it() {
        // A bare name is never treated as a path, even by coincidence — a
        // separator (or an absolute path) is what makes a needle a path.
        assert_eq!(existing_path("Work"), None);
        // A path that is not there is not adopted.
        assert_eq!(existing_path("C:/definitely/not/here"), None);

        // An existing absolute folder is adopted, exactly as written.
        let tmp = std::env::temp_dir().join(format!("koru-profile-flag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        assert_eq!(existing_path(&tmp.to_string_lossy()), Some(tmp.clone()));

        // A relative path with a separator is recognised too (resolved by the
        // OS, so this asserts recognition, not a canonical form).
        let sub = tmp.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&tmp).unwrap();
        assert_eq!(existing_path("./sub"), Some(PathBuf::from("./sub")));
        std::env::set_current_dir(&cwd).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ------------------------------------------------------------- profiles

    fn rec(n: u8, name: &str) -> registry::ProfileRecord {
        registry::ProfileRecord::new(
            koru_core::ProfileId::new(),
            name,
            None,
            Path::new(&format!("C:/Koru/{n}")),
        )
    }

    /// Paging arithmetic is where an off-by-one hides a profile somebody then
    /// cannot reach at all — so every one has to appear on exactly one page.
    #[test]
    fn every_profile_appears_on_exactly_one_page_of_the_picker() {
        for n in 0..9u8 {
            let records: Vec<_> = (0..n).map(|i| rec(i, &format!("p{i}"))).collect();
            let pages = pages(&records);
            let mut seen: Vec<usize> = pages
                .iter()
                .flat_map(|p| p.iter().flatten().copied())
                .collect();
            seen.sort_unstable();
            assert_eq!(seen, (0..n as usize).collect::<Vec<_>>(), "{n} profiles");
            // Two per dialog, so a page is never empty and never over-full.
            for p in &pages {
                assert!(p[0].is_some(), "an empty page at {n}");
            }
            assert_eq!(pages.len(), (n as usize).div_ceil(2));
        }
    }

    /// An instruction on the command line is not overridden by a held key.
    /// `--profile` is resolved before the picker is ever considered, so Shift
    /// cannot override an explicit choice.
    #[test]
    fn an_explicit_profile_flag_beats_a_held_shift() {
        assert!(!wants_profile_picker(true), "--profile was typed; it wins");
    }

    /// One profile has no question in it, and a dialog with one button is a
    /// dialog that only wastes a launch.
    #[test]
    fn the_picker_is_skipped_when_there_is_only_one_profile() {
        // Whatever this machine's registry holds, the rule is the same: below
        // two, there is nothing to ask.
        let n = profiles().len();
        if n < 2 {
            assert!(!wants_profile_picker(false));
        }
        // And the paging agrees: nothing to page through.
        assert!(pages(&[]).is_empty());
        assert_eq!(pages(&[rec(1, "only")]).len(), 1);
    }

    /// The three flags that name a profile, in both spellings.
    #[test]
    fn the_profile_flags_parse_in_both_spellings() {
        let args = |list: &[&str]| -> Vec<String> {
            std::iter::once("koru.exe")
                .chain(list.iter().copied())
                .map(str::to_string)
                .collect()
        };
        assert_eq!(
            cli::profile_from(args(&["--profile", "Work"])),
            Some("Work".into())
        );
        assert_eq!(
            cli::profile_from(args(&["--profile=Work"])),
            Some("Work".into())
        );
        assert_eq!(cli::profile_from(args(&["--profile", "  "])), None);
        // `--profiles` is its own command and must not be read as `--profile s`.
        assert_eq!(cli::profile_from(args(&["--profiles"])), None);
        assert_eq!(cli::profile_from(args(&[])), None);

        assert!(cli::pick_profile_from(args(&["--pick-profile"])));
        assert!(!cli::pick_profile_from(args(&["--profile", "Work"])));
        assert!(!cli::pick_profile_from(args(&[])));
    }

    /// `--root` is the store's, and it is not `--profile` wearing a hat.
    ///
    /// **The two live side by side and mean unrelated things**: `--profile`
    /// names content, `--root` names the store that content is kept in. A
    /// launch may give either, both, or neither, so the parse of one must not
    /// be reachable from the other.
    #[test]
    fn the_store_root_parses_in_both_spellings_and_is_not_the_profile() {
        let args = |list: &[&str]| -> Vec<String> {
            std::iter::once("koru.exe")
                .chain(list.iter().copied())
                .map(str::to_string)
                .collect()
        };
        let p = |s: &str| Some(std::path::PathBuf::from(s));

        // **`--root` names a channel root; the store is `store/` inside it.**
        // The raw form is what the agent is given — its endpoint is derived from
        // the outer directory — and the joined form is what dials a store. They
        // were once the same value, which is how a rooted store came to put its
        // databases around the agent's own files.
        assert_eq!(cli::root_from(args(&["--root", "C:/dev"])), p("C:/dev"));
        assert_eq!(
            cli::store_root_from(args(&["--root", "C:/dev"])),
            p("C:/dev/store")
        );
        assert_eq!(
            cli::store_root_from(args(&["--root=C:/dev"])),
            p("C:/dev/store")
        );
        // Whitespace is not a path, and a flag that was meant to point
        // somewhere and did not is the case where carrying on is dangerous —
        // `None` here means the shared store, which is the honest answer.
        assert_eq!(cli::store_root_from(args(&["--root", "  "])), None);
        assert_eq!(cli::store_root_from(args(&[])), None);
        assert_eq!(cli::root_from(args(&[])), None);

        // Neither flag may be read as the other, in either direction.
        assert_eq!(cli::store_root_from(args(&["--profile", "C:/work"])), None);
        assert_eq!(cli::profile_from(args(&["--root", "C:/dev"])), None);
        assert_eq!(
            cli::store_root_from(args(&["--profile", "Work", "--root", "C:/dev"])),
            p("C:/dev/store")
        );
        assert_eq!(
            cli::profile_from(args(&["--profile", "Work", "--root", "C:/dev"])),
            Some("Work".into())
        );
    }

    /// A relaunch names the folder and **no peer**: the new process auto-names
    /// its own replica, which is exactly what makes two profile windows safe to
    /// run at once.
    #[test]
    fn a_relaunch_names_the_folder_and_no_peer() {
        let dir = Path::new("C:/Koru/Work");
        let plain = relaunch_args(dir, None);
        assert_eq!(
            plain,
            vec!["--profile".to_string(), dir.display().to_string()]
        );
        assert!(!plain.iter().any(|a| a.starts_with("--peer")));

        let id = koru_core::WorkspaceId::new();
        let with_ws = relaunch_args(dir, Some(id));
        assert_eq!(with_ws[2], "--workspace");
        assert_eq!(with_ws[3], id.to_storage());
    }

    /// **Every launcher that opens another window carries the channel root.**
    ///
    /// *Open in New Window* on a workspace did not, so in root mode the child
    /// opened the default channel's store instead of the one its parent was
    /// showing: half the clicks put up "could not reach the Koru Store" and
    /// the other half opened the wrong content, depending on whether a default
    /// store happened to be running. Asserted together with its siblings
    /// because the omission is invisible one launcher at a time.
    #[test]
    fn every_relaunch_that_opens_a_window_carries_the_channel_root() {
        let dir = Path::new("C:/Koru/settings");
        let root = Path::new("C:/Koru/root");
        let workspace = koru_core::WorkspaceId::new();
        let profile = koru_core::ProfileId::new();

        let carries_root = |args: &[String]| {
            args.windows(2)
                .any(|pair| pair[0] == "--root" && pair[1] == root.display().to_string())
        };

        let workspace_window = workspace_relaunch_args(dir, Some(workspace), Some(root));
        assert!(
            carries_root(&workspace_window),
            "a workspace opened in a new window lost the root: {workspace_window:?}"
        );
        assert!(
            carries_root(&store_profile_relaunch_args(dir, profile, Some(root))),
            "a profile relaunch lost the root"
        );
        assert!(
            carries_root(&target_args(
                dir,
                profile,
                workspace,
                None,
                None,
                None,
                true,
                Some(root)
            )),
            "a search target in a new window lost the root"
        );

        // No root to carry is the ordinary installed case, and must not invent
        // one — a `--root` naming nothing would be worse than none at all.
        assert!(
            !workspace_relaunch_args(dir, Some(workspace), None)
                .iter()
                .any(|a| a == "--root"),
            "a rootless launch must not name a root"
        );
    }

    #[test]
    fn a_store_profile_relaunch_carries_identity_and_root() {
        let profile = koru_core::ProfileId::new();
        assert_eq!(
            store_profile_relaunch_args(
                Path::new("C:/Koru/settings"),
                profile,
                Some(Path::new("C:/Koru/root")),
            ),
            vec![
                "--profile".to_string(),
                "C:/Koru/settings".to_string(),
                "--store-profile".to_string(),
                profile.to_storage(),
                "--root".to_string(),
                "C:/Koru/root".to_string(),
            ]
        );
    }

    #[test]
    fn a_forced_search_window_carries_live_and_trash_targets() {
        let workspace = koru_core::WorkspaceId::new();
        let document = koru_core::DocumentId::new();
        let node = koru_core::NodeId::new();
        let trash = koru_core::NodeId::new();
        let args = target_args(
            Path::new("C:/Koru profile"),
            koru_core::ProfileId::parse("30259104-220c-4d97-9921-6deb94ee3b12")
                .expect("valid profile id"),
            workspace,
            Some(document),
            Some(node),
            Some(trash),
            true,
            Some(Path::new("C:/Koru root")),
        );

        assert_eq!(
            args,
            vec![
                "--profile".to_string(),
                "C:/Koru profile".to_string(),
                "--store-profile".to_string(),
                "30259104-220c-4d97-9921-6deb94ee3b12".to_string(),
                "--root".to_string(),
                "C:/Koru root".to_string(),
                "--workspace".to_string(),
                workspace.to_storage(),
                "--open-document".to_string(),
                document.to_storage(),
                "--open".to_string(),
                node.to_storage(),
                "--open-trash".to_string(),
                trash.to_storage(),
                "--new-window".to_string(),
            ]
        );

        let mut command = std::process::Command::new("koru");
        set_search_match_environment(&mut command, Some("emoji 🤝"));
        assert_eq!(
            command
                .get_envs()
                .find(|(name, _)| *name == SEARCH_MATCH_ENV)
                .and_then(|(_, value)| value)
                .and_then(|value| value.to_str()),
            Some("emoji 🤝")
        );
    }

    /// The list lives beside the exe (or its per-user fallback), never inside a
    /// profile folder — which is why it must never be the only record of a
    /// profile, and why every folder describes itself.
    #[test]
    fn the_registry_path_points_at_profiles_conf() {
        if let Some(p) = registry_path() {
            assert!(p.is_absolute(), "{p:?}");
            assert!(p.ends_with(registry::REGISTRY_FILE), "{p:?}");
        }
    }

    #[test]
    fn a_profile_new_window_keeps_the_channel_root_but_not_the_source_workspace() {
        let profile = koru_core::ProfileId::new();
        let args = profile_new_window_args(
            Path::new("local-profile"),
            profile,
            Some(Path::new("isolated-root")),
        );
        assert!(
            args.windows(2)
                .any(|p| p == ["--store-profile", &profile.to_storage()])
        );
        assert!(args.windows(2).any(|p| p == ["--root", "isolated-root"]));
        assert_eq!(args.last().map(String::as_str), Some("--new-window"));
        assert!(
            !args
                .iter()
                .any(|arg| matches!(arg.as_str(), "--workspace" | "--open" | "--node"))
        );
    }

    #[test]
    fn the_store_profile_marker_is_beside_the_install_registry() {
        if let Some(path) = store_profile_selection_path() {
            assert!(path.is_absolute(), "{path:?}");
            assert!(path.ends_with(STORE_PROFILE_SELECTION_FILE), "{path:?}");
        }
    }

    /// A first run under the store never asks for a folder: the scaffold sits
    /// beside the registry, the same place the install already writes machine-
    /// local markers.
    #[test]
    fn the_local_scaffold_sits_beside_the_registry() {
        let Some(registry) = registry_path() else {
            return;
        };
        let Some(scaffold) = local_scaffold_folder() else {
            panic!("scaffold should resolve whenever the registry does");
        };
        assert_eq!(scaffold.parent(), registry.parent());
        assert!(scaffold.ends_with("local-profile"), "{scaffold:?}");
        assert!(scaffold.is_dir(), "{scaffold:?}");
    }

    #[test]
    fn a_store_profile_marker_round_trips_only_a_valid_identity() {
        let profile = koru_core::ProfileId::new();
        assert_eq!(
            parse_store_profile_selection(&format!("{}\n", profile.to_storage())),
            Some(profile)
        );
        assert_eq!(parse_store_profile_selection("not-a-profile\n"), None);
    }
}

// ------------------------------------------------------------------ profiles

/// The list of profiles this install knows about.
///
/// Beside the executable when that directory is writable — a portable app keeps
/// its index with the binary — and in the per-user data directory
/// (`%LOCALAPPDATA%\koru` on Windows) when it is not, e.g. an install in
/// Program Files. Read and write use the same path, resolved once.
fn registry_path() -> Option<PathBuf> {
    static REGISTRY: OnceLock<Option<PathBuf>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| {
            if cfg!(debug_assertions) {
                pick_registry_path(&registry_candidates(), writable)
            } else {
                let destination = local_data_base()?
                    .join("koru")
                    .join(registry::REGISTRY_FILE);
                if let Some(legacy) = exe_dir().map(|dir| dir.join(registry::REGISTRY_FILE)) {
                    migrate_legacy_registry(&legacy, &destination);
                }
                Some(destination)
            }
        })
        .clone()
}

const STORE_PROFILE_SELECTION_FILE: &str = "store-profile.conf";

/// The Store profile a plain launch should reopen. This is deliberately a
/// one-value machine-local marker rather than part of ordinary `Settings`:
/// several profile windows may remain open, and an older window saving its
/// unrelated layout must not overwrite the profile selected more recently.
fn store_profile_selection_path() -> Option<PathBuf> {
    registry_path().map(|path| path.with_file_name(STORE_PROFILE_SELECTION_FILE))
}

pub fn selected_store_profile() -> Option<koru_core::ProfileId> {
    let raw = std::fs::read_to_string(store_profile_selection_path()?).ok()?;
    parse_store_profile_selection(&raw)
}

fn parse_store_profile_selection(raw: &str) -> Option<koru_core::ProfileId> {
    koru_core::ProfileId::parse(raw.trim())
}

/// Stamp the profile that actually opened. Only profile selection writes this
/// file, so a still-running window on another profile cannot steal the next
/// cold launch merely by persisting its tabs or geometry later.
pub fn remember_store_profile(profile: koru_core::ProfileId) {
    let Some(path) = store_profile_selection_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(error) = std::fs::write(path, format!("{}\n", profile.to_storage())) {
        eprintln!("warning: could not remember the selected Store profile ({error})");
    }
}

/// Clear the machine-local launch hint only when it still names the removed
/// Profile. A newer Profile selection wins a concurrent cleanup.
pub fn forget_store_profile(profile: koru_core::ProfileId) -> std::io::Result<()> {
    let Some(path) = store_profile_selection_path() else {
        return Ok(());
    };
    if selected_store_profile() == Some(profile) && path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}
/// Merge the old portable registry into the per-user one once. The old copy is
/// preserved so rollback to an older Koru still has its index.
fn migrate_legacy_registry(legacy: &Path, destination: &Path) {
    if legacy == destination || !legacy.is_file() {
        return;
    }
    let Some(parent) = destination.parent() else {
        return;
    };
    let marker = parent.join(format!(
        ".profiles-migrated-{}",
        koru_update::install_key(legacy.parent().unwrap_or(parent))
    ));
    if marker.is_file() {
        return;
    }
    let old = match std::fs::read_to_string(legacy) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("warning: could not read the legacy profile list ({error})");
            return;
        }
    };
    let current = std::fs::read_to_string(destination).unwrap_or_default();
    let body = merge_registry_text(&current, &old);
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let temporary = parent.join(format!(".profiles-migration-{}.tmp", std::process::id()));
    let written = std::fs::write(&temporary, body)
        .and_then(|_| {
            if destination.exists() {
                std::fs::copy(&temporary, destination).map(|_| ())
            } else {
                std::fs::rename(&temporary, destination)
            }
        })
        .and_then(|_| std::fs::write(&marker, b"migrated; legacy copy retained\n"));
    let _ = std::fs::remove_file(&temporary);
    if let Err(error) = written {
        eprintln!("warning: could not migrate the profile list ({error})");
    }
}

fn merge_registry_text(current: &str, legacy: &str) -> String {
    let mut records = registry::parse(current);
    for old in registry::parse(legacy) {
        let replace = registry::find_by_id(&records, old.id)
            .is_none_or(|existing| existing.last_opened < old.last_opened);
        if replace {
            registry::upsert(&mut records, old);
        }
    }
    let selected = registry::selected_id(current).or_else(|| registry::selected_id(legacy));
    registry::body(&records, selected)
}

/// The candidate locations, most-preferred first: beside the exe, then the
/// per-user fallback.
fn registry_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(exe) = exe_dir() {
        v.push(exe.join(registry::REGISTRY_FILE));
    }
    if let Some(base) = local_data_base() {
        v.push(base.join("koru").join(registry::REGISTRY_FILE));
    }
    v
}

/// The first candidate whose parent is writable; the first candidate if none
/// are, so the write fails loudly in one place rather than being silently lost.
/// Pure but for the predicate, so the selection is testable without disk.
fn pick_registry_path(
    candidates: &[PathBuf],
    is_writable: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    candidates
        .iter()
        .find(|p| p.parent().is_some_and(|parent| is_writable(parent)))
        .cloned()
        .or_else(|| candidates.first().cloned())
}

/// The directory the executable lives in.
fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|p| p.to_path_buf())
}

/// The per-machine fallback root: `%LOCALAPPDATA%` on Windows,
/// `$XDG_DATA_HOME` or `~/.local/share` elsewhere.
///
/// **Deliberately not given a macOS arm under ADR-116.** This resolver serves
/// the profile registry, and CUTOVER_STATUS (deleted) item E deletes this whole file
/// (and `--profile` with it) — migrating a doomed path would orphan existing
/// dev-Mac registries for zero lasting value.
fn local_data_base() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    }
}

/// Whether a directory can be written to. Probes by writing a temp file when it
/// exists, and by creating it when it does not (the fallback root may not exist
/// yet). Called once, via [`registry_path`].
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".koru-probe-{}", std::process::id()));
    if std::fs::File::create(&probe).is_ok_and(|_| std::fs::remove_file(&probe).is_ok()) {
        return true;
    }
    // Not directly writable. If it doesn't exist, creating it may be enough
    // (the fallback root); otherwise it genuinely is read-only.
    !dir.exists() && std::fs::create_dir_all(dir).is_ok()
}

/// The folder of the profile a plain launch should reopen, if the registry names
/// one and it is still there. `None` otherwise — never a guess.
fn selected_folder() -> Option<PathBuf> {
    let path = registry_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let id = registry::selected_id(&raw)?;
    let records = registry::parse(&raw);
    let rec = registry::find_by_id(&records, id)?;
    rec.dir.exists().then(|| rec.dir.clone())
}

/// Read the index. An unreadable file is an empty list, never an error: it is
/// only an index, and every folder it names also describes itself.
pub fn profiles() -> Vec<registry::ProfileRecord> {
    registry_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| registry::parse(&t))
        .unwrap_or_default()
}

/// Record that this profile was opened, so the switcher and the picker can list
/// it — and so `--profile <name>` can find it.
///
/// Re-reads immediately before writing, because several profile windows run at
/// once and each stamps itself. The worst case is a lost timestamp; nothing
/// content-bearing is in this file.
pub fn remember_profile(dir: &Path, profile: &koru_core::profile::ProfileDoc) {
    let Some(id) = profile.id() else { return };
    let Some(path) = registry_path() else { return };
    let mut records = profiles();
    registry::upsert(
        &mut records,
        registry::ProfileRecord {
            id,
            name: profile.name().unwrap_or_default(),
            email: profile.email(),
            last_opened: Some(koru_docstore::backup::stamp_now()),
            dir: dir.to_path_buf(),
        },
    );
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&path, registry::body(&records, Some(id))) {
        eprintln!("warning: could not update the profile list ({e})");
    }
}

/// Tell the user about anything unusual in how the *profile* opened, and about
/// an adoption.
///
/// Separate from [`announce`] because the two documents fail independently: a
/// workspace can be perfectly fine while the list of workspaces had to be
/// recovered, and saying "your workspace was recovered" about that would send
/// somebody looking at the wrong thing.
pub fn announce_profile(dir: &Path, report: &Report, adopted: usize) {
    if adopted > 0 {
        // Not a warning: nothing went wrong, and nothing moved. But a folder
        // that has quietly grown a `profile/` directory is exactly the kind of
        // change that gets noticed a week later and misread as damage.
        println!(
            "[Koru] adopted {adopted} workspace(s) already in this folder into a profile; \
             nothing in workspaces/ was moved or rewritten"
        );
    }
    if report.is_quiet() {
        return;
    }
    let mut body = String::new();
    if let Some(r) = &report.recovery {
        body.push_str(&format!(
            "The list of workspaces in this profile could not be read ({}).\n\n",
            r.problem
        ));
        match &r.from {
            Some(p) => body.push_str(&format!("Recovered from a backup:\n{}\n\n", p.display())),
            None => body.push_str(
                "There was no usable backup. Your workspaces are all still on \
                 disk — they have been listed again from the folder.\n\n",
            ),
        }
    }
    if let Some(why) = &report.read_only {
        body.push_str(&format!(
            "{why}.\n\nWorkspaces still open normally; this session will not \
             add, rename or remove one.\n\n"
        ));
    }
    body.push_str(&format!("Profile: {}", dir.display()));

    eprintln!("[Koru] {}", body.replace('\n', " "));
    rfd::MessageDialog::new()
        .set_title("Koru")
        .set_level(rfd::MessageLevel::Warning)
        .set_description(&body)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

/// Where a profile in the list lives, if it is still listed.
///
/// `None` for one that has been removed — never a guess. The registry is the
/// index; a folder it does not name is a folder nothing here knows about.
pub fn profile_dir(id: koru_core::ProfileId) -> Option<PathBuf> {
    registry::find_by_id(&profiles(), id).map(|r| r.dir.clone())
}

/// Start another Koru on `dir`, optionally on a particular workspace.
///
/// **No `--peer` is passed**, and that is the whole reason several profile
/// windows are safe to run at once: the new process auto-names its own replica,
/// so the two never append to one update file — and they share the `default`
/// window layout, which is what they already did.
pub fn launch(dir: &Path, workspace: Option<koru_core::WorkspaceId>) {
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("error: could not find this executable to launch another window");
        return;
    };
    let channel_root = cli::root_from_args();
    let mut cmd = std::process::Command::new(exe);
    for arg in workspace_relaunch_args(dir, workspace, channel_root.as_deref()) {
        cmd.arg(arg);
    }
    // A diagnostic launch should stay diagnostic when opening another profile
    // creates a child process. This remains process-local and never enters
    // profile or workspace settings.
    if std::env::args().any(|arg| arg == "--no-search") {
        cmd.arg("--no-search");
    }
    suppress_console(&mut cmd);
    match cmd.spawn() {
        // The child focuses its first pane on `resumed`; grant that process
        // permission before it can create the window, or Windows is entitled
        // to leave the new workspace behind the one it was opened from — which
        // is indistinguishable from the click having done nothing.
        Ok(child) => allow_child_foreground(child.id()),
        Err(e) => fatal(&format!("Koru could not open another window:\n\n{e}")),
    }
}

/// Open a stable search target in a new OS window even when an existing Koru
/// process owns the profile. `--new-window` bypasses the ordinary activation
/// handoff; without it, `--open` is intentionally delivered to that existing
/// process and Ctrl+Enter would look identical to Enter.
pub fn launch_target_new_window(
    dir: &Path,
    profile: koru_core::ProfileId,
    workspace: koru_core::WorkspaceId,
    document: Option<koru_core::DocumentId>,
    node: Option<koru_core::NodeId>,
    trash_root: Option<koru_core::NodeId>,
    match_text: Option<&str>,
) {
    let Ok(exe) = std::env::current_exe() else {
        crate::search_debug!(
            "[Koru][search-open][pid={}] process launch failed before spawn: workspace={} node={} trash_root={} error=current_exe",
            std::process::id(),
            workspace.to_storage(),
            node.map(|id| id.to_storage()).as_deref().unwrap_or("-"),
            trash_root
                .map(|id| id.to_storage())
                .as_deref()
                .unwrap_or("-"),
        );
        return;
    };
    let channel_root = cli::root_from_args();
    let mut cmd = std::process::Command::new(exe);
    for arg in target_args(
        dir,
        profile,
        workspace,
        document,
        node,
        trash_root,
        true,
        channel_root.as_deref(),
    ) {
        cmd.arg(arg);
    }
    set_search_match_environment(&mut cmd, match_text);
    suppress_console(&mut cmd);
    match cmd.spawn() {
        Ok(child) => {
            // The child explicitly focuses its first pane on `resumed`; grant
            // that exact process permission before it can create the window.
            // Without this Windows is entitled to leave the new workspace
            // behind the search/source window even though this launch was the
            // direct result of Ctrl+click.
            allow_child_foreground(child.id());
            crate::search_debug!(
                "[Koru][search-open][pid={}] process launch complete: child_pid={} workspace={} node={} trash_root={}",
                std::process::id(),
                child.id(),
                workspace.to_storage(),
                node.map(|id| id.to_storage()).as_deref().unwrap_or("-"),
                trash_root
                    .map(|id| id.to_storage())
                    .as_deref()
                    .unwrap_or("-"),
            );
        }
        Err(error) => {
            crate::search_debug!(
                "[Koru][search-open][pid={}] process launch failed: workspace={} node={} trash_root={} error={error}",
                std::process::id(),
                workspace.to_storage(),
                node.map(|id| id.to_storage()).as_deref().unwrap_or("-"),
                trash_root
                    .map(|id| id.to_storage())
                    .as_deref()
                    .unwrap_or("-"),
            );
            fatal(&format!(
                "Koru could not open the search result:\n\n{error}"
            ));
        }
    }
}

/// A separate Profile window resolves its own remembered workspace at startup.
/// Never inherit the source window's workspace or search target.
pub fn launch_profile_new_window(dir: &Path, profile: koru_core::ProfileId) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    let channel_root = cli::root_from_args();
    cmd.args(profile_new_window_args(
        dir,
        profile,
        channel_root.as_deref(),
    ));
    set_search_match_environment(&mut cmd, None);
    suppress_console(&mut cmd);
    let child = cmd.spawn()?;
    allow_child_foreground(child.id());
    Ok(())
}

fn profile_new_window_args(
    dir: &Path,
    profile: koru_core::ProfileId,
    channel_root: Option<&Path>,
) -> Vec<String> {
    let mut args = store_profile_relaunch_args(dir, profile, channel_root);
    args.push("--new-window".into());
    args
}

/// A search result opens an application window, never a console alongside it.
///
/// Debug builds are console-subsystem executables, so inheriting the ordinary
/// creation flags flashes a terminal even though all of the child's UI is
/// graphical. `CREATE_NO_WINDOW` suppresses only that console; it does not
/// prevent Koru from creating its winit window.
#[cfg(windows)]
fn suppress_console(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn suppress_console(_command: &mut std::process::Command) {}

#[cfg(windows)]
fn allow_child_foreground(pid: u32) {
    use windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
    let allowed = unsafe { AllowSetForegroundWindow(pid) }.is_ok();
    crate::search_debug!(
        "[Koru][search-open][pid={}] child foreground permission: child_pid={pid} permitted={allowed}",
        std::process::id(),
    );
}

#[cfg(not(windows))]
fn allow_child_foreground(_pid: u32) {}

fn target_args(
    dir: &Path,
    profile: koru_core::ProfileId,
    workspace: koru_core::WorkspaceId,
    document: Option<koru_core::DocumentId>,
    node: Option<koru_core::NodeId>,
    trash_root: Option<koru_core::NodeId>,
    new_window: bool,
    channel_root: Option<&Path>,
) -> Vec<String> {
    let mut args = store_profile_relaunch_args(dir, profile, channel_root);
    args.extend(["--workspace".into(), workspace.to_storage()]);
    if let Some(document) = document {
        args.extend(["--open-document".into(), document.to_storage()]);
    }
    if let Some(node) = node {
        args.extend(["--open".into(), node.to_storage()]);
    }
    if let Some(root) = trash_root {
        args.extend(["--open-trash".into(), root.to_storage()]);
    }
    if new_window {
        args.push("--new-window".into());
    }
    args
}

/// Private one-shot evidence for a child search window. Keeping authored
/// customer text out of argv avoids exposing it in process listings. Removing
/// the inherited value first is important when this process was itself opened
/// from a search result and later launches another window without evidence.
pub const SEARCH_MATCH_ENV: &str = "KORU_SEARCH_MATCH";

fn set_search_match_environment(command: &mut std::process::Command, match_text: Option<&str>) {
    command.env_remove(SEARCH_MATCH_ENV);
    if let Some(match_text) = match_text {
        command.env(SEARCH_MATCH_ENV, match_text);
    }
}

/// The arguments a relaunch passes. The folder travels as `--profile` — a
/// profile is a folder, so naming the folder names the profile, opened and
/// registered on first open. Pure, so the shape is testable without starting
/// anything.
pub fn relaunch_args(dir: &Path, workspace: Option<koru_core::WorkspaceId>) -> Vec<String> {
    let mut out = vec!["--profile".to_string(), dir.display().to_string()];
    if let Some(id) = workspace {
        out.push("--workspace".into());
        out.push(id.to_storage());
    }
    out
}

/// Arguments for *Open in New Window* on a workspace, kept pure for tests.
///
/// **The channel root travels, and forgetting it is the bug this exists to
/// name.** Without `--root` the child resolves the *default* channel store
/// rather than the one its parent is showing, so in root mode the new window
/// opened somebody else's content — or, with no store running there, put up
/// "Koru could not reach the Koru Store" and vanished. It looked like the
/// click worked sometimes and not others, because whether the default store
/// happened to be up decided which.
///
/// The two sibling launchers ([`store_profile_relaunch_args`] and
/// [`target_args`]) already carried it; this path was missed.
pub fn workspace_relaunch_args(
    dir: &Path,
    workspace: Option<koru_core::WorkspaceId>,
    channel_root: Option<&Path>,
) -> Vec<String> {
    let mut out = relaunch_args(dir, workspace);
    if let Some(root) = channel_root {
        out.extend(["--root".into(), root.display().to_string()]);
    }
    out
}

/// Arguments for a Store-profile relaunch, kept pure for regression tests.
pub fn store_profile_relaunch_args(
    dir: &Path,
    profile: koru_core::ProfileId,
    channel_root: Option<&Path>,
) -> Vec<String> {
    let mut out = relaunch_args(dir, None);
    out.extend(["--store-profile".into(), profile.to_storage()]);
    if let Some(root) = channel_root {
        out.extend(["--root".into(), root.display().to_string()]);
    }
    out
}

/// Choose a folder for a new profile.
///
/// The folder question, and only the folder question: name and email are edited
/// in the app afterwards. rfd has no multi-field form, and chaining two prompts
/// before the window exists would put a worse version of the pane's own fields
/// in front of somebody who has not seen the app yet.
///
/// `None` if they backed out, and nothing is created — the same rule the
/// first-run question follows.
pub fn ask_for_a_new_profile_folder() -> Option<PathBuf> {
    let picked = rfd::FileDialog::new()
        .set_title("Koru - a folder for the new profile")
        .pick_folder()?;
    match profilefolder::inspect(&picked) {
        // Already ours: opening it is not creating anything, so it needs no
        // confirmation — and refusing would be refusing to reopen a profile.
        FolderKind::Profile { .. } | FolderKind::Legacy => Some(picked),
        FolderKind::Empty => {
            let confirm = rfd::MessageDialog::new()
                .set_title("Koru - new profile?")
                .set_description(&format!(
                    "A new, empty Koru profile will be created in:\n\n{}\n\n\
                     Everything that profile holds lives in that folder. If you \
                     were looking for a profile you already have, choose No and \
                     pick the folder it is in.",
                    picked.display()
                ))
                .set_buttons(rfd::MessageButtons::YesNo)
                .show();
            (confirm == rfd::MessageDialogResult::Yes).then_some(picked)
        }
    }
}

// -------------------------------------------------- which profile, and asking

/// Which profile folder this launch uses.
///
/// The one entry point, and the order in it is the whole of the policy. Content
/// itself is opened from the store afterwards; this path only feeds the
/// transitional shell (settings, a few leftover folder call sites).
///
/// 1. `--profile <name|id|path>` names one explicitly. The registry is a lookup
///    by name or id; an existing path is adopted as a profile.
/// 2. Holding **Shift**, or `--pick-profile`, asks which — but only when there
///    is a choice to make, and never over an explicit `--profile`.
/// 3. The `last-selected` marker: the profile reopened on a plain launch.
/// 4. The most recently opened profile.
/// 5. None of the above: a machine-local scaffold beside the registry. **No
///    folder question** — the store owns the person's content
///    ([ADR-86](docs/decisions/ADR-076-100.md#adr-86-onboarding--a-first-run-creates-its-profile-and-workspace-through-the-ordinary-api-and-asks-no-folder-question)).
pub fn profile_folder() -> Option<PathBuf> {
    if let Some(needle) = cli::profile_from_args() {
        return named_profile(&needle);
    }

    // The picker runs before any window because there is nowhere to put it:
    // winit permits one event loop per process, and `App` cannot exist with no
    // workspace open.
    if wants_profile_picker(false)
        && let Some(dir) = pick_profile(&by_recency_owned())
    {
        announce_location(&dir);
        return Some(dir);
    }

    if let Some(dir) = selected_folder() {
        announce_location(&dir);
        return Some(dir);
    }

    if let Some(rec) = registry::most_recent(&profiles())
        && rec.dir.exists()
    {
        announce_location(&rec.dir);
        return Some(rec.dir.clone());
    }

    // First start under the store: no question. The scaffold is stamped as
    // `last-selected` when `App::new` opens it, so the next plain launch skips
    // this arm.
    let chosen = local_scaffold_folder()?;
    announce_location(&chosen);
    Some(chosen)
}

/// Resolve `--profile <name|id|path>` through the list.
///
/// Every failure here is a refusal rather than a guess: a name that matches
/// nothing, a name that matches two, or a folder that has gone. Choosing
/// between two profiles somebody might have meant is the same failure as
/// choosing a directory for them. A needle that is an existing path is adopted
/// — a profile is a folder, so naming the folder names the profile.
fn named_profile(needle: &str) -> Option<PathBuf> {
    let records = profiles();
    let dir = registry::find_by_name(&records, needle)
        .or_else(|| {
            koru_core::ProfileId::parse(needle).and_then(|id| registry::find_by_id(&records, id))
        })
        .map(|r| r.dir.clone())
        .or_else(|| existing_path(needle))
        .or_else(|| {
            fatal(&format!(
                "No profile called '{needle}'.\n\nKoru knows about:\n{}\n\n\
                 Run koru --profiles to see them.",
                list_names(&records)
            ));
            None
        })?;
    if !dir.exists() {
        fatal(&format!(
            "That profile's folder is not there:\n\n{}\n\nIf it is on a drive that \
             is not plugged in, or in a cloud folder that has not synced yet, it \
             will come back on its own. Nothing has been deleted.",
            dir.display()
        ));
        return None;
    }
    announce_location(&dir);
    Some(dir)
}

/// `--profile <path>`: adopt a folder that is already there as a profile. Only
/// an absolute path or one containing a separator counts, so a bare name is
/// always left for the name/id lookup rather than resolving to a same-named
/// folder in the working directory.
fn existing_path(needle: &str) -> Option<PathBuf> {
    let p = Path::new(needle);
    let looks_pathy =
        p.is_absolute() || needle.contains(std::path::MAIN_SEPARATOR) || needle.contains('/');
    (looks_pathy && p.is_dir()).then(|| p.to_path_buf())
}

fn by_recency_owned() -> Vec<registry::ProfileRecord> {
    let records = profiles();
    registry::by_recency(&records)
        .into_iter()
        .cloned()
        .collect()
}

fn list_names(records: &[registry::ProfileRecord]) -> String {
    if records.is_empty() {
        return "  (none yet)".into();
    }
    records
        .iter()
        .map(|r| format!("  {}", r.name))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether this launch should ask which profile to open.
///
/// Never over an explicit `--profile` (that is resolved first, in
/// [`profile_folder`], so Shift cannot override an instruction on the command
/// line) and only when there is a choice to make — one profile has no question
/// in it.
pub fn wants_profile_picker(explicit: bool) -> bool {
    if explicit {
        return false;
    }
    if profiles().len() < 2 {
        return false;
    }
    cli::pick_profile_from_args() || shift_is_down()
}

/// Is Shift held right now?
///
/// Asked once, before the event loop exists — winit cannot answer until it is
/// running, so this is a direct platform query. `false` everywhere it cannot be
/// asked, which is the safe answer: the worst case is that the last profile
/// opens, exactly as it does today, and `--pick-profile` is always there.
#[cfg(windows)]
fn shift_is_down() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT};
    // SAFETY: a pure read of the keyboard state — no pointers, no allocation,
    // and safe to call before a window exists.
    (unsafe { GetAsyncKeyState(VK_SHIFT as i32) } as u16 & 0x8000) != 0
}

#[cfg(not(windows))]
fn shift_is_down() -> bool {
    false
}

/// Ask which profile to open.
///
/// **rfd rather than a window.** winit permits one `EventLoop` per process and
/// `App` cannot exist without a workspace open, so a real chooser window would
/// mean letting the app start with no document — a much larger change, and the
/// wrong one to bundle with this. Written up in `docs/deferred/`.
///
/// Two per dialog, most-recently-opened first, with the third button paging.
/// **Cancelling exits rather than guessing**, the same rule the folder question
/// follows.
fn pick_profile(records: &[registry::ProfileRecord]) -> Option<PathBuf> {
    let pages = pages(records);
    for (i, page) in pages.iter().enumerate() {
        let last = i + 1 == pages.len();
        let Some(first) = page[0].map(|j| &records[j]) else {
            continue;
        };
        let second = page[1].map(|j| &records[j]);
        let body = format!(
            "Which profile would you like to open?\n\n{}{}",
            describe_profile(first),
            second.map(describe_profile).unwrap_or_default()
        );
        let more = if last { "Cancel" } else { "More..." };
        let answer = rfd::MessageDialog::new()
            .set_title("Koru - which profile?")
            .set_description(&body)
            .set_buttons(rfd::MessageButtons::YesNoCancelCustom(
                first.name.clone(),
                second
                    .map(|r| r.name.clone())
                    .unwrap_or_else(|| "Cancel".into()),
                more.to_string(),
            ))
            .show();
        let rfd::MessageDialogResult::Custom(label) = answer else {
            return None;
        };
        if label == first.name {
            return Some(first.dir.clone());
        }
        if let Some(r) = second
            && label == r.name
        {
            return Some(r.dir.clone());
        }
        if last || label != more {
            return None;
        }
    }
    None
}

fn describe_profile(r: &registry::ProfileRecord) -> String {
    let missing = if r.dir.exists() {
        ""
    } else {
        "  (folder not found)"
    };
    format!(
        "{}{}\n{}\n{}\n\n",
        r.name,
        missing,
        r.email.as_deref().unwrap_or(""),
        r.dir.display()
    )
}

/// Two per dialog, in order.
///
/// Pure, and tested, because paging arithmetic is exactly where an off-by-one
/// hides a profile somebody then cannot reach at all.
fn pages(records: &[registry::ProfileRecord]) -> Vec<[Option<usize>; 2]> {
    (0..records.len())
        .step_by(2)
        .map(|i| [i, i + 1])
        .map(|[a, b]| [a, b].map(|k| (k < records.len()).then_some(k)))
        .filter_map(|p| p[0].map(|_| p))
        .collect()
}

/// `--profiles`: every profile, and whether its folder is still there.
pub fn report_profiles() {
    let records = profiles();
    if records.is_empty() {
        println!("No profiles yet - Koru makes one the first time it opens a folder.");
        if let Some(p) = registry_path() {
            println!("The list would be kept in: {}", p.display());
        }
        return;
    }
    for r in registry::by_recency(&records) {
        println!("{}", r.name);
        println!("  id:      {}", r.id.to_storage());
        if let Some(e) = &r.email {
            println!("  email:   {e}");
        }
        println!("  folder:  {}", r.dir.display());
        println!(
            "  status:  {}",
            if r.dir.exists() {
                "present"
            } else {
                "MISSING - the folder is not there right now"
            }
        );
        if let Some(t) = &r.last_opened {
            println!("  opened:  {t}");
        }
    }
    if let Some(p) = registry_path() {
        println!();
        println!("list: {}", p.display());
    }
}

/// Pick a folder that **already holds** a profile.
///
/// The counterpart to [`ask_for_a_new_profile_folder`], and the difference is
/// which way it refuses: that one confirms an empty folder and creates a profile
/// in it, this one refuses an empty folder by name.
///
/// It exists because the registry is only an index. Every folder describes
/// itself, so a profile dropped from the list is not lost — but until now the
/// only way back was a command line, which is not much of a promise.
pub fn ask_to_open_a_profile_folder() -> Option<PathBuf> {
    let picked = rfd::FileDialog::new()
        .set_title("Koru - open a profile folder")
        .pick_folder()?;
    match profilefolder::inspect(&picked) {
        FolderKind::Profile { .. } | FolderKind::Legacy => Some(picked),
        FolderKind::Empty => {
            rfd::MessageDialog::new()
                .set_title("Koru")
                .set_level(rfd::MessageLevel::Warning)
                .set_description(&format!(
                    "There is no Koru profile in:\n\n{}\n\nNothing was opened and \
                     nothing was created. If you meant to start a new profile \
                     here, use Add account instead.",
                    picked.display()
                ))
                .set_buttons(rfd::MessageButtons::Ok)
                .show();
            None
        }
    }
}
