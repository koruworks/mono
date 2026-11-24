// SPDX-License-Identifier: MPL-2.0

use super::*;

#[derive(Default)]
struct Memory {
    values: BTreeMap<String, String>,
    flushed: bool,
    writes: usize,
    fail_write: Option<usize>,
}

impl Memory {
    fn flush(&mut self) -> Result<()> {
        self.flushed = true;
        Ok(())
    }
    fn set(&mut self, key: &str, value: &str) -> Result<()> {
        assert!(self.flushed, "receipt preceded durable SystemDoc flush");
        self.writes += 1;
        if self.fail_write == Some(self.writes) {
            return Err(Error::bare(Code::StorageFailed));
        }
        self.values.insert(key.to_string(), value.to_string());
        Ok(())
    }
    fn cursor(&self, workspace: WorkspaceId) -> Option<Cursor> {
        decode(self.values.get(&cursor_key(workspace)).cloned()).unwrap()
    }
}

#[test]
fn receipts_follow_flush_and_cursor_follows_every_receipt() {
    let workspace = WorkspaceId::new();
    let incarnation = Uuid::new_v4();
    let system = DocumentId::new();
    let events = [Uuid::new_v4(), Uuid::new_v4()];
    let page = Page::prepare(workspace, incarnation, system, None, &events, 7).unwrap();
    let mut memory = Memory::default();
    assert!(
        page.persist(
            &mut memory,
            |_| Err(Error::bare(Code::StorageFailed)),
            Memory::set
        )
        .is_err()
    );
    assert!(memory.values.is_empty());
    for fail in 1..=3 {
        let mut memory = Memory {
            fail_write: Some(fail),
            ..Default::default()
        };
        assert!(
            page.persist(&mut memory, Memory::flush, Memory::set)
                .is_err()
        );
        assert!(
            memory.cursor(workspace).is_none(),
            "partial page advanced cursor"
        );
        assert_eq!(memory.values.len(), fail - 1);
        memory.fail_write = None;
        // Reopening sees only persisted state. A missing binding safely
        // restarts the generation even if some event keys already exist.
        let retry = Page::prepare(
            workspace,
            incarnation,
            system,
            memory.cursor(workspace),
            &events,
            7,
        )
        .unwrap();
        retry
            .persist(&mut memory, Memory::flush, Memory::set)
            .unwrap();
        let cursor = memory.cursor(workspace).unwrap();
        assert_eq!(cursor.cursor, 7);
        assert!(
            events
                .iter()
                .all(|event| memory.values[&event_key(workspace, *event)]
                    == cursor.generation.to_string())
        );
        assert_eq!(memory.values.len(), 3);
    }
}

#[test]
fn cursor_never_regresses_and_scope_replacement_invalidates_old_receipts() {
    let workspace = WorkspaceId::new();
    let incarnation = Uuid::new_v4();
    let system = DocumentId::new();
    let event = Uuid::new_v4();
    let mut memory = Memory::default();
    Page::prepare(workspace, incarnation, system, None, &[event], 10)
        .unwrap()
        .persist(&mut memory, Memory::flush, Memory::set)
        .unwrap();
    let original = memory.cursor(workspace).unwrap().generation;
    Page::prepare(
        workspace,
        incarnation,
        system,
        memory.cursor(workspace),
        &[event],
        2,
    )
    .unwrap()
    .persist(&mut memory, Memory::flush, Memory::set)
    .unwrap();
    assert_eq!(memory.cursor(workspace).unwrap().cursor, 10);
    assert_eq!(memory.cursor(workspace).unwrap().generation, original);
    for (new_incarnation, new_system) in [
        (Uuid::new_v4(), system),
        (incarnation, DocumentId::new()),
        (incarnation, system),
    ] {
        let old = memory.cursor(workspace).unwrap().generation;
        Page::prepare(
            workspace,
            new_incarnation,
            new_system,
            memory.cursor(workspace),
            &[],
            0,
        )
        .unwrap()
        .persist(&mut memory, Memory::flush, Memory::set)
        .unwrap();
        let current = memory.cursor(workspace).unwrap();
        assert_eq!(current.cursor, 0);
        assert_ne!(current.generation, old);
        assert_ne!(
            memory.values[&event_key(workspace, event)],
            current.generation.to_string()
        );
    }
}

#[test]
fn page_keys_are_bounded_and_input_validation_precedes_writes() {
    let workspace = WorkspaceId::new();
    let incarnation = Uuid::new_v4();
    let system = DocumentId::new();
    let event = Uuid::new_v4();
    let page = Page::prepare(workspace, incarnation, system, None, &[event, event], 0).unwrap();
    assert_eq!(page.receipt_keys.len(), 1);
    assert!(page.receipt_keys[0].len() < 128);
    assert_eq!(page.receipt_value.len(), 36);
    for events in [vec![event; 65], vec![Uuid::nil()]] {
        assert!(Page::prepare(workspace, incarnation, system, None, &events, 1).is_err());
    }
    assert!(decode(Some("{}".into())).is_err());
    assert!(Page::prepare(workspace, Uuid::nil(), system, None, &[], 0).is_err());
}
