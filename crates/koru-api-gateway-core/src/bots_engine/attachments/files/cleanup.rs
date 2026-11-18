// SPDX-License-Identifier: MPL-2.0
//! Delete only an engine-authorized leaf. No recursive deletion or following
//! junctions, and no enumeration on a timer after the startup scan completes.
use super::*;

fn optional(fs: &dyn Files, path: &Path) -> Result<bool, Error> {
    match fs.canonical(path) {
        Ok(actual) if actual == path => Ok(true),
        Ok(_) => Err(Error::NotAllowed),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(storage(e)),
    }
}
fn base(fs: &dyn Files, root: &Path) -> Result<Option<PathBuf>, Error> {
    let mut path = fs.canonical(root).map_err(storage)?;
    for part in ["bots", "attachments"] {
        path.push(part);
        if !optional(fs, &path)? {
            return Ok(None);
        }
    }
    Ok(Some(path))
}
fn uuid(name: &str) -> Option<Uuid> {
    Uuid::parse_str(name)
        .ok()
        .filter(|id| !id.is_nil() && id.to_string() == name)
}
fn erase(fs: &dyn Files, target: &Path) -> Result<(), Error> {
    if !optional(fs, target)? {
        return Ok(());
    }
    // Check the complete tiny shape before removing anything. Foreign names
    // are left in place for inspection, even inside an otherwise dead draft.
    let entries = fs
        .entries(target)
        .map_err(storage)?
        .take(7)
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    if entries.len() > 5 {
        return Err(Error::NotAllowed);
    }
    let mut files = vec![];
    let mut originals = None;
    for path in entries {
        if path.parent() != Some(target) {
            return Err(Error::NotAllowed);
        }
        exact(fs, &path)?;
        match path.file_name().and_then(|n| n.to_str()) {
            Some("file") => {
                let children = fs
                    .entries(&path)
                    .map_err(storage)?
                    .take(2)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(storage)?;
                if children.len() > 1 {
                    return Err(Error::NotAllowed);
                }
                for child in children {
                    let name = child
                        .file_name()
                        .and_then(|n| n.to_str())
                        .ok_or(Error::NotAllowed)?;
                    if child.parent() != Some(path.as_path())
                        || name != koru_ai_core::attachments::sanitize_file_name(name)
                    {
                        return Err(Error::NotAllowed);
                    }
                    exact(fs, &child)?;
                    files.push(child);
                }
                originals = Some(path);
            }
            Some("record.json" | "thumb.png" | "sent.png" | "sent.jpg") => files.push(path),
            _ => return Err(Error::NotAllowed),
        }
    }
    for path in files {
        exact(fs, &path)?;
        fs.remove_file(&path).map_err(storage)?;
    }
    if let Some(path) = originals {
        exact(fs, &path)?;
        fs.remove_dir(&path).map_err(storage)?;
    }
    exact(fs, target)?;
    fs.remove_dir(target).map_err(storage)
}
pub(in crate::bots_engine::attachments) fn discard(
    fs: &dyn Files,
    root: &Path,
    thread: Uuid,
    id: Uuid,
) -> Result<(), Error> {
    if thread.is_nil() || id.is_nil() {
        return Err(Error::InvalidRequest);
    }
    let Some(base) = base(fs, root)? else {
        return Ok(());
    };
    let parent = base.join(thread.to_string());
    if !optional(fs, &parent)? {
        return Ok(());
    }
    erase(fs, &parent.join(id.to_string()))
}

pub(in crate::bots_engine::attachments) fn sweep(
    fs: &dyn Files,
    root: &Path,
    started_ms: i64,
    mut collect: impl FnMut(Uuid, Option<Uuid>) -> Result<bool, Error>,
    stopping: impl Fn() -> bool,
) -> Result<(), Error> {
    let Some(base) = base(fs, root)? else {
        return Ok(());
    };
    let mut failed = false;
    for thread_path in fs.entries(&base).map_err(storage)? {
        if stopping() {
            return Ok(());
        }
        let Ok(thread_path) = thread_path else {
            failed = true;
            continue;
        };
        let Some(thread) = thread_path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(uuid)
        else {
            continue;
        };
        if thread_path.parent() != Some(base.as_path()) || exact(fs, &thread_path).is_err() {
            failed = true;
            continue;
        }
        let entries = match fs.entries(&thread_path) {
            Ok(entries) => entries,
            Err(_) => {
                failed = true;
                continue;
            }
        };
        for path in entries {
            if stopping() {
                return Ok(());
            }
            let Ok(path) = path else {
                failed = true;
                continue;
            };
            let result = (|| -> Result<(), Error> {
                if path.parent() != Some(thread_path.as_path()) {
                    return Err(Error::NotAllowed);
                }
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    return Ok(());
                };
                let id = if let Some(id) = uuid(name) {
                    Some(id)
                } else if name.strip_prefix(".staging-").and_then(uuid).is_some() {
                    None
                } else {
                    return Ok(());
                };
                exact(fs, &path)?;
                if fs.modified_ms(&path).map_err(storage)? >= started_ms {
                    return Ok(());
                }
                if collect(thread, id)? {
                    erase(fs, &path)?;
                }
                Ok(())
            })();
            match result {
                Err(Error::EngineUnavailable) => return Err(Error::EngineUnavailable),
                Err(Error::NotAllowed) => (), // Another owner or an unsafe path is retained.
                Err(_) => failed = true,
                Ok(()) => (),
            }
        }
    }
    if failed {
        Err(Error::StorageUnavailable)
    } else {
        Ok(())
    }
}
