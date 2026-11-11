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
            matches!(*op, "canonical" | "occupied" | "create_dir" | "is_dir"),
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
        check_selected(fs.as_ref(), selected().to_str().unwrap()),
        Ok(selected().to_str().unwrap().to_owned())
    );
    let missing = selected().join("missing");
    assert_eq!(
        check_selected(fs.as_ref(), missing.to_str().unwrap()),
        Err(FolderFailure::Invalid)
    );
    let file = selected().join("notes.txt");
    fs.file(&file);
    assert_eq!(
        check_selected(fs.as_ref(), file.to_str().unwrap()),
        Err(FolderFailure::Invalid)
    );
    assert_eq!(
        check_selected(fs.as_ref(), "relative/folder"),
        Err(FolderFailure::Invalid)
    );
    // A link is bound to where it resolves.
    let link = selected().join("shortcut");
    let target = selected().join("target");
    fs.mkdirs(&target);
    fs.link(&link, &target);
    assert_eq!(
        check_selected(fs.as_ref(), link.to_str().unwrap()),
        Ok(target.to_str().unwrap().to_owned())
    );
    assert!(fs.creates().is_empty());
    // "Change folder before the first task" uses the same check.
    assert!(require_existing_folder(fs.as_ref(), selected().to_str().unwrap()).is_ok());
    assert_eq!(
        require_existing_folder(fs.as_ref(), missing.to_str().unwrap())
            .unwrap_err()
            .code,
        "invalid_request"
    );
}

#[test]
fn a_stored_binding_never_carries_the_windows_verbatim_prefix() {
    assert_eq!(
        display(Path::new(r"\\?\C:\koru\ai\bots")).unwrap(),
        r"C:\koru\ai\bots"
    );
    assert_eq!(
        display(Path::new(r"\\?\UNC\server\share\work")).unwrap(),
        r"\\server\share\work"
    );
    assert_eq!(display(Path::new("/koru/ai")).unwrap(), "/koru/ai");
}
