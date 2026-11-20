// SPDX-License-Identifier: MPL-2.0
use super::*;
use std::cell::RefCell;
#[derive(Default)]
struct Memory(RefCell<Option<Vec<u8>>>);
impl Storage for Memory {
    fn transact<T>(
        &self,
        action: impl FnOnce(Option<Vec<u8>>) -> Result<(T, Option<Vec<u8>>)>,
    ) -> Result<T> {
        let (out, bytes) = action(self.0.borrow().clone())?;
        if let Some(bytes) = bytes {
            *self.0.borrow_mut() = Some(bytes);
        }
        Ok(out)
    }
}
fn scope() -> Scope {
    Scope {
        profile: "11111111-1111-4111-8111-111111111111".into(),
        workspace: "22222222-2222-4222-8222-222222222222".into(),
    }
}
fn pin(id: u128) -> Pin {
    Pin {
        id: PinId(id),
        family: "33333333-3333-4333-8333-333333333333".into(),
        version: format!("44444444-4444-4444-8444-{id:012x}"),
        title: "Mini app".into(),
    }
}
#[test]
fn operations_are_idempotent_and_scope_local() {
    let memory = Memory::default();
    let apply = |mutation| {
        mutate(
            &memory,
            &Request {
                scope: scope(),
                mutation,
            },
        )
        .unwrap()
    };
    for id in 1..=3 {
        apply(Mutation::Add { pin: pin(id) });
    }
    let move_one = Mutation::Move {
        id: PinId(1),
        to: Placement::After { neighbor: PinId(3) },
    };
    assert_eq!(apply(move_one.clone()), apply(move_one));
    assert_eq!(
        apply(Mutation::Add { pin: pin(1) })
            .iter()
            .map(|p| p.id.0)
            .collect::<Vec<_>>(),
        vec![2, 3, 1]
    );
    assert_eq!(
        apply(Mutation::Remove { id: PinId(2) }),
        apply(Mutation::Remove { id: PinId(2) })
    );
    let mut other = scope();
    other.workspace = other.profile.clone();
    assert!(list(&memory, &other).unwrap().is_empty());
    assert_eq!(list(&memory, &scope()).unwrap().len(), 2);
}
#[test]
fn unsupported_bytes_are_preserved_and_no_mutation_is_written() {
    for bytes in [
        b"bad".to_vec(),
        br#"{"version":2,"scopes":[]}"#.to_vec(),
        br#"{"version":1,"scopes":[],"future":true}"#.to_vec(),
        vec![b' '; MAX_BYTES + 1],
    ] {
        let memory = Memory(RefCell::new(Some(bytes.clone())));
        assert!(
            mutate(
                &memory,
                &Request {
                    scope: scope(),
                    mutation: Mutation::Add { pin: pin(1) }
                }
            )
            .is_err()
        );
        assert_eq!(*memory.0.borrow(), Some(bytes));
    }
}
#[test]
fn missing_neighbor_and_duplicate_identity_do_not_publish() {
    let memory = Memory::default();
    mutate(
        &memory,
        &Request {
            scope: scope(),
            mutation: Mutation::Add { pin: pin(1) },
        },
    )
    .unwrap();
    let before = memory.0.borrow().clone();
    assert!(
        mutate(
            &memory,
            &Request {
                scope: scope(),
                mutation: Mutation::Move {
                    id: PinId(1),
                    to: Placement::Before { neighbor: PinId(9) }
                }
            }
        )
        .is_err()
    );
    assert_eq!(*memory.0.borrow(), before);
    let mut duplicate = pin(2);
    duplicate.id = PinId(1);
    assert!(
        mutate(
            &memory,
            &Request {
                scope: scope(),
                mutation: Mutation::Add { pin: duplicate }
            }
        )
        .is_err()
    );
}
#[test]
fn removing_last_pin_reclaims_a_scope_and_allows_future_workspaces() {
    let mut file = File::default();
    for i in 1..=MAX_SCOPES {
        let mut s = scope();
        s.workspace = format!("22222222-2222-4222-8222-{i:012x}");
        file.apply(
            &s,
            &Mutation::Add {
                pin: pin(i as u128),
            },
        )
        .unwrap();
    }
    let mut first = scope();
    first.workspace = "22222222-2222-4222-8222-000000000001".into();
    file.apply(&first, &Mutation::Remove { id: PinId(1) })
        .unwrap();
    let mut next = scope();
    next.workspace = "22222222-2222-4222-8222-ffffffffffff".into();
    assert!(file.apply(&next, &Mutation::Add { pin: pin(999) }).is_ok());
    assert_eq!(file.scopes.len(), MAX_SCOPES);
}
#[test]
fn injected_pin_paths_are_siblings_outside_profile_files() {
    let root = std::path::Path::new("synthetic-root");
    let paths = SettingsPaths::under_root(root);
    assert_eq!(
        paths.mini_app_pins_file(),
        root.join("mini-app-pins-v1.json")
    );
    assert_ne!(paths.mini_app_pins_file(), paths.desktop_file());
    let resolved = SettingsPaths::from_resolved_files(
        "roaming/preferences.conf".into(),
        "roaming/terminal.conf".into(),
        "machine/desktop-v1.json".into(),
        "update/state.json".into(),
        "state/windows.conf".into(),
    );
    assert_eq!(
        resolved.mini_app_pins_file(),
        std::path::Path::new("machine/mini-app-pins-v1.json")
    );
}
