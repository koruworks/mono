// SPDX-License-Identifier: MPL-2.0
use super::*;
use std::{cell::RefCell, collections::BTreeMap};

#[derive(Default)]
struct Fake {
    entries: RefCell<BTreeMap<PathBuf, bool>>,
    aliases: BTreeMap<PathBuf, PathBuf>,
    creates: RefCell<Vec<PathBuf>>,
    hide_occupied: bool,
}
impl Fake {
    fn add(&self, path: &Path, empty: bool) {
        self.entries.borrow_mut().insert(path.to_owned(), empty);
    }
}
impl Folders for Fake {
    fn canonical(&self, path: &Path) -> io::Result<PathBuf> {
        if let Some(target) = self.aliases.get(path) {
            return Ok(target.clone());
        }
        if self.entries.borrow().contains_key(path) {
            Ok(path.to_owned())
        } else {
            Err(io::ErrorKind::NotFound.into())
        }
    }
    fn occupied(&self, path: &Path) -> io::Result<bool> {
        Ok(!self.hide_occupied
            && (self.entries.borrow().contains_key(path) || self.aliases.contains_key(path)))
    }
    fn create(&self, path: &Path) -> io::Result<()> {
        self.creates.borrow_mut().push(path.to_owned());
        if self.entries.borrow().contains_key(path) || self.aliases.contains_key(path) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        self.add(path, true);
        Ok(())
    }
    fn directory(&self, path: &Path) -> io::Result<bool> {
        Ok(self.entries.borrow().contains_key(path))
    }
    fn empty(&self, path: &Path) -> io::Result<bool> {
        self.entries
            .borrow()
            .get(path)
            .copied()
            .ok_or(io::ErrorKind::NotFound.into())
    }
}
fn root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\synthetic")
    } else {
        PathBuf::from("/synthetic")
    }
}
fn fixture() -> (Fake, PathBuf, Uuid) {
    let fs = Fake::default();
    let root = root();
    fs.add(&root, true);
    (fs, root, Uuid::new_v4())
}

#[test]
fn managed_creates_only_three_levels_and_recovers_only_empty_creation() {
    let (fs, root, bot) = fixture();
    let target = managed_path(&root, bot);
    assert_eq!(
        managed(&fs, &root, bot, Purpose::Create),
        Ok(target.clone())
    );
    assert_eq!(fs.creates.borrow().len(), 3);
    assert_eq!(
        managed(&fs, &root, bot, Purpose::RecoverCreation),
        Ok(target.clone())
    );
    fs.add(&target, false);
    assert_eq!(
        managed(&fs, &root, bot, Purpose::RecoverCreation),
        Err(Failure::Mismatch)
    );
    assert_eq!(managed(&fs, &root, bot, Purpose::Run), Ok(target));
}

#[test]
fn initial_creation_never_adopts_existing_or_racing_folder() {
    let (mut fs, root, bot) = fixture();
    let target = managed_path(&root, bot);
    fs.add(&target, true);
    assert_eq!(
        managed(&fs, &root, bot, Purpose::Create),
        Err(Failure::Occupied)
    );
    assert!(fs.creates.borrow().is_empty());
    fs.hide_occupied = true;
    assert_eq!(
        managed(&fs, &root, bot, Purpose::Create),
        Err(Failure::Occupied)
    );
}

#[test]
fn parent_alias_refuses_before_any_child_creation() {
    for depth in 0..3 {
        let (mut fs, root, bot) = fixture();
        let parent = root.join("bots");
        let bot_folder = parent.join(bot.to_string());
        let target = bot_folder.join("workspace");
        let alias = [&parent, &bot_folder, &target][depth];
        let outside = root.with_file_name("other-data");
        fs.add(&outside, true);
        fs.aliases.insert(alias.clone(), outside);
        assert_eq!(
            managed(&fs, &root, bot, Purpose::Run),
            Err(Failure::Mismatch)
        );
        assert_eq!(fs.creates.borrow().last(), Some(alias));
        assert_eq!(fs.creates.borrow().len(), depth + 1);
    }
}

#[test]
fn selected_never_creates_and_checks_canonical_reserved_roots() {
    let (mut fs, root, _) = fixture();
    let selected_path = root.with_file_name("project");
    fs.add(&selected_path, false);
    assert_eq!(
        selected(&fs, &selected_path, &[root.clone()]),
        Ok(selected_path.clone())
    );
    let alias = root.with_file_name("shortcut");
    fs.aliases.insert(alias.clone(), root.clone());
    assert_eq!(
        selected(&fs, &alias, &[root.clone()]),
        Err(Failure::Reserved)
    );
    let sibling = root.with_file_name("synthetic-other");
    fs.add(&sibling, false);
    assert_eq!(selected(&fs, &sibling, &[root.clone()]), Ok(sibling));
    let missing_root = root.with_file_name("missing");
    assert_eq!(
        selected(&fs, &selected_path, &[missing_root]),
        Err(Failure::Unavailable)
    );
    assert_eq!(
        selected(&fs, Path::new("relative"), &[root]),
        Err(Failure::Invalid)
    );
    assert!(fs.creates.borrow().is_empty());
}

#[cfg(windows)]
#[test]
fn windows_display_binding_must_resolve_back_to_exact_directory() {
    let mut fs = Fake::default();
    let verbatim = PathBuf::from(r"\\?\C:\project");
    let shown = PathBuf::from(r"C:\project");
    fs.aliases.insert(shown.clone(), verbatim.clone());
    assert_eq!(binding(&fs, &verbatim), Ok(shown.clone()));
    fs.aliases.insert(shown, PathBuf::from(r"\\?\C:\different"));
    assert_eq!(binding(&fs, &verbatim), Err(Failure::Mismatch));
    let unc = PathBuf::from(r"\\?\UNC\server\share\project");
    let shown = PathBuf::from(r"\\server\share\project");
    fs.aliases.insert(shown.clone(), unc.clone());
    assert_eq!(binding(&fs, &unc), Ok(shown));
}
