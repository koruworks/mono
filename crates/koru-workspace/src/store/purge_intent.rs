// SPDX-License-Identifier: MPL-2.0

//! Machine-local final-purge intent, beside the Store outbox. A synced purge
//! event alone is never authorization to discard this device's changed copy.
//! Each immutable format-1 file records one locally requested batch. It is
//! flushed before publishing its events, and retained after manifest removal
//! so a crash before the manifest acknowledgment can resume the same events.
//! All callers run these filesystem operations on workers.

use koru_core::pages::purge::Event;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::Path,
};

mod source;
pub(crate) use source::prepare;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    format: u8,
    workspace: String,
    events: Vec<Event>,
}

pub(crate) fn write(dir: &Path, workspace: &str, events: &[Event]) -> Result<(), String> {
    let first = events.first().ok_or("empty purge intent")?;
    let path = dir.join(format!("{}.json", first.event_id));
    let intent = Intent {
        format: 1,
        workspace: workspace.to_owned(),
        events: events.to_vec(),
    };
    validate(&intent, workspace)?;
    let bytes = serde_json::to_vec(&intent).map_err(|e| e.to_string())?;
    if path.exists() {
        let existing = read_one(&path, workspace)?;
        return if existing == events {
            Ok(())
        } else {
            Err("purge intent collision".into())
        };
    }
    write_once(&path, &bytes)
}

pub(crate) fn write_once(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or("journal path has no parent")?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    if path.exists() {
        return if fs::read(path).map_err(|e| e.to_string())? == bytes {
            Ok(())
        } else {
            Err("immutable journal collision".into())
        };
    }
    let part = path.with_extension(format!("{}.part", std::process::id()));
    let mut file = fs::File::create(&part).map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())?;
    drop(file);
    rename(&part, &path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    for parent in dir.ancestors().take(4) {
        fs::File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(crate) fn read(dir: &Path, workspace: &str) -> Result<Vec<Vec<Event>>, String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().and_then(|x| x.to_str()) == Some("json") {
            out.push(read_one(&path, workspace)?);
        }
    }
    Ok(out)
}

fn read_one(path: &Path, workspace: &str) -> Result<Vec<Event>, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let intent: Intent = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    validate(&intent, workspace)?;
    Ok(intent.events)
}

fn validate(intent: &Intent, workspace: &str) -> Result<(), String> {
    let root = intent.events.first().ok_or("empty purge intent")?.root_id;
    if intent.format != 1 || intent.workspace != workspace {
        return Err("unsupported purge intent".into());
    }
    for event in &intent.events {
        event.validate().map_err(|_| "invalid purge intent")?;
        if event.root_id != root || !koru_replication::cloud::transfer::removal::fits(event) {
            return Err("invalid purge intent".into());
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn rename(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[cfg(windows)]
fn rename(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // No replacement: every request filename is its immutable event UUID.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn intent_replays_exactly_and_rejects_other_workspace_or_changed_payload() {
        let dir = tempfile::tempdir().unwrap();
        let id = koru_core::DocumentId::new().as_uuid();
        let event = Event::new(id, id, None, "device");
        write(dir.path(), "workspace", &[event.clone()]).unwrap();
        write(dir.path(), "workspace", &[event.clone()]).unwrap();
        assert_eq!(
            read(dir.path(), "workspace").unwrap(),
            vec![vec![event.clone()]]
        );
        assert!(read(dir.path(), "different").is_err());
        let mut changed = event;
        changed.by = "another device".into();
        assert!(write(dir.path(), "workspace", &[changed]).is_err());
    }
}
