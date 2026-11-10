// SPDX-License-Identifier: MPL-2.0
//! A person's Stop after an automatic interruption or failure keeps every piece
//! of evidence. Its stronger intent cannot be recorded in the attempt status
//! without a schema change: the original absorbing trigger refuses any change
//! from a terminal status, and this regression pins that boundary.
use super::*;

#[test]
fn explicit_cancel_over_interrupted_or_failed_keeps_evidence_across_restart() {
    for failed in [false, true] {
        let (mut repo, turn, _, fence) = prepared();
        let mut effects = vec![text(&turn, "partial")];
        if failed {
            effects.push(event(AiEvent::Failed {
                attempt_id: turn.attempt.id,
                error: invariant_failed(),
                at: at(4),
            }));
        }
        repo.apply_bot_replay(&batch(&turn, &fence, None, effects), at(4))
            .unwrap();
        if !failed {
            repo.recover_interrupted(at(5)).unwrap();
        }
        let before = repo.attempt(turn.attempt.id).unwrap().unwrap();
        assert_eq!(
            before.status,
            if failed {
                AttemptStatus::Failed
            } else {
                AttemptStatus::Interrupted
            }
        );
        let replay = repo.bot_replay_state(turn.attempt.id).unwrap();
        let message = before.assistant_message_id.unwrap();
        let reply = repo.message(message).unwrap().unwrap();
        repo.cancel_bot_chat(turn.attempt.id, at(6)).unwrap();
        repo.recover_interrupted(at(7)).unwrap();
        assert_eq!(repo.attempt(turn.attempt.id).unwrap().unwrap(), before);
        assert_eq!(repo.bot_replay_state(turn.attempt.id).unwrap(), replay);
        assert_eq!(repo.message(message).unwrap().unwrap(), reply);
        assert!(reply.markdown.contains("partial"));
        assert!(!repo.bot_delivery_pending(turn.attempt.id).unwrap());
        assert!(
            repo.connection
                .execute(
                    "UPDATE inference_attempts SET status='cancelled' WHERE id=?1",
                    [turn.attempt.id.to_storage()],
                )
                .is_err(),
            "a durable Cancelled upgrade needs a schema change"
        );
    }
}
