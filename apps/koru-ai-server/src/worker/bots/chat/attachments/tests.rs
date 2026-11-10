// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_api::Failure;
use koru_delegate_core as d;
use koru_session_protocol as s;
use std::collections::HashMap;

fn fence() -> BotExecutionFence {
    BotExecutionFence {
        owner: d::BotOwner {
            user_scope: koru_ai_core::bots::UserScopeId::new(),
            device: d::DeviceId::new(),
        },
        bot: d::BotId::mint(),
        run: d::RunId::mint(),
        attempt: d::RunAttemptId::mint(),
        generation: d::AttemptGeneration::from_raw(1).unwrap(),
        session: s::SessionId::mint(),
        turn: None,
    }
}

fn staging(pictures: usize) -> Staging {
    Staging::with_pictures(
        (0..pictures)
            .map(|n| (PathBuf::from("dir"), PathBuf::from(format!("p{n}"))))
            .collect(),
    )
}

fn session_copy(id: AttachmentId, size: u64, mode: s::DeliveryMode) -> Attachment {
    Attachment {
        id,
        session_id: s::SessionId::mint(),
        kind: AttachmentKind::Image,
        source: SessionSource::PublicUpload,
        media_type: "image/png".into(),
        byte_count: size,
        dimensions: None,
        status: AttachmentStatus::Pending,
        delivery_mode: mode,
        delivery: s::DeliveryState::Pending,
        turn_id: None,
        preview_available: true,
        created_at_ms: 0,
    }
}

const NATIVE: s::DeliveryMode = s::DeliveryMode::NativeStructured;

/// What the owner was asked, as the fake records it.
#[derive(Debug, PartialEq, Eq)]
enum Asked {
    Ingest(AttachmentId),
    List,
}

/// Prepare and accept the next step, recording the ingested size as the
/// provider thread would.
fn issue(staging: &mut Staging, fence: &BotExecutionFence, size: u64) -> Option<Asked> {
    let step = staging.next(Some(fence))?;
    let asked = match &step.command {
        DelegateCommand::IngestAttachment { attachment_id, .. } => {
            let _ = step.read.as_ref().unwrap().size.set(size);
            Asked::Ingest(*attachment_id)
        }
        DelegateCommand::ListAttachments { .. } => Asked::List,
        _ => panic!("staging issued another command"),
    };
    staging.mark_sent(Some(fence));
    Some(asked)
}

fn ingested(staging: &Staging, index: usize) -> AttachmentId {
    staging.pictures[index].session
}

fn retryable() -> Failure {
    Failure::new("delegate_unavailable", "lost", true)
}

fn frozen(staging: &mut Staging) -> (Vec<AttachmentId>, bool) {
    match staging.settle() {
        Settled::Frozen { native, as_files } => (native, as_files),
        _ => panic!("staging did not freeze"),
    }
}

#[test]
fn confirmed_pictures_freeze_in_order_into_the_initial_input() {
    let fence = fence();
    let mut staging = staging(2);
    for index in 0..2 {
        let id = ingested(&staging, index);
        assert_eq!(issue(&mut staging, &fence, 4), Some(Asked::Ingest(id)));
        assert!(matches!(staging.settle(), Settled::Pending));
        staging
            .apply(Ok(BotReply::Attachment {
                attachment: session_copy(id, 4, NATIVE),
            }))
            .unwrap();
    }
    let ids = vec![ingested(&staging, 0), ingested(&staging, 1)];
    assert_eq!(frozen(&mut staging), (ids, false));
    assert!(matches!(staging.settle(), Settled::AlreadyFrozen));
    assert!(staging.next(Some(&fence)).is_none());
}

#[test]
fn a_lost_ingest_reply_is_listed_and_confirmed_only_by_exact_metadata() {
    let fence = fence();
    let mut staging = staging(1);
    let id = ingested(&staging, 0);
    assert_eq!(issue(&mut staging, &fence, 9), Some(Asked::Ingest(id)));
    staging.apply(Err(retryable())).unwrap();
    assert_eq!(issue(&mut staging, &fence, 9), Some(Asked::List));
    staging
        .apply(Ok(BotReply::Attachments {
            attachments: vec![session_copy(id, 9, NATIVE)],
        }))
        .unwrap();
    assert_eq!(frozen(&mut staging), (vec![id], false));
}

#[test]
fn our_id_listed_with_other_metadata_refuses_the_task() {
    for wrong in [session_copy(AttachmentId::mint(), 9, NATIVE), {
        let mut other = session_copy(AttachmentId::mint(), 8, NATIVE);
        other.turn_id = Some(s::AgentTurnId::mint());
        other
    }] {
        let fence = fence();
        let mut staging = staging(1);
        let id = ingested(&staging, 0);
        issue(&mut staging, &fence, 9);
        staging.apply(Err(retryable())).unwrap();
        issue(&mut staging, &fence, 9);
        let mut listed = wrong.clone();
        listed.id = id;
        listed.byte_count = if wrong.turn_id.is_some() { 9 } else { 8 };
        assert!(
            staging
                .apply(Ok(BotReply::Attachments {
                    attachments: vec![listed],
                }))
                .is_err()
        );
    }
}

#[test]
fn absence_is_uncertain_after_three_listings_and_the_picture_goes_as_its_path() {
    let fence = fence();
    let mut staging = staging(1);
    let id = ingested(&staging, 0);
    issue(&mut staging, &fence, 9);
    staging.apply(Err(retryable())).unwrap();
    for listing in 0..MAX_LISTINGS {
        assert!(matches!(staging.settle(), Settled::Pending), "{listing}");
        assert_eq!(issue(&mut staging, &fence, 9), Some(Asked::List));
        let reply = if listing == 1 {
            Err(retryable())
        } else {
            Ok(BotReply::Attachments {
                attachments: vec![session_copy(AttachmentId::mint(), 9, NATIVE)],
            })
        };
        staging.apply(reply).unwrap();
    }
    assert_eq!(staging.pictures[0].state, State::Uncertain);
    assert_eq!(frozen(&mut staging), (Vec::new(), true));
    assert_ne!(id, AttachmentId::mint());
}

#[test]
fn certain_non_delivery_goes_as_path_and_other_refusals_fail_like_begin() {
    for (failure, outcome) in [
        (Failure::new(NOT_SENT, "unread", false), Some(State::AsPath)),
        (Failure::new(TOO_LARGE, "large", false), Some(State::AsPath)),
        (Failure::new("bot_fenced", "fenced", false), None),
        (
            Failure::new("bot_controller_required", "lease", false),
            None,
        ),
    ] {
        let fence = fence();
        let mut staging = staging(1);
        issue(&mut staging, &fence, 9);
        let applied = staging.apply(Err(failure));
        match outcome {
            Some(state) => {
                applied.unwrap();
                assert_eq!(staging.pictures[0].state, state);
            }
            None => assert!(applied.is_err()),
        }
    }
}

#[test]
fn a_session_without_native_pictures_sends_the_rest_as_paths_without_ipc() {
    let fence = fence();
    let mut staging = staging(3);
    let id = ingested(&staging, 0);
    issue(&mut staging, &fence, 9);
    staging
        .apply(Ok(BotReply::Attachment {
            attachment: session_copy(id, 9, s::DeliveryMode::Unsupported),
        }))
        .unwrap();
    assert!(staging.next(Some(&fence)).is_none());
    assert_eq!(frozen(&mut staging), (Vec::new(), true));
}

#[test]
fn a_step_the_pool_did_not_accept_was_never_sent_and_is_rebuilt() {
    let fence = fence();
    let mut staging = staging(1);
    let id = ingested(&staging, 0);
    for _ in 0..2 {
        let step = staging.next(Some(&fence)).unwrap();
        assert!(matches!(
            step.command,
            DelegateCommand::IngestAttachment { attachment_id, .. } if attachment_id == id
        ));
    }
    assert_eq!(staging.pictures[0].state, State::Planned);
    // An unsolicited reply with nothing in flight is not staging progress.
    assert!(staging.apply(Ok(BotReply::Applied)).is_err());
}

#[test]
fn every_later_step_reuses_the_first_issued_fence() {
    let first = fence();
    let mut staging = staging(2);
    issue(&mut staging, &first, 9);
    staging.apply(Err(retryable())).unwrap();
    let other = fence();
    let step = staging.next(Some(&other)).unwrap();
    assert!(matches!(
        step.command,
        DelegateCommand::ListAttachments { fence } if fence == first
    ));
}

/// Every reply ordering the fake allows, up to four replies per picture
/// across two pictures: an id that may have been sent is never sent again.
#[test]
fn staging_is_never_reentered_for_any_reply_ordering() {
    let id_probe = AttachmentId::mint();
    let replies: Vec<Box<dyn Fn(AttachmentId) -> Result<BotReply, Failure>>> = vec![
        Box::new(|id| {
            Ok(BotReply::Attachment {
                attachment: session_copy(id, 9, NATIVE),
            })
        }),
        Box::new(|id| {
            Ok(BotReply::Attachment {
                attachment: session_copy(id, 9, s::DeliveryMode::Unsupported),
            })
        }),
        Box::new(|_| Err(retryable())),
        Box::new(|_| Err(Failure::new(NOT_SENT, "unread", false))),
        Box::new(|_| Err(Failure::new("bot_fenced", "fenced", false))),
        Box::new(|id| {
            Ok(BotReply::Attachments {
                attachments: vec![session_copy(id, 9, NATIVE)],
            })
        }),
        Box::new(|_| {
            Ok(BotReply::Attachments {
                attachments: vec![],
            })
        }),
        Box::new(|_| Ok(BotReply::Applied)),
    ];
    let n = replies.len();
    let mut orderings = 0usize;
    for code in 0..n.pow(4) {
        let sequence: Vec<usize> = (0..4).map(|i| code / n.pow(i) % n).collect();
        let fence = fence();
        let mut staging = staging(2);
        let mut sent: HashMap<AttachmentId, usize> = HashMap::new();
        for &reply in &sequence {
            let Some(asked) = issue(&mut staging, &fence, 9) else {
                break;
            };
            let target = match asked {
                Asked::Ingest(id) => {
                    *sent.entry(id).or_default() += 1;
                    id
                }
                Asked::List => staging.pictures[staging.in_flight.unwrap()].session,
            };
            if staging.apply(replies[reply](target)).is_err() {
                break;
            }
        }
        assert!(sent.values().all(|count| *count == 1), "{sequence:?}");
        assert!(!sent.contains_key(&id_probe));
        orderings += 1;
    }
    assert_eq!(orderings, n.pow(4));
}

#[test]
fn only_contained_regular_files_within_the_cap_are_read() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("attachment");
    std::fs::create_dir_all(dir.join("file")).unwrap();
    let inside = dir.join("file").join("p.png");
    std::fs::write(&inside, b"picture").unwrap();
    assert_eq!(read_contained(&dir, &inside).unwrap(), b"picture");

    let outside = root.path().join("elsewhere.png");
    std::fs::write(&outside, b"picture").unwrap();
    assert!(
        read_contained(&dir, &outside).is_none(),
        "outside the folder"
    );

    let empty = dir.join("file").join("empty.png");
    std::fs::write(&empty, b"").unwrap();
    assert!(read_contained(&dir, &empty).is_none(), "empty");

    let large = dir.join("file").join("large.png");
    std::fs::File::create(&large)
        .unwrap()
        .set_len(MAX_PICTURE_BYTES + 1)
        .unwrap();
    assert!(read_contained(&dir, &large).is_none(), "over the cap");

    assert!(read_contained(&dir, &dir.join("file").join("gone.png")).is_none());

    // A link, even to a file inside the folder, is not read. Creating one may
    // need a privilege on Windows; the refusal is only checked when it exists.
    let link = dir.join("file").join("link.png");
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_file(&inside, &link).is_ok();
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&inside, &link).is_ok();
    if linked {
        assert!(read_contained(&dir, &link).is_none(), "a link");
    }
}

#[test]
fn an_unreadable_picture_answers_not_sent_without_an_owner_call() {
    let root = tempfile::tempdir().unwrap();
    let read = IngestRead {
        dir: root.path().to_path_buf(),
        original: root.path().join("file").join("missing.png"),
        size: Arc::default(),
    };
    let fence = fence();
    let failure = read
        .load(DelegateCommand::IngestAttachment {
            fence,
            attachment_id: AttachmentId::mint(),
            source: SessionSource::PublicUpload,
            data_base64: String::new(),
        })
        .unwrap_err();
    assert_eq!(failure.code, NOT_SENT);
}

#[test]
fn a_read_picture_fills_the_one_ingest_and_records_its_size() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("file")).unwrap();
    let original = root.path().join("file").join("p.png");
    std::fs::write(&original, b"abc").unwrap();
    let size = Arc::new(OnceLock::new());
    let read = IngestRead {
        dir: root.path().to_path_buf(),
        original,
        size: Arc::clone(&size),
    };
    let command = read
        .load(DelegateCommand::IngestAttachment {
            fence: fence(),
            attachment_id: AttachmentId::mint(),
            source: SessionSource::PublicUpload,
            data_base64: String::new(),
        })
        .unwrap();
    assert!(matches!(
        command,
        DelegateCommand::IngestAttachment { data_base64, .. } if data_base64 == "YWJj"
    ));
    assert_eq!(size.get(), Some(&3));
}
