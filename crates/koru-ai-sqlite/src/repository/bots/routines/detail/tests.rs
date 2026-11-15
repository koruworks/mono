// SPDX-License-Identifier: MPL-2.0
use super::super::tests::{confirm, fixture, prepared};
use super::*;

#[test]
fn routine_detail_snapshot_detects_metadata_folder_creation_history_visibility_drift() {
    for mutation in 0..5 {
        let (mut repo, bot, owner, input) = fixture();
        let creation = prepared(&mut repo, owner, &input);
        repo.transition_bot_routine_creation(
            &creation.identity,
            creation.observed(),
            confirm(&creation),
            11,
        )
        .unwrap();
        let subject = repo.routine_read_subject(owner, bot.id).unwrap();
        let snapshot = repo
            .routine_detail_snapshot(&subject, creation.identity.schedule, None, 16)
            .unwrap();
        repo.validate_routine_detail(&snapshot).unwrap();
        match mutation {
            0 => {
                repo.connection
                    .execute(
                        "UPDATE bots SET revision=revision+1 WHERE bot_id=?1",
                        [bot.id.to_storage()],
                    )
                    .unwrap();
            }
            1 => {
                repo.connection
                    .execute(
                        "UPDATE bots SET working_folder='C:/new-folder' WHERE bot_id=?1",
                        [bot.id.to_storage()],
                    )
                    .unwrap();
            }
            2 => {
                repo.connection
                    .execute(
                        "UPDATE bots SET model='new-model' WHERE bot_id=?1",
                        [bot.id.to_storage()],
                    )
                    .unwrap();
            }
            3 => {
                repo.connection.execute("UPDATE bot_routines SET updated_at_ms=updated_at_ms+1 WHERE schedule_id=?1",[creation.identity.schedule.to_storage()]).unwrap();
            }
            _ => {
                repo.connection
                    .execute(
                        "UPDATE chats SET profile_id=?1 WHERE id=?2",
                        params![ProfileId::new().to_storage(), bot.main_chat.to_storage()],
                    )
                    .unwrap();
            }
        }
        assert!(
            repo.validate_routine_detail(&snapshot).is_err(),
            "mutation {mutation}"
        );
    }
    let (mut repo, bot, owner, input) = fixture();
    let creation = prepared(&mut repo, owner, &input);
    repo.connection
        .execute(
            "UPDATE bots SET working_folder='relative invalid' WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    let subject = repo.routine_read_subject(owner, bot.id).unwrap();
    let snapshot = repo
        .routine_detail_snapshot(&subject, creation.identity.schedule, Some(i64::MIN), 16)
        .unwrap();
    let detail = snapshot
        .response(-100, RoutineDetailLive::Unavailable {})
        .unwrap();
    assert_eq!(
        detail.destination,
        RoutineDestinationObservation::Unavailable {}
    );
    assert_eq!(
        detail.local_history,
        RoutineLocalHistoryObservation::Unavailable {}
    );
    assert!(
        matches!(detail.readiness,RoutineReadinessObservation::Blocked {reasons} if reasons.contains(&RoutineReadinessReason::CreationNotBound))
    );
    repo.connection
        .execute(
            "UPDATE bots SET working_folder='another invalid' WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    assert_eq!(
        repo.validate_routine_detail(&snapshot),
        Err(BotError::Conflict)
    );
    for limit in [0, 17, 255] {
        assert!(matches!(
            repo.routine_detail_snapshot(&subject, creation.identity.schedule, None, limit),
            Err(BotError::Invalid)
        ));
    }
}
