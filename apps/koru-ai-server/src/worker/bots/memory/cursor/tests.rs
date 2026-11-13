// SPDX-License-Identifier: MPL-2.0
use super::*;

fn scope(subject: Subject) -> Scope {
    Scope {
        owner: BotOwner {
            user_scope: UserScopeId::new(),
            device: DeviceId::new(),
        },
        bot: BotId::mint(),
        profile: ProfileId::new(),
        page: BotPage {
            workspace: WorkspaceId::new(),
            document: DocumentId::new(),
        },
        creation_request: BotRequestId::mint(),
        view: MemoryView::Current,
        subject,
    }
}
fn subjects() -> [Subject; 2] {
    [
        Subject::Owner {},
        Subject::Run {
            run: RunId::mint(),
            generation: AttemptGeneration::from_raw(u32::MAX).unwrap(),
            submission: BotRequestId::mint(),
        },
    ]
}
fn positions() -> [MemoryReadPosition; 2] {
    [
        MemoryReadPosition::List {
            after: 50_000,
            limit: 20,
        },
        MemoryReadPosition::Detail {
            fact_id: BotMemoryNodeId::from_uuid(uuid::Uuid::nil()),
            node_ordinal: 50_000,
            start_utf8: 8 * 1024 * 1024,
        },
    ]
}

#[test]
fn maximum_canonical_owner_run_list_detail_tokens_fit_issued_ceiling() {
    for subject in subjects() {
        let mut scope = scope(subject);
        let key = CursorKey::new(scope.owner);
        for view in [MemoryView::Current, MemoryView::Captured] {
            if matches!(scope.subject, Subject::Owner {}) && view == MemoryView::Captured {
                continue;
            }
            scope.view = view;
            for position in positions() {
                let version = DocumentVersionId::new();
                let now = u64::MAX - TTL_MS - 1;
                let token = key.issue_at(&scope, version, &position, None, now).unwrap();
                assert!(
                    token.len() <= BOT_MEMORY_ISSUED_CURSOR_BYTES,
                    "{}",
                    token.len()
                );
                let opened = key.open_at(&token, &scope, now).unwrap();
                assert_eq!(opened.version, version);
                assert_eq!(opened.position, position);
                assert_eq!(opened.lifetime, (now, u64::MAX - 1));
                // Even invalid traversal extrema cannot make the encoding larger
                // than the issued ceiling. They still fail semantic validation.
                let bytes = URL_SAFE_NO_PAD.decode(&token).unwrap();
                let mut claims: Claims =
                    serde_json::from_slice(&bytes[..bytes.len() - 32]).unwrap();
                claims.n = match claims.n {
                    Position::List { .. } => Position::List {
                        a: u32::MAX,
                        l: u16::MAX,
                    },
                    Position::Detail { f, .. } => Position::Detail {
                        f,
                        o: u32::MAX,
                        p: u32::MAX,
                    },
                };
                let mut payload = serde_json::to_vec(&claims).unwrap();
                payload.extend_from_slice(key.mac(&payload).as_bytes());
                assert!(URL_SAFE_NO_PAD.encode(payload).len() <= BOT_MEMORY_ISSUED_CURSOR_BYTES);
                assert_eq!(validate(&claims, now), Err(CursorError::Invalid));
            }
        }
    }
}

#[test]
fn cursor_binds_all_subject_and_snapshot_claims_and_rejects_tampering() {
    let scope = scope(subjects()[1].clone());
    let key = CursorKey::new(scope.owner);
    let token = key
        .issue_at(&scope, DocumentVersionId::new(), &positions()[1], None, 10)
        .unwrap();
    let mut mutations = Vec::new();
    let mut changed = scope.clone();
    changed.owner.user_scope = UserScopeId::new();
    mutations.push(changed);
    let mut changed = scope.clone();
    changed.owner.device = DeviceId::new();
    mutations.push(changed);
    let mut changed = scope.clone();
    changed.bot = BotId::mint();
    mutations.push(changed);
    let mut changed = scope.clone();
    changed.profile = ProfileId::new();
    mutations.push(changed);
    let mut changed = scope.clone();
    changed.page.workspace = WorkspaceId::new();
    mutations.push(changed);
    let mut changed = scope.clone();
    changed.page.document = DocumentId::new();
    mutations.push(changed);
    let mut changed = scope.clone();
    changed.creation_request = BotRequestId::mint();
    mutations.push(changed);
    let mut changed = scope.clone();
    changed.view = MemoryView::Captured;
    mutations.push(changed);
    let mut changed = scope.clone();
    changed.subject = Subject::Owner {};
    mutations.push(changed);
    let Subject::Run {
        run,
        generation,
        submission,
    } = scope.subject
    else {
        panic!()
    };
    for subject in [
        Subject::Run {
            run: RunId::mint(),
            generation,
            submission,
        },
        Subject::Run {
            run,
            generation: AttemptGeneration::FIRST,
            submission,
        },
        Subject::Run {
            run,
            generation,
            submission: BotRequestId::mint(),
        },
    ] {
        let mut changed = scope.clone();
        changed.subject = subject;
        mutations.push(changed);
    }
    for changed in mutations {
        assert!(matches!(
            key.open_at(&token, &changed, 10),
            Err(CursorError::Invalid)
        ));
    }
    let mut bytes = URL_SAFE_NO_PAD.decode(&token).unwrap();
    bytes[12] ^= 1;
    assert!(matches!(
        key.open_at(&URL_SAFE_NO_PAD.encode(bytes), &scope, 10),
        Err(CursorError::Invalid)
    ));
    assert!(matches!(
        key.open_at(&(token.clone() + "="), &scope, 10),
        Err(CursorError::Invalid)
    ));
    assert!(matches!(
        key.open_at(&"a".repeat(BOT_MEMORY_CURSOR_BYTES + 1), &scope, 10),
        Err(CursorError::Invalid)
    ));
}

#[test]
fn cursor_continuations_keep_original_expiry_and_restart_invalidates() {
    let scope = scope(Subject::Owner {});
    let key = CursorKey::new(scope.owner);
    let version = DocumentVersionId::new();
    let token = key
        .issue_at(&scope, version, &positions()[0], None, 10)
        .unwrap();
    let opened = key.open_at(&token, &scope, TTL_MS).unwrap();
    let next = key
        .issue_at(
            &scope,
            version,
            &positions()[0],
            Some(opened.lifetime),
            TTL_MS,
        )
        .unwrap();
    assert_eq!(
        key.open_at(&next, &scope, TTL_MS).unwrap().lifetime,
        (10, TTL_MS + 10)
    );
    assert!(matches!(
        key.open_at(&next, &scope, TTL_MS + 10),
        Err(CursorError::Expired)
    ));
    assert!(matches!(
        CursorKey::new(scope.owner).open_at(&token, &scope, 10),
        Err(CursorError::Invalid)
    ));
}

#[test]
fn signed_unknown_fields_noncanonical_json_and_bad_position_refuse() {
    let scope = scope(Subject::Owner {});
    let key = CursorKey::new(scope.owner);
    let token = key
        .issue_at(&scope, DocumentVersionId::new(), &positions()[0], None, 0)
        .unwrap();
    let decoded = URL_SAFE_NO_PAD.decode(token).unwrap();
    let payload = &decoded[..decoded.len() - 32];
    let mut value: serde_json::Value = serde_json::from_slice(payload).unwrap();
    value["unknown"] = true.into();
    let unknown = serde_json::to_vec(&value).unwrap();
    let spaced = [b" ".as_slice(), payload].concat();
    for mut payload in [unknown, spaced] {
        payload.extend_from_slice(key.mac(&payload).as_bytes());
        assert!(matches!(
            key.open_at(&URL_SAFE_NO_PAD.encode(payload), &scope, 0),
            Err(CursorError::Invalid)
        ));
    }
}

#[test]
fn continuation_cannot_change_operation_fact_or_limit() {
    let scope = scope(Subject::Owner {});
    let key = CursorKey::new(scope.owner);
    let fact_id = BotMemoryNodeId::from_uuid(uuid::Uuid::new_v4());
    let token = key
        .issue(
            &scope,
            DocumentVersionId::new(),
            &MemoryReadPosition::Detail {
                fact_id,
                node_ordinal: 1,
                start_utf8: 0,
            },
            None,
        )
        .unwrap();
    let operation = MemoryReadOperation::Detail {
        view: MemoryView::Current,
        fact_id,
        cursor: Some(token.clone()),
    };
    assert!(key.open(&token, &scope, &operation).is_ok());
    for operation in [
        MemoryReadOperation::Detail {
            view: MemoryView::Current,
            fact_id,
            cursor: None,
        },
        MemoryReadOperation::Detail {
            view: MemoryView::Current,
            fact_id,
            cursor: Some("different".into()),
        },
        MemoryReadOperation::Detail {
            view: MemoryView::Current,
            fact_id: BotMemoryNodeId::from_uuid(uuid::Uuid::new_v4()),
            cursor: Some(token.clone()),
        },
        MemoryReadOperation::Detail {
            view: MemoryView::Captured,
            fact_id,
            cursor: Some(token.clone()),
        },
        MemoryReadOperation::List {
            view: MemoryView::Current,
            limit: 20,
            cursor: Some(token.clone()),
        },
    ] {
        assert!(matches!(
            key.open(&token, &scope, &operation),
            Err(CursorError::Invalid)
        ));
    }
    let token = key
        .issue(
            &scope,
            DocumentVersionId::new(),
            &MemoryReadPosition::List {
                after: 1,
                limit: 20,
            },
            None,
        )
        .unwrap();
    assert!(matches!(
        key.open(
            &token,
            &scope,
            &MemoryReadOperation::List {
                view: MemoryView::Current,
                limit: 19,
                cursor: Some(token.clone())
            }
        ),
        Err(CursorError::Invalid)
    ));
}
