// SPDX-License-Identifier: MPL-2.0

use super::*;

pub(super) fn bot_header() -> RunHeader {
    let mut value = header();
    value.scope = WorkspaceScope::BotFolder {
        folder: BotWorkingFolder::new("D:/retained/bot".into()).unwrap(),
    };
    value.bot = Some(BotProvenance {
        parent: None,
        routine: None,
        owner: BotOwner {
            user_scope: koru_store_core::UserScopeId::new(),
            device: device(9),
        },
        bot: BotId::from_uuid(v4(40)),
        submission: BotRequestId::from_uuid(v4(41)),
        chat_id: v4(42),
        ai_attempt_id: Some(v4(43)),
        assistant_message_id: Some(v4(44)),
        config_revision: 2,
        policy_revision: 1,
        continued_from: None,
    });
    value
}

// Model bytes written by an older build without exposing a legacy writer.
pub(super) fn retained_run(header: &RunHeader) -> RunDocument {
    let doc = loro::LoroDoc::new();
    let root = doc.get_map("run");
    root.insert("schema", RUN_BOT_DOCUMENT_SCHEMA_VERSION)
        .unwrap();
    for (key, value) in crate::codec::header_to_keys(header).unwrap() {
        root.insert(&key, value).unwrap();
    }
    doc.commit();
    RunDocument::adopt(doc).unwrap()
}

#[test]
fn retained_manual_bot_in_ordinary_format_is_read_only() {
    let header = bot_header();
    let run = retained_run(&header);
    run.doc().get_map("run").insert("schema", 3).unwrap();
    run.doc().commit();
    let before = run.doc().oplog_vv();
    assert_eq!(run.header().unwrap(), header);
    assert!(run.set_status(&RunStatus::Completed).is_err());
    assert!(run.append_step(&step(StepKind::Completed, 5)).is_err());
    assert!(run.set_rating(&rating()).is_err());
    assert_eq!(run.doc().oplog_vv(), before);
}

#[test]
fn bot_scope_cannot_be_created_without_provenance() {
    let mut header = bot_header();
    header.bot = None;
    assert!(RunDocument::create(&header).is_err());
}
