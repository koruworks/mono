// SPDX-License-Identifier: MPL-2.0
//! The folder step over a fake filesystem: exactly the BotId's folder under
//! the canonical AI root, links that resolve elsewhere refused, and nothing
//! ever removed.
use super::*;

fn work(bot: BotId) -> PathBuf {
    root().join("bots").join(bot.to_storage()).join("work")
}

#[test]
fn the_managed_folder_is_made_level_by_level_under_the_canonical_root() {
    let fs = FakeFs::with(&[root()]);
    let bot = BotId::mint();
    let binding = prepare_managed(fs.as_ref(), &root(), bot, true).unwrap();
    assert_eq!(binding, work(bot).to_str().unwrap());
    assert!(fs.exists(&work(bot)));
    assert_eq!(
        fs.creates(),
        vec![
            root().join("bots"),
            root().join("bots").join(bot.to_storage()),
            work(bot)
        ]
    );
    // Nothing but creation and reads ever happens through the port.
    for (op, _) in &fs.state.lock().unwrap().calls {
        assert!(
            matches!(
                *op,
                "canonical" | "occupied" | "create_dir" | "is_dir" | "is_empty_dir"
            ),
            "{op}"
        );
    }
}

#[test]
fn anything_at_the_folder_on_first_acceptance_is_a_conflict_and_nothing_is_made() {
    for occupant in ["folder", "file"] {
        let fs = FakeFs::with(&[root()]);
        let bot = BotId::mint();
        if occupant == "folder" {
            fs.mkdirs(&work(bot));
        } else {
            fs.mkdirs(&root().join("bots").join(bot.to_storage()));
            fs.file(&work(bot));
        }
        let before = fs.present();
        assert_eq!(
            prepare_managed(fs.as_ref(), &root(), bot, true),
            Err(FolderFailure::Occupied),
            "{occupant}"
        );
        assert!(fs.creates().is_empty(), "{occupant}");
        assert_eq!(fs.present(), before);
    }
}

#[test]
fn a_recovery_adopts_exactly_the_folder_its_intent_names() {
    let fs = FakeFs::with(&[root()]);
    let bot = BotId::mint();
    fs.mkdirs(&work(bot));
    let before = fs.present();
    let binding = prepare_managed(fs.as_ref(), &root(), bot, false).unwrap();
    assert_eq!(binding, work(bot).to_str().unwrap());
    assert_eq!(fs.present(), before);
    // A file there is not a folder to adopt.
    let other = BotId::mint();
    fs.mkdirs(&root().join("bots").join(other.to_storage()));
    fs.file(&work(other));
    assert_eq!(
        prepare_managed(fs.as_ref(), &root(), other, false),
        Err(FolderFailure::Mismatch)
    );
}

#[test]
fn a_link_that_resolves_elsewhere_refuses_at_every_level() {
    let elsewhere = PathBuf::from(if cfg!(windows) {
        r"E:\elsewhere"
    } else {
        "/elsewhere"
    });
    let bot = BotId::mint();
    for at in [
        root().join("bots"),
        root().join("bots").join(bot.to_storage()),
        work(bot),
    ] {
        let fs = FakeFs::with(&[root(), elsewhere.clone()]);
        if at != root().join("bots") {
            fs.mkdirs(at.parent().unwrap());
        }
        fs.link(&at, &elsewhere);
        let before = fs.present();
        assert_eq!(
            prepare_managed(fs.as_ref(), &root(), bot, false),
            Err(FolderFailure::Mismatch),
            "{at:?}"
        );
        // Nothing was made through the link.
        assert_eq!(fs.present(), before, "{at:?}");
    }
}

#[test]
fn an_unreachable_root_or_a_failed_mkdir_is_unavailable() {
    let fs = FakeFs::with(&[]);
    assert_eq!(
        prepare_managed(fs.as_ref(), &root(), BotId::mint(), true),
        Err(FolderFailure::Unavailable)
    );
    let fs = FakeFs::with(&[root()]);
    fs.fail_create("work", Fail::Before);
    let bot = BotId::mint();
    assert_eq!(
        prepare_managed(fs.as_ref(), &root(), bot, true),
        Err(FolderFailure::Unavailable)
    );
    assert!(!fs.exists(&work(bot)));
}

#[test]
fn a_selected_folder_is_validated_and_never_made() {
    let fs = FakeFs::with(&[selected()]);
    assert_eq!(
        check_selected(fs.as_ref(), &reserved(), selected().to_str().unwrap()),
        Ok(selected().to_str().unwrap().to_owned())
    );
    let missing = selected().join("missing");
    assert_eq!(
        check_selected(fs.as_ref(), &reserved(), missing.to_str().unwrap()),
        Err(FolderFailure::Invalid)
    );
    let file = selected().join("notes.txt");
    fs.file(&file);
    assert_eq!(
        check_selected(fs.as_ref(), &reserved(), file.to_str().unwrap()),
        Err(FolderFailure::Invalid)
    );
    assert_eq!(
        check_selected(fs.as_ref(), &reserved(), "relative/folder"),
        Err(FolderFailure::Invalid)
    );
    // A link is bound to where it resolves.
    let link = selected().join("shortcut");
    let target = selected().join("target");
    fs.mkdirs(&target);
    fs.link(&link, &target);
    assert_eq!(
        check_selected(fs.as_ref(), &reserved(), link.to_str().unwrap()),
        Ok(target.to_str().unwrap().to_owned())
    );
    assert!(fs.creates().is_empty());
    // "Change folder before the first task" uses the same check.
    assert!(
        require_existing_folder(fs.as_ref(), &reserved(), selected().to_str().unwrap()).is_ok()
    );
    assert_eq!(
        require_existing_folder(fs.as_ref(), &reserved(), missing.to_str().unwrap())
            .unwrap_err()
            .code,
        "invalid_request"
    );
}

/// A filesystem that answers `canonical` from a fixed table, for the
/// verbatim-prefix round trip.
struct Canonical(Vec<(PathBuf, PathBuf)>);
impl BotFolderSystem for Canonical {
    fn canonical(&self, at: &Path) -> std::io::Result<PathBuf> {
        self.0
            .iter()
            .find(|(from, _)| from == at)
            .map(|(_, to)| to.clone())
            .ok_or_else(missing)
    }
    fn occupied(&self, _: &Path) -> std::io::Result<bool> {
        unreachable!()
    }
    fn create_dir(&self, _: &Path) -> std::io::Result<()> {
        unreachable!()
    }
    fn is_dir(&self, _: &Path) -> std::io::Result<bool> {
        unreachable!()
    }
    fn is_empty_dir(&self, _: &Path) -> std::io::Result<bool> {
        unreachable!()
    }
}

#[test]
fn the_verbatim_prefix_is_dropped_only_when_the_path_keeps_its_meaning() {
    let verbatim = PathBuf::from(r"\\?\C:\koru\ai\bots");
    let unc = PathBuf::from(r"\\?\UNC\server\share\work");
    let fs = Canonical(vec![
        (PathBuf::from(r"C:\koru\ai\bots"), verbatim.clone()),
        (PathBuf::from(r"\\server\share\work"), unc.clone()),
    ]);
    // Only Windows reads these as absolute folders.
    if cfg!(windows) {
        assert_eq!(display(&fs, &verbatim).unwrap(), r"C:\koru\ai\bots");
        assert_eq!(display(&fs, &unc).unwrap(), r"\\server\share\work");
    }
    // A path without the prefix is stored as it is.
    assert_eq!(display(&fs, &root()).unwrap(), root().to_str().unwrap());
    // A trailing dot is trimmed by ordinary path parsing: without the
    // prefix the path names another folder, so it is refused, not stored.
    let dotted = PathBuf::from(r"\\?\C:\work\notes.");
    let fs = Canonical(vec![(
        PathBuf::from(r"C:\work\notes."),
        PathBuf::from(r"\\?\C:\work\notes"),
    )]);
    assert_eq!(display(&fs, &dotted), Err(FolderFailure::Invalid));
    // A reserved device name means the device without the prefix.
    let device = PathBuf::from(r"\\?\C:\work\con");
    let fs = Canonical(vec![(
        PathBuf::from(r"C:\work\con"),
        PathBuf::from(r"\\.\con"),
    )]);
    assert_eq!(display(&fs, &device), Err(FolderFailure::Invalid));
    // An over-long path that does not resolve without the prefix.
    let long = PathBuf::from(format!(r"\\?\C:\{}", "a".repeat(300)));
    assert_eq!(
        display(&Canonical(vec![]), &long),
        Err(FolderFailure::Invalid)
    );
}

#[test]
fn a_later_folder_job_adopts_only_an_empty_folder_exactly_there() {
    // A Retry or a restart after `folder_pending`: the folder may be ours.
    let fs = FakeFs::with(&[root()]);
    let bot = BotId::mint();
    fs.mkdirs(&work(bot));
    assert_eq!(
        prepare_managed(fs.as_ref(), &root(), bot, false).unwrap(),
        work(bot).to_str().unwrap()
    );
    // Something put in it since: this creation cannot prove it made it.
    for occupant in ["file", "folder"] {
        let fs = FakeFs::with(&[root()]);
        let bot = BotId::mint();
        fs.mkdirs(&work(bot));
        if occupant == "file" {
            fs.file(&work(bot).join("notes.txt"));
        } else {
            fs.mkdirs(&work(bot).join("sub"));
        }
        let before = fs.present();
        assert_eq!(
            prepare_managed(fs.as_ref(), &root(), bot, false),
            Err(FolderFailure::Mismatch),
            "{occupant}"
        );
        assert_eq!(fs.present(), before, "nothing is removed");
    }
    // A folder this job makes itself is bound whatever happens next.
    let fs = FakeFs::with(&[root()]);
    let bot = BotId::mint();
    assert!(prepare_managed(fs.as_ref(), &root(), bot, false).is_ok());
    assert!(fs.exists(&work(bot)));
}

#[test]
fn koru_s_own_data_is_never_a_selected_folder() {
    let store = PathBuf::from(if cfg!(windows) {
        r"D:\koru\store"
    } else {
        "/koru/store"
    });
    let other = root()
        .join("bots")
        .join(BotId::mint().to_storage())
        .join("work");
    let fs = FakeFs::with(&[root(), store.clone(), other.clone(), selected()]);
    let reserved = ReservedFolders {
        ai_root: root(),
        store_root: Some(store.clone()),
    };
    for at in [root(), store.clone(), root().join("bots"), other.clone()] {
        assert_eq!(
            check_selected(fs.as_ref(), &reserved, at.to_str().unwrap()),
            Err(FolderFailure::Reserved),
            "{at:?}"
        );
    }
    // Through a link it is the same folder.
    let shortcut = selected().join("shortcut");
    fs.link(&shortcut, &other);
    assert_eq!(
        check_selected(fs.as_ref(), &reserved, shortcut.to_str().unwrap()),
        Err(FolderFailure::Reserved)
    );
    // Anything else is the person's to choose.
    assert_eq!(
        check_selected(fs.as_ref(), &reserved, selected().to_str().unwrap()),
        Ok(selected().to_str().unwrap().to_owned())
    );
    assert_eq!(
        require_existing_folder(fs.as_ref(), &reserved, root().to_str().unwrap())
            .unwrap_err()
            .code,
        "invalid_request"
    );
}
