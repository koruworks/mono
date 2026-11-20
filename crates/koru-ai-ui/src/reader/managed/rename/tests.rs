// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn rename_uses_native_revision_and_returns_the_authoritative_chat() {
    let mut snapshot = crate::managed::tests::opened().snapshot;
    let thread = snapshot.conversation.chat.id;
    let revision = snapshot.revision;
    let mut writes = 0;
    let chat = run(thread, "A useful title", |command| {
        Ok(match command {
            C::Read { thread: actual } => {
                assert_eq!(actual, thread);
                R::Read {
                    snapshot: snapshot.clone(),
                }
            }
            C::Rename {
                id,
                thread: actual,
                revision: expected,
                title,
            } => {
                assert_eq!(actual, thread);
                assert_eq!(expected, revision);
                assert_eq!(title, "A useful title");
                writes += 1;
                snapshot.conversation.chat.title = title;
                snapshot.revision += 1;
                R::Changed {
                    request: id,
                    replayed: false,
                }
            }
            _ => panic!("rename uses only the native thread carrier"),
        })
    })
    .unwrap();
    assert_eq!(writes, 1);
    assert_eq!(chat.id, thread);
    assert_eq!(chat.title, "A useful title");
}

#[test]
fn a_read_for_another_thread_cannot_authorize_a_rename() {
    let snapshot = crate::managed::tests::opened().snapshot;
    let mut calls = 0;
    let result = run(ChatId::new(), "Do not save", |_| {
        calls += 1;
        Ok(R::Read {
            snapshot: snapshot.clone(),
        })
    });
    assert_eq!(result.unwrap_err().code, "invalid_response");
    assert_eq!(calls, 1);
}

#[test]
fn a_conflicting_revision_is_reported_without_retrying_or_falling_back() {
    let snapshot = crate::managed::tests::opened().snapshot;
    let mut calls = 0;
    let result = run(
        snapshot.conversation.chat.id,
        "Do not overwrite",
        |command| {
            calls += 1;
            match command {
                C::Read { .. } => Ok(R::Read {
                    snapshot: snapshot.clone(),
                }),
                C::Rename { .. } => Err(koru_ai_wire::Failure {
                    code: "revision_conflict".into(),
                    message: "Changed elsewhere".into(),
                    retryable: false,
                    details: Default::default(),
                }),
                _ => panic!("unexpected command"),
            }
        },
    );
    assert_eq!(result.unwrap_err().code, "revision_conflict");
    assert_eq!(calls, 2);
}
