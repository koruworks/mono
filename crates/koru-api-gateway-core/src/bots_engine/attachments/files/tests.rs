// SPDX-License-Identifier: MPL-2.0
use super::*;
use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
#[derive(Default)]
pub(in crate::bots_engine) struct Memory {
    modified: Mutex<BTreeMap<PathBuf, i64>>,
    entries: Mutex<BTreeMap<PathBuf, Option<Vec<u8>>>>,
    aliases: Mutex<BTreeMap<PathBuf, PathBuf>>,
    fail_write: AtomicBool,
}
impl Memory {
    pub(in crate::bots_engine) fn new(root: &Path) -> Self {
        Self {
            entries: Mutex::new(BTreeMap::from([(root.to_path_buf(), None)])),
            ..Self::default()
        }
    }
}
impl Files for Memory {
    fn entries(
        &self,
        path: &Path,
    ) -> io::Result<Box<dyn Iterator<Item = io::Result<PathBuf>> + Send>> {
        let entries = self.entries.lock().unwrap();
        if entries.get(path) != Some(&None) {
            return Err(io::ErrorKind::NotFound.into());
        }
        let children: Vec<_> = entries
            .keys()
            .filter(|p| p.parent() == Some(path))
            .cloned()
            .map(Ok)
            .collect();
        Ok(Box::new(children.into_iter()))
    }
    fn modified_ms(&self, path: &Path) -> io::Result<i64> {
        self.canonical(path)?;
        Ok(*self.modified.lock().unwrap().get(path).unwrap_or(&0))
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        let mut entries = self.entries.lock().unwrap();
        if !matches!(entries.get(path), Some(Some(_))) {
            return Err(io::ErrorKind::NotFound.into());
        }
        entries.remove(path);
        Ok(())
    }
    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        let mut entries = self.entries.lock().unwrap();
        if entries.get(path) != Some(&None) || entries.keys().any(|p| p.parent() == Some(path)) {
            return Err(io::ErrorKind::Other.into());
        }
        entries.remove(path);
        Ok(())
    }
    fn canonical(&self, path: &Path) -> io::Result<PathBuf> {
        if let Some(other) = self.aliases.lock().unwrap().get(path) {
            return Ok(other.clone());
        }
        if self.entries.lock().unwrap().contains_key(path) {
            Ok(path.to_path_buf())
        } else {
            Err(io::ErrorKind::NotFound.into())
        }
    }
    fn directory(&self, path: &Path) -> io::Result<()> {
        let mut entries = self.entries.lock().unwrap();
        if entries.contains_key(path) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        assert_eq!(entries.get(path.parent().unwrap()), Some(&None));
        entries.insert(path.to_owned(), None);
        Ok(())
    }
    fn create(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        if self.fail_write.load(Ordering::Relaxed) {
            return Err(io::ErrorKind::Other.into());
        }
        let mut entries = self.entries.lock().unwrap();
        if entries.contains_key(path) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        assert_eq!(entries.get(path.parent().unwrap()), Some(&None));
        entries.insert(path.to_owned(), Some(bytes.to_vec()));
        Ok(())
    }
    fn read(&self, path: &Path, limit: usize) -> io::Result<Vec<u8>> {
        match self.entries.lock().unwrap().get(path) {
            Some(Some(bytes)) if bytes.len() <= limit => Ok(bytes.clone()),
            _ => Err(io::ErrorKind::NotFound.into()),
        }
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut entries = self.entries.lock().unwrap();
        if entries.contains_key(to) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        let moved: Vec<_> = entries
            .iter()
            .filter_map(|(p, v)| {
                p.strip_prefix(from)
                    .ok()
                    .map(|suffix| (p.clone(), to.join(suffix), v.clone()))
            })
            .collect();
        assert!(!moved.is_empty());
        for (old, new, value) in moved {
            entries.remove(&old);
            entries.insert(new, value);
        }
        Ok(())
    }
}
fn root() -> PathBuf {
    PathBuf::from(if cfg!(windows) {
        r"C:\synthetic"
    } else {
        "/synthetic"
    })
}
fn upload() -> CheckedAttachment {
    koru_ai_server::check_attachment(
        koru_ai_core::ChatId::new(),
        koru_ai_core::ChatAttachmentId::new(),
        "notes.txt",
        AttachmentSource::Drop,
        b"hello",
    )
    .unwrap()
}
#[test]
fn immutable_publication_replays_only_identical_content_and_detects_corruption() {
    let root = root();
    let fs = Memory::new(&root);
    let mut checked = upload();
    let first = publish(&fs, &root, &checked, b"hello").unwrap();
    checked.record.attachment.created_at = koru_ai_core::Timestamp::from_millis(1);
    assert_eq!(publish(&fs, &root, &checked, b"hello").unwrap(), first);
    assert_eq!(read(&fs, &root, &first, false).unwrap(), b"hello");
    checked.record.content_blake3 = blake3::hash(b"other").to_hex().to_string();
    assert_eq!(
        publish(&fs, &root, &checked, b"other").unwrap_err(),
        Error::Conflict
    );
    let path = root
        .join("bots/attachments")
        .join(first.thread.to_string())
        .join(first.attachment.id.to_storage())
        .join("file/notes.txt");
    fs.entries
        .lock()
        .unwrap()
        .insert(path, Some(b"other".to_vec()));
    assert_eq!(
        read(&fs, &root, &first, false).unwrap_err(),
        Error::StorageUnavailable
    );
}
#[test]
fn interrupted_write_never_publishes_and_retry_uses_a_fresh_staging_directory() {
    let root = root();
    let fs = Memory::new(&root);
    let checked = upload();
    fs.fail_write.store(true, Ordering::Relaxed);
    assert_eq!(
        publish(&fs, &root, &checked, b"hello").unwrap_err(),
        Error::StorageUnavailable
    );
    let target = root
        .join("bots/attachments")
        .join(checked.record.chat_id.to_storage())
        .join(checked.record.attachment.id.to_storage());
    assert!(fs.canonical(&target).is_err());
    fs.fail_write.store(false, Ordering::Relaxed);
    let saved = publish(&fs, &root, &checked, b"hello").unwrap();
    assert_eq!(read(&fs, &root, &saved, false).unwrap(), b"hello");
}
#[test]
fn junctions_in_parents_or_originals_refuse_without_writing_outside_root() {
    let root = root();
    let fs = Memory::new(&root);
    let checked = upload();
    fs.aliases
        .lock()
        .unwrap()
        .insert(root.join("bots"), root.join("elsewhere"));
    assert_eq!(
        publish(&fs, &root, &checked, b"hello").unwrap_err(),
        Error::NotAllowed
    );
    fs.aliases.lock().unwrap().clear();
    let saved = publish(&fs, &root, &checked, b"hello").unwrap();
    let file = root
        .join("bots/attachments")
        .join(saved.thread.to_string())
        .join(saved.attachment.id.to_storage())
        .join("file/notes.txt");
    fs.aliases
        .lock()
        .unwrap()
        .insert(file, root.join("elsewhere"));
    assert_eq!(
        read(&fs, &root, &saved, false).unwrap_err(),
        Error::NotAllowed
    );
    assert!(
        !fs.entries
            .lock()
            .unwrap()
            .contains_key(&root.join("elsewhere"))
    );
}

#[test]
fn provider_paths_require_verified_originals_and_keep_path_references_metadata_only() {
    let root = root();
    let fs = Memory::new(&root);
    let checked = upload();
    let saved = publish(&fs, &root, &checked, b"hello").unwrap();
    let input = input_file(&fs, &root, &saved).unwrap();
    assert!(input.path.ends_with("notes.txt"));
    assert!(input.image.is_none());
    let mut reference = saved.clone();
    reference.attachment.source = AttachmentSource::Path;
    reference.attachment.kind = koru_ai_core::AttachmentKind::Folder;
    reference.attachment.reference_path = Some(root.join("not-present").to_str().unwrap().into());
    assert_eq!(
        input_file(&fs, &root, &reference).unwrap().path,
        reference.attachment.reference_path.unwrap()
    );
    fs.entries
        .lock()
        .unwrap()
        .remove(&PathBuf::from(input.path));
    assert!(input_file(&fs, &root, &saved).is_err());
}

#[test]
fn sweep_keeps_protected_and_new_entries_and_removes_only_owned_old_leaves() {
    let root = root();
    let fs = Memory::new(&root);
    let old = publish(&fs, &root, &upload(), b"hello").unwrap();
    let kept = publish(&fs, &root, &upload(), b"hello").unwrap();
    let fresh = publish(&fs, &root, &upload(), b"hello").unwrap();
    let target = |record: &Record| {
        root.join("bots/attachments")
            .join(record.thread.to_string())
            .join(record.attachment.id.to_storage())
    };
    fs.modified.lock().unwrap().insert(target(&fresh), 100);
    let staging = target(&old)
        .parent()
        .unwrap()
        .join(format!(".staging-{}", Uuid::new_v4()));
    fs.directory(&staging).unwrap();
    fs.directory(&staging.join("file")).unwrap();
    let mut checked = vec![];
    sweep(
        &fs,
        &root,
        50,
        |thread, id| {
            checked.push((thread, id));
            Ok(id != Some(kept.attachment.id.as_uuid()))
        },
        || false,
    )
    .unwrap();
    assert!(fs.canonical(&target(&old)).is_err());
    assert!(fs.canonical(&staging).is_err());
    assert!(fs.canonical(&target(&kept)).is_ok());
    assert!(fs.canonical(&target(&fresh)).is_ok());
    assert!(
        !checked
            .iter()
            .any(|(_, id)| *id == Some(fresh.attachment.id.as_uuid()))
    );
}

#[test]
fn deletion_checks_the_complete_shape_and_never_follows_a_redirect() {
    let root = root();
    let fs = Memory::new(&root);
    let saved = publish(&fs, &root, &upload(), b"hello").unwrap();
    let target = root
        .join("bots/attachments")
        .join(saved.thread.to_string())
        .join(saved.attachment.id.to_storage());
    let foreign = target.join("unrecognized.txt");
    fs.create(&foreign, b"retain").unwrap();
    assert_eq!(
        discard(&fs, &root, saved.thread, saved.attachment.id.as_uuid()).unwrap_err(),
        Error::NotAllowed
    );
    assert_eq!(read(&fs, &root, &saved, false).unwrap(), b"hello");
    fs.remove_file(&foreign).unwrap();
    fs.aliases
        .lock()
        .unwrap()
        .insert(target.clone(), root.join("elsewhere"));
    assert_eq!(
        discard(&fs, &root, saved.thread, saved.attachment.id.as_uuid()).unwrap_err(),
        Error::NotAllowed
    );
    fs.aliases.lock().unwrap().clear();
    discard(&fs, &root, saved.thread, saved.attachment.id.as_uuid()).unwrap();
    discard(&fs, &root, saved.thread, saved.attachment.id.as_uuid()).unwrap();
    assert!(fs.canonical(&target).is_err());
}
