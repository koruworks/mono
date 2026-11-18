// SPDX-License-Identifier: MPL-2.0
//! Immutable engine attachment bytes. Only this gateway adapter touches disk;
//! the engine actor stores metadata. Publication precedes metadata acceptance.
//! Interrupted staging is retained rather than risking deletion of live data.
use koru_ai_core::{AttachmentSource, attachments::MAX_ATTACHMENT_BYTES};
use koru_ai_server::CheckedAttachment;
use koru_bots_engine::{Error, attachments::Record};
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub(in crate::bots_engine) trait Files: Send + Sync {
    fn canonical(&self, path: &Path) -> io::Result<PathBuf>;
    fn directory(&self, path: &Path) -> io::Result<()>;
    fn create(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    fn read(&self, path: &Path, limit: usize) -> io::Result<Vec<u8>>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
}
pub(in crate::bots_engine) struct Native;
impl Files for Native {
    fn canonical(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }
    fn directory(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir(path)
    }
    fn create(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_all()
    }
    fn read(&self, path: &Path, limit: usize) -> io::Result<Vec<u8>> {
        let file = std::fs::File::open(path)?;
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(bytes)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }
}
fn storage(_: io::Error) -> Error {
    Error::StorageUnavailable
}
fn exact(fs: &dyn Files, path: &Path) -> Result<(), Error> {
    if fs.canonical(path).map_err(storage)? != path {
        return Err(Error::NotAllowed);
    }
    Ok(())
}
fn level(fs: &dyn Files, path: &Path, create: bool) -> Result<(), Error> {
    if create {
        match fs.directory(path) {
            Ok(()) => (),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(storage(e)),
        }
    }
    exact(fs, path)
}
fn thread_dir(fs: &dyn Files, root: &Path, thread: Uuid, create: bool) -> Result<PathBuf, Error> {
    if thread.is_nil() {
        return Err(Error::InvalidRequest);
    }
    let mut path = fs.canonical(root).map_err(storage)?;
    for part in [
        "bots".to_owned(),
        "attachments".to_owned(),
        thread.to_string(),
    ] {
        path.push(part);
        level(fs, &path, create)?;
    }
    Ok(path)
}
fn checked_read(fs: &dyn Files, path: &Path, limit: usize) -> Result<Vec<u8>, Error> {
    exact(fs, path)?;
    fs.read(path, limit).map_err(storage)
}
fn record(checked: &CheckedAttachment) -> Record {
    Record {
        thread: checked.record.chat_id.as_uuid(),
        attachment: checked.record.attachment.clone(),
        content_blake3: checked.record.content_blake3.clone(),
        has_sent_copy: checked.record.has_sent_copy,
    }
}

/// Never overwrites an existing attachment. A retry adopts only identical
/// bytes and metadata; neither timestamp nor later message binding is identity.
pub(super) fn publish(
    fs: &dyn Files,
    root: &Path,
    checked: &CheckedAttachment,
    original: &[u8],
) -> Result<Record, Error> {
    let record = record(checked);
    if record.attachment.id.as_uuid().is_nil()
        || record.attachment.source == AttachmentSource::Path
        || original.len() as u64 != record.attachment.byte_count
        || blake3::hash(original).to_hex().as_str() != record.content_blake3
    {
        return Err(Error::InvalidRequest);
    }
    let parent = thread_dir(fs, root, record.thread, true)?;
    let target = parent.join(record.attachment.id.to_storage());
    if !target
        .join("file")
        .join(&record.attachment.file_name)
        .to_str()
        .is_some_and(koru_bots_engine::workspace::input::valid_path)
    {
        return Err(Error::InvalidRequest);
    }
    let verify = || -> Result<Record, Error> {
        exact(fs, &target)?;
        let bytes = checked_read(fs, &target.join("record.json"), 16 * 1024)?;
        let saved: Record = serde_json::from_slice(&bytes).map_err(|_| Error::Conflict)?;
        let mut same = record.clone();
        same.attachment.created_at = saved.attachment.created_at;
        exact(fs, &target.join("file"))?;
        if saved != same
            || checked_read(
                fs,
                &target.join("file").join(&record.attachment.file_name),
                MAX_ATTACHMENT_BYTES as usize,
            )? != original
        {
            return Err(Error::Conflict);
        }
        if let Some(thumbnail) = &checked.thumbnail {
            if checked_read(fs, &target.join("thumb.png"), MAX_ATTACHMENT_BYTES as usize)?
                != *thumbnail
            {
                return Err(Error::Conflict);
            }
        }
        if let Some((name, bytes)) = &checked.sent_copy {
            if checked_read(fs, &target.join(name), MAX_ATTACHMENT_BYTES as usize)? != *bytes {
                return Err(Error::Conflict);
            }
        }
        Ok(saved)
    };
    match fs.canonical(&target) {
        Ok(_) => return verify(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
        Err(e) => return Err(storage(e)),
    }
    let staging = parent.join(format!(".staging-{}", Uuid::new_v4()));
    // A fresh directory only: never adopt an attacker-supplied staging name.
    fs.directory(&staging).map_err(storage)?;
    exact(fs, &staging)?;
    level(fs, &staging.join("file"), true)?;
    fs.create(
        &staging.join("file").join(&record.attachment.file_name),
        original,
    )
    .map_err(storage)?;
    if let Some(bytes) = &checked.thumbnail {
        fs.create(&staging.join("thumb.png"), bytes)
            .map_err(storage)?;
    }
    if let Some((name, bytes)) = &checked.sent_copy {
        fs.create(&staging.join(name), bytes).map_err(storage)?;
    }
    fs.create(
        &staging.join("record.json"),
        &serde_json::to_vec(&record).map_err(|_| Error::InvalidRequest)?,
    )
    .map_err(storage)?;
    exact(fs, &parent)?;
    // Every published directory is nonempty. rename cannot replace one on
    // either Windows or Unix; a competing identical publication is a replay.
    match fs.rename(&staging, &target) {
        Ok(()) => Ok(record),
        Err(_) => verify(),
    }
}

pub(super) fn read(
    fs: &dyn Files,
    root: &Path,
    record: &Record,
    thumbnail: bool,
) -> Result<Vec<u8>, Error> {
    if record.attachment.source == AttachmentSource::Path {
        return Err(Error::NotAllowed);
    }
    let parent = thread_dir(fs, root, record.thread, false)?;
    let target = parent.join(record.attachment.id.to_storage());
    exact(fs, &target)?;
    let path = if thumbnail {
        target.join("thumb.png")
    } else {
        exact(fs, &target.join("file"))?;
        target.join("file").join(&record.attachment.file_name)
    };
    let bytes = checked_read(fs, &path, MAX_ATTACHMENT_BYTES as usize)?;
    if !thumbnail
        && (bytes.len() as u64 != record.attachment.byte_count
            || blake3::hash(&bytes).to_hex().as_str() != record.content_blake3)
    {
        return Err(Error::StorageUnavailable);
    }
    Ok(bytes)
}

pub(super) fn input_file(
    fs: &dyn Files,
    root: &Path,
    record: &Record,
) -> Result<koru_bots_engine::workspace::input::File, Error> {
    use koru_bots_engine::workspace::input::{File, Image, valid_path};
    let a = &record.attachment;
    if a.source == AttachmentSource::Path {
        let path = a
            .reference_path
            .clone()
            .filter(|p| valid_path(p))
            .ok_or(Error::InvalidRequest)?;
        return Ok(File {
            id: a.id.as_uuid(),
            kind: a.kind,
            path,
            image: None,
        });
    }
    // Verify the original against engine metadata before exposing any path.
    read(fs, root, record, false)?;
    let target = thread_dir(fs, root, record.thread, false)?.join(a.id.to_storage());
    let original = target.join("file").join(&a.file_name);
    let path = original
        .to_str()
        .filter(|p| valid_path(p))
        .ok_or(Error::InvalidRequest)?
        .to_owned();
    let image = if a.kind == koru_ai_core::AttachmentKind::Image {
        if record.has_sent_copy {
            let mut found = None;
            for (name, media_type) in [("sent.png", "image/png"), ("sent.jpg", "image/jpeg")] {
                let candidate = target.join(name);
                match fs.canonical(&candidate) {
                    Ok(_) => {
                        if found.is_some() {
                            return Err(Error::StorageUnavailable);
                        }
                        checked_read(
                            fs,
                            &candidate,
                            koru_ai_core::attachments::MAX_SENT_COPY_BYTES as usize,
                        )?;
                        let path = candidate
                            .to_str()
                            .filter(|p| valid_path(p))
                            .ok_or(Error::InvalidRequest)?
                            .to_owned();
                        found = Some(Image {
                            path,
                            media_type: media_type.into(),
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                    Err(e) => return Err(storage(e)),
                }
            }
            Some(found.ok_or(Error::StorageUnavailable)?)
        } else {
            Some(Image {
                path: path.clone(),
                media_type: a.media_type.clone(),
            })
        }
    } else {
        None
    };
    Ok(File {
        id: a.id.as_uuid(),
        kind: a.kind,
        path,
        image,
    })
}

#[cfg(test)]
pub(in crate::bots_engine) mod tests;
