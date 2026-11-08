// SPDX-License-Identifier: MPL-2.0

use super::*;

/// The contract's own example connection.
fn example() -> ChangeAttribution {
    ChangeAttribution {
        connection: Uuid::parse_str("6f1c0b52-3a43-4a8e-9a43-0d7f4c1e2b9a").unwrap(),
        name: "Claude – work".into(),
        client: "https://claude.ai/oauth/client.json".into(),
        verified: true,
        account: Uuid::parse_str("0b6c7e1d-58a2-4f60-9c1a-5d2e8f3b7a10").unwrap(),
    }
}

fn example_op() -> Uuid {
    Uuid::parse_str("5e9d2c41-7b3f-4a6e-8d15-2c9f0e7a4b63").unwrap()
}

/// Every change in `doc` with its message, oldest first.
fn messages(doc: &loro::LoroDoc) -> Vec<Option<String>> {
    let ids: Vec<loro::ID> = doc.oplog_frontiers().iter().collect();
    let mut changes = Vec::new();
    doc.travel_change_ancestors(&ids, &mut |change| {
        changes.push((change.lamport, change.message.as_deref().map(str::to_owned)));
        std::ops::ControlFlow::Continue(())
    })
    .unwrap();
    changes.sort_by_key(|(lamport, _)| *lamport);
    changes.into_iter().map(|(_, message)| message).collect()
}

/// **Byte for byte the contract's example**: its properties in its order, no
/// insignificant whitespace, `create` last and only on a seed.
#[test]
fn the_message_is_spelled_exactly_as_the_contract_writes_it() {
    assert_eq!(
        example().message(example_op()),
        r#"{"koru":"actor/1","kind":"assistant","connection":"6f1c0b52-3a43-4a8e-9a43-0d7f4c1e2b9a","name":"Claude – work","client":"https://claude.ai/oauth/client.json","verified":true,"account":"0b6c7e1d-58a2-4f60-9c1a-5d2e8f3b7a10","op":"5e9d2c41-7b3f-4a6e-8d15-2c9f0e7a4b63"}"#
    );
    let hash = "9f2c".repeat(16);
    assert_eq!(
        example().seed_message(example_op(), &hash),
        format!(
            r#"{{"koru":"actor/1","kind":"assistant","connection":"6f1c0b52-3a43-4a8e-9a43-0d7f4c1e2b9a","name":"Claude – work","client":"https://claude.ai/oauth/client.json","verified":true,"account":"0b6c7e1d-58a2-4f60-9c1a-5d2e8f3b7a10","op":"5e9d2c41-7b3f-4a6e-8d15-2c9f0e7a4b63","create":{{"request_hash":"{hash}"}}}}"#
        )
    );
}

/// The reader's rules: exactly `actor/1`, unknown properties ignored, and
/// anything else — another version, plain text — is not one.
#[test]
fn a_reader_takes_actor_1_and_nothing_else() {
    let read = ActorMessage::read(&example().seed_message(example_op(), "ab")).unwrap();
    assert_eq!(read.connection, example().connection);
    assert_eq!(read.op, example_op());
    assert_eq!(read.name, "Claude – work");
    assert_eq!(read.create.unwrap().request_hash, "ab");
    assert_eq!(
        ActorMessage::read(&example().message(example_op()))
            .unwrap()
            .create,
        None
    );

    let later =
        example()
            .message(example_op())
            .replacen(r#""kind""#, r#""added_later":[1,2],"kind""#, 1);
    assert!(
        ActorMessage::read(&later).is_some(),
        "an unknown property is ignored"
    );
    for not_one in [
        "Fix the heading".to_owned(),
        example()
            .message(example_op())
            .replace("actor/1", "actor/2"),
        r#"{"kind":"assistant"}"#.to_owned(),
        String::new(),
    ] {
        assert_eq!(ActorMessage::read(&not_one), None, "{not_one}");
    }
}

/// A subscription labels every commit made while it lives — the explicit
/// ones the document model makes after each operation, and the implicit one
/// an export makes — and nothing after it is dropped.
#[test]
fn every_commit_made_while_subscribed_is_labelled_and_none_after() {
    let doc = loro::LoroDoc::new();
    doc.get_map("meta").insert("before", 1).unwrap();
    doc.commit();
    {
        let _labelled = attribute_commits(&doc, "first");
        doc.get_map("meta").insert("one", 1).unwrap();
        doc.commit();
        doc.get_text("body").insert(0, "pending").unwrap();
        // No explicit commit: the export's implicit one is labelled too.
        let _ = doc.export(loro::ExportMode::all_updates()).unwrap();
    }
    doc.get_map("meta").insert("after", 1).unwrap();
    doc.commit();

    let found = messages(&doc);
    assert_eq!(found.first(), Some(&None));
    assert_eq!(found.last(), Some(&None));
    let labelled: Vec<&str> = found.iter().flatten().map(String::as_str).collect();
    assert!(!labelled.is_empty());
    assert!(
        labelled.iter().all(|message| *message == "first"),
        "{found:?}"
    );
}

/// **Producer check (TECHNICAL §16):** a Loro history-retaining snapshot keeps
/// each change's commit message, and so do the update exports a Store and a
/// Cloud upload carry. D191 does not say so; this does.
#[test]
fn a_history_retaining_snapshot_keeps_every_commit_message() {
    let doc = loro::LoroDoc::new();
    doc.get_map("meta").insert("plain", 1).unwrap();
    doc.commit();
    let base = doc.oplog_vv();
    {
        let _labelled = attribute_commits(&doc, &example().message(example_op()));
        doc.get_text("body")
            .insert(0, "an assistant wrote this")
            .unwrap();
        doc.commit();
    }
    {
        let _labelled = attribute_commits(&doc, &example().seed_message(Uuid::new_v4(), "cd"));
        doc.get_map("meta").insert("seeded", true).unwrap();
        doc.commit();
    }
    let written = messages(&doc);
    assert_eq!(written.iter().flatten().count(), 2, "{written:?}");

    let snapshot = doc.export(loro::ExportMode::snapshot()).unwrap();
    let reopened = loro::LoroDoc::new();
    reopened.import(&snapshot).unwrap();
    assert_eq!(messages(&reopened), written, "a snapshot round trip");

    let updates = doc.export(loro::ExportMode::all_updates()).unwrap();
    let replayed = loro::LoroDoc::new();
    replayed.import(&updates).unwrap();
    assert_eq!(messages(&replayed), written, "an all-updates round trip");

    // A peer that had the base receives the rest as an incremental update.
    let peer = loro::LoroDoc::new();
    peer.import(&doc.export(loro::ExportMode::updates_till(&base)).unwrap())
        .unwrap();
    peer.import(&doc.export(loro::ExportMode::updates(&base)).unwrap())
        .unwrap();
    assert_eq!(messages(&peer), written, "an incremental update");

    for message in written.iter().flatten() {
        assert!(ActorMessage::read(message).is_some(), "{message}");
    }
}

/// **No Koru path writes a commit message of its own (TECHNICAL §6.3, §16)**,
/// read off the source: the only code that labels a Loro commit is this
/// module's subscription and the manifest's twin of it in `koru-store-core`,
/// both reached only with an app connection's attribution. Anything else that
/// sets a commit message would make `actor/1` ambiguous, and fails here.
///
/// Test code is skipped: a producer check writes a message on purpose. It
/// reads the checked-in sources and writes nothing. The runtime half — the
/// commits themselves carrying no message on a local host — is
/// `koru-ai-server`'s `no_local_path_writes_a_commit_message`.
#[test]
fn only_attribution_labels_a_commit() {
    const LABELLING: [&str; 5] = [
        "set_next_commit_message",
        "set_next_commit_options",
        "CommitOptions",
        "subscribe_pre_commit",
        ".commit_msg(",
    ];
    const ALLOWED: [&str; 2] = [
        "crates/koru-mcp-server/src/attribution.rs",
        "crates/koru-store-core/src/manifest.rs",
    ];
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut found = Vec::new();
    let mut pending = vec![root.join("crates"), root.join("apps")];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                if !matches!(name.as_str(), "target" | "node_modules" | ".git" | "build") {
                    pending.push(path);
                }
                continue;
            }
            if !name.ends_with(".rs") {
                continue;
            }
            let relative = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            // Test code may write a message on purpose — the producer checks
            // do, to prove one survives — and is no Koru path.
            if relative.ends_with("/tests.rs") || relative.contains("/tests/") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if LABELLING.iter().any(|call| text.contains(call))
                && !ALLOWED.contains(&relative.as_str())
            {
                found.push(relative);
            }
        }
    }
    assert!(
        found.is_empty(),
        "a commit message written outside attribution: {found:?}"
    );
}
