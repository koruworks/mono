// SPDX-License-Identifier: MPL-2.0
use super::*;

fn binding(c: &SqliteCatalog) -> NativeEffectBinding {
    NativeEffectBinding {
        user_scope: c.local_user_scope().unwrap(),
        scope: "fixture:native".into(),
        request_key: DocumentId::new().as_uuid(),
        workspace: WorkspaceId::new(),
        document: DocumentId::new(),
        kind: NativeEffectKind::EditDocument,
        request_fingerprint: [7; 32],
    }
}
fn at(ms: i64) -> Timestamp {
    Timestamp::from_millis(ms)
}
fn thirteen() -> SqliteCatalog {
    let mut connection = db::open_in_memory().unwrap();
    db::migrate(&mut connection, "catalog.db", MIGRATIONS, 13).unwrap();
    root_catalog::migrate_to_root_catalog(&mut connection).unwrap();
    SqliteCatalog { connection }
}

#[test]
fn catalog_fourteen_preserves_populated_thirteen_and_refuses_old_readers() {
    let mut old = thirteen();
    let p = Profile::new("Retained", Locale::default(), at(100)).unwrap();
    old.put_profile(&p).unwrap();
    let scope = old.local_user_scope().unwrap();
    assert_eq!(old.schema_version().unwrap(), 13);
    let mut c = SqliteCatalog::from_connection(old.connection).unwrap();
    assert_eq!(c.schema_version().unwrap(), 14);
    assert_eq!(c.profile(p.id).unwrap(), Some(p));
    assert_eq!(c.local_user_scope().unwrap(), scope);
    assert_eq!(
        db::migrate(&mut c.connection, "catalog.db", MIGRATIONS, 13)
            .unwrap_err()
            .code(),
        Code::SchemaTooNew
    );
    assert_eq!(
        SqliteCatalog::from_connection(c.connection)
            .unwrap()
            .schema_version()
            .unwrap(),
        14
    );
    let mut old = thirteen();
    old.connection
        .execute_batch("CREATE TABLE native_effect_fences(conflict TEXT)")
        .unwrap();
    assert!(db::migrate(&mut old.connection, "catalog.db", AFTER_ROOT_MIGRATIONS, 14).is_err());
    assert_eq!(old.schema_version().unwrap(), 13);
    let connection = db::open_in_memory().unwrap();
    connection.execute_batch("PRAGMA user_version=15").unwrap();
    assert_eq!(
        SqliteCatalog::from_connection(connection)
            .err()
            .unwrap()
            .code(),
        Code::SchemaTooNew
    );
}

#[test]
fn first_proof_is_immutable_with_backward_clocks_and_binding_conflicts() {
    let mut c = SqliteCatalog::in_memory().unwrap();
    let b = binding(&c);
    assert!(c.native_effect_fence(&b).unwrap().is_none());
    assert_eq!(
        c.complete_native_effect_drain(&b, at(1))
            .unwrap_err()
            .code(),
        Code::InvalidRequest
    );
    let first = c.fence_native_effect(&b, at(100)).unwrap();
    assert_eq!(c.fence_native_effect(&b, at(200)).unwrap(), first);
    let proof = c.complete_native_effect_drain(&b, at(20)).unwrap();
    assert_eq!(proof.fenced_at, at(100));
    assert_eq!(proof.drained_at, at(20));
    assert_eq!(c.complete_native_effect_drain(&b, at(300)).unwrap(), proof);
    for variant in 0..4 {
        let mut changed = b.clone();
        match variant {
            0 => changed.document = DocumentId::new(),
            1 => changed.workspace = WorkspaceId::new(),
            2 => changed.kind = NativeEffectKind::CreateDocument,
            _ => changed.request_fingerprint = [8; 32],
        }
        assert_eq!(
            c.native_effect_fence(&changed).unwrap_err().code(),
            Code::IdempotencyConflict
        );
        assert_eq!(
            c.fence_native_effect(&changed, at(500)).unwrap_err().code(),
            Code::IdempotencyConflict
        );
        assert_eq!(
            c.complete_native_effect_drain(&changed, at(500))
                .unwrap_err()
                .code(),
            Code::IdempotencyConflict
        );
    }
    let mut other_scope = b.clone();
    other_scope.scope = "fixture:other".into();
    assert!(
        c.native_effect_fence(&other_scope).unwrap().is_none(),
        "scope is part of the operation key"
    );
    let mut foreign = b.clone();
    foreign.user_scope = UserScopeId::new();
    assert_eq!(
        c.native_effect_fence(&foreign).unwrap_err().code(),
        Code::Forbidden
    );
}

#[test]
fn failed_drain_commit_preserves_deny_and_exact_retry_survives_reopen() {
    let mut c = SqliteCatalog::in_memory().unwrap();
    let b = binding(&c);
    c.fence_native_effect(&b, at(100)).unwrap();
    c.connection.execute_batch("CREATE TRIGGER fail_drain BEFORE UPDATE ON native_effect_fences BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(c.complete_native_effect_drain(&b, at(200)).is_err());
    assert!(
        c.native_effect_fence(&b)
            .unwrap()
            .unwrap()
            .drained_at
            .is_none()
    );
    c.connection
        .execute_batch("DROP TRIGGER fail_drain")
        .unwrap();
    let mut reopened = SqliteCatalog::from_connection(c.connection).unwrap();
    let proof = reopened.complete_native_effect_drain(&b, at(300)).unwrap();
    let mut again = SqliteCatalog::from_connection(reopened.connection).unwrap();
    assert_eq!(
        again.complete_native_effect_drain(&b, at(400)).unwrap(),
        proof
    );
}

#[test]
fn content_purge_and_same_workspace_identity_recreation_do_not_remove_fence() {
    let mut c = SqliteCatalog::in_memory().unwrap();
    let p = Profile::new("Fixture", Locale::default(), at(1)).unwrap();
    c.put_profile(&p).unwrap();
    let w = Workspace::new(p.id, "Fixture", at(1)).unwrap();
    c.put_workspace(&w).unwrap();
    let mut b = binding(&c);
    b.workspace = w.id;
    let proof = {
        c.fence_native_effect(&b, at(10)).unwrap();
        c.complete_native_effect_drain(&b, at(20)).unwrap()
    };
    c.purge_workspace(w.id).unwrap();
    assert!(c.workspace(w.id).unwrap().is_none());
    c.put_workspace(&w).unwrap();
    assert_eq!(c.complete_native_effect_drain(&b, at(30)).unwrap(), proof);
    let fk_count: i64 = c
        .connection
        .query_row(
            "SELECT count(*) FROM pragma_foreign_key_list('native_effect_fences')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(fk_count, 0);
}

#[test]
fn invalid_or_corrupt_proof_never_becomes_a_missing_fence() {
    let mut c = SqliteCatalog::in_memory().unwrap();
    let b = binding(&c);
    for key in [
        "00000000-0000-0000-0000-000000000000",
        "11111111-1111-4111-0111-111111111111",
    ] {
        let mut invalid = b.clone();
        invalid.request_key = key.parse().unwrap();
        assert_eq!(
            c.fence_native_effect(&invalid, at(1)).unwrap_err().code(),
            Code::InvalidRequest
        );
    }
    c.fence_native_effect(&b, at(1)).unwrap();
    c.connection.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE native_effect_fences SET document_id='invalid'; PRAGMA ignore_check_constraints=OFF;").unwrap();
    assert_eq!(
        c.native_effect_fence(&b).unwrap_err().code(),
        Code::CorruptState
    );
    assert!(c.complete_native_effect_drain(&b, at(2)).is_err());
}
