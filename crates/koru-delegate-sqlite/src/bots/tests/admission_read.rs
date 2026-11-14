// SPDX-License-Identifier: MPL-2.0
use super::*;
use rusqlite::types::Value;

#[test]
fn admission_read_sql_is_exact_current_paused_and_read_only_without_work() {
    let (mut db, _) = memory();
    let s = submission(1);
    let owner = s.provenance.owner;
    let bot = s.provenance.bot;
    assert_eq!(db.bot_barrier(owner, bot).unwrap(), None);
    let barrier = pause(&s, i64::MAX as u64, true);
    db.apply_bot_barrier(&barrier).unwrap();
    let before = db.conn().total_changes();
    assert_eq!(db.bot_barrier(owner, bot).unwrap(), Some(barrier));
    assert_eq!(db.bot_barrier(owner, BotId::mint()).unwrap(), None);
    let mut other = owner;
    other.device = koru_delegate_core::DeviceId::new();
    assert_eq!(db.bot_barrier(other, bot).unwrap(), None);
    other = owner;
    other.user_scope = serde_json::from_value(serde_json::json!(Uuid::new_v4())).unwrap();
    assert_eq!(db.bot_barrier(other, bot).unwrap(), None);
    assert_eq!(db.conn().total_changes(), before);
    assert!(db.unsettled_bot(owner, bot, 1).unwrap().0.is_empty());
}

#[test]
fn admission_read_sql_rejects_corrupt_types_bounds_and_ids_before_allocating() {
    let (mut db, _) = memory();
    let s = submission(1);
    let good = pause(&s, 1, false);
    db.apply_bot_barrier(&good).unwrap();
    db.conn()
        .execute_batch("PRAGMA ignore_check_constraints=ON")
        .unwrap();
    let cases = [
        ("epoch", Value::Real(1.5)),
        ("epoch", Value::Text("bad".into())),
        ("epoch", Value::Integer(0)),
        ("epoch", Value::Integer(-1)),
        ("paused", Value::Real(0.5)),
        ("paused", Value::Text("bad".into())),
        ("paused", Value::Integer(2)),
        ("request_id", Value::Blob(vec![b'a'; 36])),
        ("request_id", Value::Text("x".repeat(4 * 1024 * 1024))),
        ("request_id", Value::Text("é".repeat(36))),
        ("request_id", Value::Text("x".repeat(36))),
        ("request_id", Value::Text(Uuid::nil().to_string())),
    ];
    for (column, value) in cases {
        db.conn()
            .execute(
                "UPDATE bot_admission_barriers SET epoch=1,paused=0,request_id=?1",
                [good.request.to_storage()],
            )
            .unwrap();
        db.conn()
            .execute(
                &format!("UPDATE bot_admission_barriers SET {column}=?1"),
                [value],
            )
            .unwrap();
        assert!(db.bot_barrier(good.owner, good.bot).is_err(), "{column}");
    }
}
