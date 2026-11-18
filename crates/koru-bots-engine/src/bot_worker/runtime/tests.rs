// SPDX-License-Identifier: MPL-2.0
use super::*;
fn entry(confirmed: bool) -> Entry {
    Entry {
        reservation: Reservation {
            key: crate::capacity::SessionKey {
                bot: Uuid::new_v4(),
                thread: Uuid::new_v4(),
            },
            session: Uuid::new_v4(),
            assignment: Uuid::new_v4(),
        },
        port: None,
        finished: Some((Some(SetupFailure::Provider), confirmed)),
        failure_recorded: false,
        retry: Instant::now(),
    }
}
#[test]
fn a_stop_fenced_failure_still_acknowledges_the_exact_retired_assignment() {
    let mut entry = entry(true);
    let expected = entry.reservation;
    let mut calls = 0;
    assert!(
        retire(&mut entry, |command| {
            calls += 1;
            match command {
                Command::SetupFailed { reservation, .. } => {
                    assert_eq!(reservation, expected);
                    Ok(ControlReply::Refused(Error::StaleTurn))
                }
                Command::Retired { reservation, turn } => {
                    assert_eq!(calls, 2);
                    assert_eq!(reservation, expected);
                    assert!(turn.is_none());
                    Ok(ControlReply::Done)
                }
                _ => panic!("unexpected command"),
            }
        })
        .unwrap()
    );
    assert_eq!(calls, 2);
}
#[test]
fn unconfirmed_shutdown_never_releases_and_failed_failure_commit_retries() {
    let mut unconfirmed = entry(false);
    assert_eq!(
        retire(&mut unconfirmed, |command| {
            assert!(matches!(command, Command::SetupFailed { .. }));
            Ok(ControlReply::Done)
        })
        .err(),
        Some(Error::EngineUnavailable)
    );
    let mut confirmed = entry(true);
    assert!(
        !retire(&mut confirmed, |command| {
            assert!(matches!(command, Command::SetupFailed { .. }));
            Ok(ControlReply::Refused(Error::StorageUnavailable))
        })
        .unwrap()
    );
    assert!(!confirmed.failure_recorded);
    let mut calls = 0;
    assert!(
        retire(&mut confirmed, |_| {
            calls += 1;
            Ok(ControlReply::Done)
        })
        .unwrap()
    );
    assert_eq!(calls, 2);
}
