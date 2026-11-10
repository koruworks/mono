// SPDX-License-Identifier: MPL-2.0
use super::*;

fn bot_header() -> RunHeader {
    let mut value = header();
    value.scope = WorkspaceScope::BotFolder {
        folder: BotWorkingFolder::new("D:/owner/bot".into()).unwrap(),
    };
    value.bot = Some(BotProvenance {
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
    value.native_results.push(NativeResultReference {
        operation_id: v4(45),
        workspace: WorkspaceId::from_uuid(v4(46)),
        document: DocumentId::from_uuid(v4(47)),
        version: vec![1, 2, 3],
        title: "Confirmed page".into(),
        confirmed_at_ms: 9,
        kind: NativeResultKind::CreatedPage,
    });
    value.chat_delivery = Some(BotDeliveryReceipt {
        run: value.run,
        attempt: attempt(48),
        generation: AttemptGeneration::FIRST,
        chat_id: v4(42),
        ai_attempt_id: v4(43),
        assistant_message_id: v4(44),
        confirmed_at_ms: 10,
    });
    value.artifacts.push(result(49));
    value
}

#[test]
fn run3_preserves_bot_and_both_result_kinds_during_ordinary_writes() {
    let expected = bot_header();
    let run = RunDocument::create(&expected).unwrap();
    let roundtrip = RunDocument::from_snapshot(&run.export_snapshot().unwrap()).unwrap();
    assert_eq!(roundtrip.header().unwrap(), expected);
    roundtrip.set_status(&RunStatus::Completed).unwrap();
    roundtrip.set_rating(&rating()).unwrap();
    let actual = roundtrip.header().unwrap();
    assert_eq!(actual.bot, expected.bot);
    assert_eq!(actual.native_results, expected.native_results);
    assert_eq!(actual.chat_delivery, expected.chat_delivery);
    assert_eq!(actual.artifacts, expected.artifacts);
}

#[test]
fn run2_does_not_manufacture_bot_ownership() {
    let expected = header();
    let run = RunDocument::create(&expected).unwrap();
    run.doc().get_map("run").insert("schema", 2i64).unwrap();
    run.doc().commit();
    let reopened = RunDocument::from_snapshot(&run.export_snapshot().unwrap()).unwrap();
    assert_eq!(reopened.header().unwrap(), expected);
    assert!(reopened.header().unwrap().bot.is_none());
}

#[test]
fn bot_result_and_delivery_binding_are_validated_before_writing() {
    let mut value = bot_header();
    value.chat_delivery.as_mut().unwrap().chat_id = v4(50);
    assert!(RunDocument::create(&value).is_err());
    value = bot_header();
    value.native_results.push(value.native_results[0].clone());
    assert!(RunDocument::create(&value).is_err());
    value = bot_header();
    value.bot = None;
    assert!(RunDocument::create(&value).is_err());
}

#[test]
fn native_registration_is_idempotent_conflict_safe_and_bot_only() {
    let mut expected = bot_header();
    let reference = expected.native_results.remove(0);
    let doc = RunDocument::create(&expected).unwrap();
    doc.register_native_result(&reference).unwrap();
    let version = doc.version();
    doc.register_native_result(&reference).unwrap();
    assert_eq!(doc.version(), version);
    let mut changed = reference.clone();
    changed.title.push_str(" changed");
    assert!(doc.register_native_result(&changed).is_err());
    assert_eq!(
        doc.header().unwrap().native_results,
        vec![reference.clone()]
    );
    assert!(
        RunDocument::create(&header())
            .unwrap()
            .register_native_result(&reference)
            .is_err()
    );
}
