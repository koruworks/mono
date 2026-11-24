// SPDX-License-Identifier: MPL-2.0

//! Blocking reads, frozen-source inspection, and recoverable seeded copies.
//! Nothing here runs on the event loop. Journals contain content and remain
//! under the machine-local Store root, never in a synced profile directory.

use super::*;
use crate::store::{outbox, revisions};
use koru_core::schema::WorkspaceDoc;
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Clone)]
pub(super) struct Source {
    pub snapshot: Vec<u8>,
    pub version: loro::VersionVector,
    store_version: Option<Vec<u8>>,
    replica: u64,
    pub title: String,
    files: Vec<(PathBuf, String)>,
}

pub(super) enum Outcome {
    Prompt(Source),
    Prepared {
        source: Source,
        replacement: Option<DocumentId>,
    },
    Released {
        source: Option<Source>,
        replacement: Option<DocumentId>,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    format: u8,
    workspace: String,
    document: String,
    replica: u64,
    version: Vec<u8>,
    candidate: Option<String>,
    title: String,
    parent: Option<String>,
    kind: String,
    initial_update: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Release {
    format: u8,
    workspace: String,
    document: String,
    version: Vec<u8>,
    store_version: Option<Vec<u8>>,
    replacement: Option<String>,
    files: Vec<(PathBuf, String)>,
}

fn client(context: &Context) -> Result<koru_api_client::Client, String> {
    match context.root.as_deref() {
        Some(root) => koru_api_client::Client::open_at(&context.segment, root),
        None => koru_api_client::Client::open(&context.segment),
    }
    .map_err(|e| e.to_string())
}

fn base(context: &Context) -> Result<PathBuf, String> {
    context
        .root
        .clone()
        .or_else(koru_channel::store_root)
        .ok_or("Store root is unavailable".into())
}

fn decision_dir(context: &Context) -> Result<PathBuf, String> {
    Ok(base(context)?
        .join("purge-decisions")
        .join(context.workspace.to_storage())
        .join(context.document.to_storage()))
}

/// Freeze Store admission before reading the source. The Store vector is a
/// release fence, separate from the complete local vector including outbox
/// revisions. An old-id upload is never needed to save pending writing.
fn read_source(
    context: &Context,
    captured: Option<Vec<u8>>,
    retained: Option<&Source>,
) -> Result<Option<Source>, String> {
    let copies = base(context)?
        .join("purge-copies")
        .join(context.workspace.to_storage())
        .join(context.document.to_storage());
    if let Some(snapshot) = &captured {
        let path = copies.join(format!("{}.loro", koru_store_core::checksum(snapshot)));
        crate::store::purge_intent::write_once(&path, snapshot)?;
    }
    let client = client(context)?;
    let workspace = explore::store_workspace(context.workspace);
    let document = explore::store_document(context.document);
    let snapshot = match client.freeze_document_purge(workspace, document) {
        Ok(snapshot) => Some(snapshot),
        Err(error) if error.code() == koru_store_core::errors::Code::DocumentNotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    let mut doc = snapshot
        .as_deref()
        .map(WorkspaceDoc::from_snapshot)
        .transpose()
        .map_err(|e| e.to_string())?;
    let store_version = doc.as_ref().map(|doc| doc.version().encode());
    for bytes in retained
        .map(|source| source.snapshot.as_slice())
        .into_iter()
        .chain(captured.as_deref())
    {
        if let Some(doc) = &mut doc {
            doc.import(bytes).map_err(|e| e.to_string())?;
        } else {
            doc = Some(WorkspaceDoc::from_snapshot(bytes).map_err(|e| e.to_string())?);
        }
    }
    let mut files = Vec::new();
    // A frozen editor can hold changes the Store never acknowledged. Keep a
    // machine-local full snapshot before asking; an unanswered prompt must
    // survive process death without re-uploading the purged identity.
    match fs::read_dir(&copies) {
        Ok(entries) => {
            for entry in entries {
                let path = entry.map_err(|e| e.to_string())?.path();
                if path.extension().and_then(|extension| extension.to_str()) != Some("loro") {
                    continue;
                }
                let bytes = fs::read(&path).map_err(|e| e.to_string())?;
                if let Some(doc) = &mut doc {
                    doc.import(&bytes).map_err(|e| e.to_string())?;
                } else {
                    doc = Some(WorkspaceDoc::from_snapshot(&bytes).map_err(|e| e.to_string())?);
                }
                remember_file(&mut files, &path)?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    let (buffers, errors) = outbox::read_all(context.root.as_deref(), workspace, document);
    if !errors.is_empty() {
        return Err("unreadable document outbox; local content retained".into());
    }
    for buffer in buffers {
        if doc.is_none() {
            doc = Some(WorkspaceDoc::from_snapshot(&buffer.updates).map_err(|e| e.to_string())?);
        }
        let doc = doc.as_mut().expect("source initialized");
        if !buffer.applies_to(&doc.version()) {
            return Err("document outbox has missing causal ancestors".into());
        }
        doc.import(&buffer.updates).map_err(|e| e.to_string())?;
        remember_file(&mut files, &buffer.path)?;
    }
    if let Some(dir) = outbox::dir(context.root.as_deref(), workspace, document) {
        let found = revisions::discover(&revisions::DiskFs, &dir).map_err(|e| e.to_string())?;
        if !found.damaged.is_empty() || !found.incomplete.is_empty() || !found.parts.is_empty() {
            return Err("incomplete document revision; local content retained".into());
        }
        for revision in found.complete {
            if doc.is_none() {
                let page = koru_core::ids::PageKey::parse(&revision.record.page)
                    .ok_or("invalid revision page")?;
                if koru_core::ids::page_document_id(
                    context.workspace,
                    page,
                    revision.record.generation,
                ) != context.document
                {
                    return Err("revision identity mismatch".into());
                }
                doc = Some(
                    WorkspaceDoc::page_genesis(context.workspace, context.document)
                        .map_err(|e| e.to_string())?,
                );
            }
            let updates_path = revision.dir.join(revisions::UPDATES);
            let bytes = fs::read(&updates_path).map_err(|e| e.to_string())?;
            let doc = doc.as_mut().expect("source initialized");
            doc.import(&bytes).map_err(|e| e.to_string())?;
            let version =
                loro::VersionVector::decode(&revision.record.version).map_err(|e| e.to_string())?;
            if !doc.version().includes_vv(&version) {
                return Err("revision has missing causal ancestors".into());
            }
            remember_file(&mut files, &updates_path)?;
            // A record is removed after its content, and only exact captured
            // files are removed. A concurrently rewritten revision is kept.
            remember_file(&mut files, &revision.record_path)?;
        }
    }
    let Some(doc) = doc else {
        return Ok(None);
    };
    let title = if context.title.is_empty() {
        crate::document_root(&doc)
            .and_then(|root| doc.read(root).ok())
            .and_then(|node| node.content.as_text().map(str::to_owned))
            .or_else(|| doc.name())
            .unwrap_or_default()
    } else {
        context.title.clone()
    };
    let snapshot = doc.export_snapshot().map_err(|e| e.to_string())?;
    let persisted = store_version
        .as_deref()
        .and_then(|bytes| loro::VersionVector::decode(bytes).ok())
        .is_some_and(|version| version.includes_vv(&doc.version()));
    if !persisted {
        let path = copies.join(format!("{}.loro", koru_store_core::checksum(&snapshot)));
        crate::store::purge_intent::write_once(&path, &snapshot)?;
        if !files.iter().any(|(existing, _)| existing == &path) {
            remember_file(&mut files, &path)?;
        }
    }
    Ok(Some(Source {
        snapshot,
        version: doc.version(),
        store_version,
        replica: retained
            .map(|source| source.replica)
            .unwrap_or_else(|| doc.peer_id()),
        title,
        files,
    }))
}

fn remember_file(files: &mut Vec<(PathBuf, String)>, path: &Path) -> Result<(), String> {
    files.push((
        path.to_path_buf(),
        koru_store_core::checksum(&fs::read(path).map_err(|e| e.to_string())?),
    ));
    Ok(())
}

pub(super) fn inspect(
    context: Context,
    captured: Option<Vec<u8>>,
    events: Vec<Event>,
) -> Result<Outcome, String> {
    if let Some(outcome) = resume_release(&context, captured.as_deref())? {
        return Ok(outcome);
    }
    // OCR is derived content, never a kept document. Its schema is independent
    // of WorkspaceDoc; regenerating readings follows the new source id.
    if context.kind == koru_core::explore::KIND_READING {
        let bytes = match client(&context)?.freeze_document_purge(
            explore::store_workspace(context.workspace),
            explore::store_document(context.document),
        ) {
            Ok(bytes) => bytes,
            Err(error) if error.code() == koru_store_core::errors::Code::DocumentNotFound => {
                return Ok(Outcome::Released {
                    source: None,
                    replacement: None,
                });
            }
            Err(error) => return Err(error.to_string()),
        };
        let doc = loro::LoroDoc::new();
        doc.import(&bytes).map_err(|e| e.to_string())?;
        let version = doc.oplog_vv();
        return Ok(Outcome::Prepared {
            source: Source {
                snapshot: bytes,
                store_version: Some(version.encode()),
                version,
                replica: doc.peer_id(),
                title: context.title,
                files: Vec::new(),
            },
            replacement: None,
        });
    }
    let Some(source) = read_source(&context, captured, None)? else {
        return Ok(Outcome::Released {
            source: None,
            replacement: None,
        });
    };
    if let Some(decision) = previous_decision(&context, &source.version)? {
        let replacement = resume_decision(&context, &decision)?;
        return Ok(Outcome::Prepared {
            source,
            replacement,
        });
    }
    let local_intents = crate::store::purge_intent::read(
        &base(&context)?
            .join("purges")
            .join(context.workspace.to_storage()),
        &context.workspace.to_storage(),
    )?;
    let local = local_intents
        .iter()
        .flatten()
        .filter(|event| event.document_id == context.document.as_uuid())
        .collect::<Vec<_>>();
    let local_authority = !local.is_empty()
        && Coverage::of(local).is_some_and(|coverage| {
            matches!(coverage, Coverage::Unknown) || coverage.covers(&source.version)
        });
    let covered = Coverage::of(&events).is_some_and(|coverage| coverage.covers(&source.version));
    if local_authority || covered {
        Ok(Outcome::Prepared {
            source,
            replacement: None,
        })
    } else {
        Ok(Outcome::Prompt(source))
    }
}

pub(super) fn decide(
    context: Context,
    previous: Source,
    captured: Option<Vec<u8>>,
    choice: DocumentPurgeDecision,
) -> Result<Outcome, String> {
    let source = read_source(&context, captured, Some(&previous))?
        .ok_or("local source disappeared before decision")?;
    let decision = match previous_decision(&context, &source.version)? {
        Some(decision) => decision,
        None => {
            let candidate = (choice == DocumentPurgeDecision::Keep).then(DocumentId::new);
            let original =
                WorkspaceDoc::from_snapshot(&source.snapshot).map_err(|e| e.to_string())?;
            let copy = match candidate {
                Some(candidate) => original
                    .fork_as_document(candidate)
                    .map_err(|e| e.to_string())?,
                None => original,
            };
            let decision = Decision {
                format: 1,
                workspace: context.workspace.to_storage(),
                document: context.document.to_storage(),
                replica: source.replica,
                version: source.version.encode(),
                candidate: candidate.map(|id| id.to_storage()),
                title: source.title.clone(),
                parent: context.parent.map(|id| id.to_storage()),
                kind: context.kind.clone(),
                initial_update: if candidate.is_some() {
                    copy.export_all_updates().map_err(|e| e.to_string())?
                } else {
                    Vec::new()
                },
            };
            let path = decision_dir(&context)?.join(format!(
                "{}-{}.json",
                decision.replica,
                koru_store_core::checksum(&decision.version)
            ));
            let bytes = serde_json::to_vec(&decision).map_err(|e| e.to_string())?;
            crate::store::purge_intent::write_once(&path, &bytes)?;
            decision
        }
    };
    let replacement = resume_decision(&context, &decision)?;
    Ok(Outcome::Prepared {
        source,
        replacement,
    })
}

fn previous_decision(
    context: &Context,
    source: &loro::VersionVector,
) -> Result<Option<Decision>, String> {
    let entries = match fs::read_dir(decision_dir(context)?) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let decision: Decision =
            serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if decision.format != 1
            || decision.workspace != context.workspace.to_storage()
            || decision.document != context.document.to_storage()
        {
            return Err("purge decision identity mismatch".into());
        }
        let kept = loro::VersionVector::decode(&decision.version).map_err(|e| e.to_string())?;
        if kept.includes_vv(source) {
            return Ok(Some(decision));
        }
    }
    Ok(None)
}

fn resume_decision(context: &Context, decision: &Decision) -> Result<Option<DocumentId>, String> {
    let Some(candidate) = &decision.candidate else {
        return Ok(None);
    };
    let candidate = DocumentId::parse(candidate).ok_or("invalid kept document id")?;
    let client = client(context)?;
    let workspace = explore::store_workspace(context.workspace);
    let entries = client.documents(workspace).map_err(|e| e.to_string())?;
    let parent = decision
        .parent
        .as_deref()
        .and_then(DocumentId::parse)
        .map(explore::store_document)
        .filter(|parent| {
            entries
                .iter()
                .any(|entry| entry.id == *parent && entry.trashed_at.is_none())
        });
    let hash = koru_store_core::checksum(&decision.initial_update);
    let key = format!(
        "keep:{}:{:016x}:{}",
        context.document,
        decision.replica,
        koru_store_core::checksum(&decision.version)
    );
    let seeded = client
        .seed_document_with_candidate(
            workspace,
            &decision.title,
            &decision.kind,
            parent,
            None,
            explore::store_document(candidate),
            &key,
            &hash,
            decision.initial_update.clone(),
        )
        .map_err(|e| e.to_string())?;
    if seeded.id != explore::store_document(candidate) {
        return Err("seeded copy identity mismatch".into());
    }
    let copy = WorkspaceDoc::from_snapshot(&seeded.initial_update).map_err(|e| e.to_string())?;
    let expected = loro::VersionVector::decode(&decision.version).map_err(|e| e.to_string())?;
    if !copy.version().includes_vv(&expected) {
        return Err("seeded receipt does not cover kept content".into());
    }
    Ok(Some(candidate))
}

/// Called only after the event-loop's newest in-memory version recheck. The
/// Store performs an atomic exact-vector check before releasing its source.
pub(super) fn release(
    context: Context,
    source: Source,
    replacement: Option<DocumentId>,
) -> Result<Outcome, String> {
    let receipt = Release {
        format: 1,
        workspace: context.workspace.to_storage(),
        document: context.document.to_storage(),
        version: source.version.encode(),
        store_version: source.store_version.clone(),
        replacement: replacement.map(|id| id.to_storage()),
        files: source.files.clone(),
    };
    let bytes = serde_json::to_vec(&receipt).map_err(|e| e.to_string())?;
    let path = release_dir(&context)?.join(format!("{}.json", koru_store_core::checksum(&bytes)));
    crate::store::purge_intent::write_once(&path, &bytes)?;
    finish_release(&context, &receipt, &path)?;
    Ok(Outcome::Released {
        source: Some(source),
        replacement,
    })
}

fn release_dir(context: &Context) -> Result<PathBuf, String> {
    Ok(base(context)?
        .join("purge-releases")
        .join(context.workspace.to_storage())
        .join(context.document.to_storage()))
}

fn finish_release(context: &Context, receipt: &Release, path: &Path) -> Result<(), String> {
    let started = std::time::Instant::now();
    let outbox = outbox::dir(
        context.root.as_deref(),
        explore::store_workspace(context.workspace),
        explore::store_document(context.document),
    )
    .ok_or("outbox directory unavailable")?;
    let copies = base(context)?
        .join("purge-copies")
        .join(context.workspace.to_storage())
        .join(context.document.to_storage());
    // The minimal receipt is durable before cleanup. Restart can finish a
    // partially removed revision without trying to decode that partial copy.
    for (file, checksum) in &receipt.files {
        if file
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
            || !(file.starts_with(&outbox) || file.starts_with(&copies))
        {
            return Err("release file is outside its document recovery directories".into());
        }
        match fs::read(file) {
            Ok(bytes) if koru_store_core::checksum(&bytes) == *checksum => {
                fs::remove_file(file).map_err(|e| e.to_string())?;
                if let Some(parent) = file.parent() {
                    let _ = fs::remove_dir(parent);
                }
            }
            Ok(_) => return Err("local revision changed during release; retained".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    if let Some(store_version) = &receipt.store_version {
        client(context)?
            .release_document_purge(
                explore::store_workspace(context.workspace),
                explore::store_document(context.document),
                store_version.clone(),
                receipt
                    .replacement
                    .as_deref()
                    .and_then(DocumentId::parse)
                    .map(explore::store_document),
            )
            .map_err(|e| e.to_string())?;
    }
    crate::store::purge_intent::write_once(&path.with_extension("done"), b"1")?;
    // The seed receipt is now represented by the minimal release marker;
    // remove its redundant content-bearing retry journal.
    if let Ok(entries) = fs::read_dir(decision_dir(context)?) {
        let released = loro::VersionVector::decode(&receipt.version).map_err(|e| e.to_string())?;
        for entry in entries {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let decision: Decision =
                serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            let version =
                loro::VersionVector::decode(&decision.version).map_err(|e| e.to_string())?;
            if released.includes_vv(&version) {
                fs::remove_file(path).map_err(|e| e.to_string())?;
            }
        }
    }
    tracing::info!(target: "cloud.document.removal", document_id = %context.document, stage = "reconciled", code = if receipt.replacement.is_some() { "kept" } else { "deleted" }, age_ms = started.elapsed().as_millis() as u64, age_scope = "release");
    Ok(())
}

fn resume_release(context: &Context, captured: Option<&[u8]>) -> Result<Option<Outcome>, String> {
    let entries = match fs::read_dir(release_dir(context)?) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let captured = captured
        .map(WorkspaceDoc::from_snapshot)
        .transpose()
        .map_err(|e| e.to_string())?;
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let receipt: Release = serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        if receipt.format != 1
            || receipt.workspace != context.workspace.to_storage()
            || receipt.document != context.document.to_storage()
        {
            return Err("release identity mismatch".into());
        }
        let version = loro::VersionVector::decode(&receipt.version).map_err(|e| e.to_string())?;
        if captured
            .as_ref()
            .is_some_and(|doc| !version.includes_vv(&doc.version()))
        {
            continue;
        }
        let mut newer_file = false;
        for (file, checksum) in &receipt.files {
            match fs::read(file) {
                Ok(bytes) if koru_store_core::checksum(&bytes) != *checksum => newer_file = true,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        if newer_file {
            continue;
        }
        // Files created since this decision are unreviewed writing. Never
        // hide them just because an older release already completed.
        let (buffers, errors) = outbox::read_all(
            context.root.as_deref(),
            explore::store_workspace(context.workspace),
            explore::store_document(context.document),
        );
        if !errors.is_empty()
            || buffers
                .iter()
                .any(|buffer| !receipt.files.iter().any(|(file, _)| *file == buffer.path))
        {
            continue;
        }
        if let Some(dir) = outbox::dir(
            context.root.as_deref(),
            explore::store_workspace(context.workspace),
            explore::store_document(context.document),
        ) {
            let revisions =
                revisions::discover(&revisions::DiskFs, &dir).map_err(|e| e.to_string())?;
            if revisions.complete.iter().any(|revision| {
                !receipt
                    .files
                    .iter()
                    .any(|(file, _)| *file == revision.record_path)
            }) || revisions.damaged.iter().any(|revision| {
                !receipt
                    .files
                    .iter()
                    .any(|(file, _)| *file == revision.record_path)
            }) || revisions.incomplete.iter().any(|revision| {
                !receipt
                    .files
                    .iter()
                    .any(|(file, _)| file.starts_with(&revision.dir))
            }) || !revisions.parts.is_empty()
            {
                continue;
            }
        }
        let copies = base(context)?
            .join("purge-copies")
            .join(context.workspace.to_storage())
            .join(context.document.to_storage());
        if let Ok(entries) = fs::read_dir(copies) {
            let mut unreviewed = false;
            for entry in entries {
                let path = entry.map_err(|e| e.to_string())?.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some("loro")
                    && !receipt.files.iter().any(|(file, _)| *file == path)
                {
                    unreviewed = true;
                }
            }
            if unreviewed {
                continue;
            }
        }
        finish_release(context, &receipt, &path)?;
        let replacement = receipt
            .replacement
            .as_deref()
            .map(|id| DocumentId::parse(id).ok_or("invalid release replacement"))
            .transpose()?;
        return Ok(Some(Outcome::Released {
            source: Some(Source {
                snapshot: Vec::new(),
                version,
                store_version: receipt.store_version,
                replica: 0,
                title: context.title.clone(),
                files: receipt.files,
            }),
            replacement,
        }));
    }
    Ok(None)
}

#[cfg(test)]
mod tests;
