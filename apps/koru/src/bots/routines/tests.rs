// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn accepted_creation_retires_an_older_read_and_is_fresh_after_reopening() {
    let mut routines = Routines::default();
    let bot = BotId::mint();
    routines.show(Some(bot), true);
    let Command::ReadRoutines { seq: old, .. } = routines.read().unwrap() else {
        panic!()
    };
    routines.creation_accepted();
    let Command::ReadRoutines { seq: fresh, .. } = routines.read().unwrap() else {
        panic!()
    };
    assert!(fresh > old);
    assert!(!routines.answer(old, bot, Err(ReadFailure::TransportOnly)));
    routines.show(Some(bot), false);
    routines.creation_accepted();
    assert!(routines.read().is_none());
    routines.show(Some(bot), true);
    assert!(matches!(
        routines.read(),
        Some(Command::ReadRoutines { after: None, .. })
    ));
}
