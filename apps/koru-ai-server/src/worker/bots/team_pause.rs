// SPDX-License-Identifier: MPL-2.0

//! Team pause in the AI worker (M8, P8.1): the three public commands, and
//! telling the delegate.
//!
//! The durable truth is the owner's row in ai.db. The AI side refuses new
//! work from the commit of a row that says paused. The delegate holds a copy
//! in memory for one run and is closed until it is told, so this worker
//! sends the value:
//!
//! - after each `PauseTeam` or `ResumeTeam` that wrote the row;
//! - each time the boot `Hello` shows a delegate run this worker has not
//!   told (a worker start, a worker restart, a delegate restart), before any
//!   Bot of that run is synchronized. A team that was never paused is sent
//!   as "never paused", so the delegate opens.
//!
//! A send that is not answered with the same value is owed and is sent
//! again, 250 ms doubling to 32 s, without end. Silence is never read as
//! done: the row stays `pending` until a delegate run echoes it.
//!
//! The send rides the boot lane's job (`boot.rs`), so it needs no lane of
//! its own; its bookkeeping is here and is independent of the boot walk.

use super::*;
use koru_delegate_api::bots::{BotReply, team_held_epoch};
use koru_delegate_core::{BotTeamBarrier, DelegateBootId};

type DelegateOwner = koru_delegate_core::BotOwner;

/// The delegate's refusal of a value at an epoch it already holds with
/// other content.
const TEAM_CONFLICT: &str = "bot_team_conflict";
/// What this worker records when the delegate answered something that is
/// not the value it was sent. Never a delegate code.
const TEAM_UNEXPECTED: &str = "bot_team_unexpected";
/// What this worker records when its own row could not be read or written.
const TEAM_STORAGE: &str = "bot_team_storage";

/// What this worker owes the delegate about the team, per delegate run.
/// Memory only: a restart starts over and sends again.
#[derive(Default)]
pub(in crate::worker) struct TeamCoordination {
    /// The delegate run the boot `Hello` last showed.
    run: Option<(DelegateOwner, DelegateBootId)>,
    /// A send to `run` came back, whatever it answered. The per-Bot boot
    /// pass of that run waits for this.
    attempted: bool,
    /// `run` echoed a value: it is told.
    told: bool,
    /// Something must be sent: `run` was not told the current row.
    owed: bool,
    busy: Option<u64>,
    serial: u64,
    retry_at: Option<Instant>,
    round: u32,
    /// The code of the last send that was not an equal echo, since this
    /// worker started.
    last_answer: Option<String>,
}

impl TeamCoordination {
    /// The boot `Hello` showed a delegate run this worker had not observed:
    /// it holds nothing of the team, so the value is owed to it.
    pub(super) fn observe(&mut self, owner: DelegateOwner, boot: DelegateBootId) {
        self.run = Some((owner, boot));
        self.attempted = false;
        self.told = false;
        self.owed = true;
        self.retry_at = None;
        self.round = 0;
    }

    /// Whether the first send to this delegate run came back. The per-Bot
    /// boot pass of a run starts only after it.
    pub(super) fn sent_to(&self, run: Option<(DelegateOwner, DelegateBootId)>) -> bool {
        run.is_some() && self.run == run && self.attempted
    }

    fn failed(&mut self, now: Instant, code: &str) {
        self.owed = true;
        self.last_answer = Some(code.to_owned());
        self.retry_at = Some(now + Duration::from_millis(250) * (1u32 << self.round.min(7)));
        self.round = (self.round + 1).min(7);
    }

    #[cfg(test)]
    pub(in crate::worker) fn told(&self) -> bool {
        self.told
    }
    #[cfg(test)]
    pub(in crate::worker) fn owed(&self) -> bool {
        self.owed
    }
    #[cfg(test)]
    pub(in crate::worker) fn last_answer(&self) -> Option<&str> {
        self.last_answer.as_deref()
    }
}

fn ai_owner(owner: DelegateOwner) -> BotOwner {
    BotOwner {
        user_scope: owner.user_scope,
        device: owner.device,
    }
}

/// What a row the worker rewrote itself (after `bot_team_stale` or
/// `bot_team_conflict`) asked for: 64 hex digits, as every request's.
fn rewrite_fingerprint(held_epoch: u64) -> String {
    blake3::hash(format!("koru.bot.team-pause.rewrite.v1:{held_epoch}").as_bytes())
        .to_hex()
        .to_string()
}

impl WorkerState {
    /// `PauseTeam`, `ResumeTeam` and `ReadTeamPause`. `None` for any other
    /// command.
    pub(super) fn bot_team_command(
        &mut self,
        context: &CommandContext,
        command: &BotCommand,
    ) -> Option<ApiResult<BotResponse>> {
        if !matches!(
            command,
            BotCommand::PauseTeam { .. }
                | BotCommand::ResumeTeam { .. }
                | BotCommand::ReadTeamPause {}
        ) {
            return None;
        }
        Some((|| {
            authorize(context, AiScope::ManageBots)?;
            let proof = context
                .prepared
                .as_ref()
                .ok_or_else(|| bot_failure(BotError::Unavailable))?;
            // The owner is the vouched one. The commands carry none.
            let owner = proof.authority.owner().map_err(bot_failure)?;
            let (request, expected, paused) = match command {
                BotCommand::PauseTeam {
                    request_id,
                    expected_team_revision,
                } => (*request_id, *expected_team_revision, true),
                BotCommand::ResumeTeam {
                    request_id,
                    expected_team_revision,
                } => (*request_id, *expected_team_revision, false),
                _ => {
                    return Ok(BotResponse::TeamPause {
                        team: self
                            .repository
                            .team_pause_view(owner)
                            .map_err(bot_failure)?,
                    });
                }
            };
            let fingerprint = blake3::hash(
                &serde_json::to_vec(command).map_err(|_| bot_failure(BotError::Invalid))?,
            )
            .to_hex()
            .to_string();
            let change = self
                .repository
                .change_team_pause(
                    owner,
                    request,
                    &fingerprint,
                    expected,
                    paused,
                    Timestamp::now().millis(),
                )
                .map_err(bot_failure)?;
            let team = change.view(owner);
            if change.changed().is_some() {
                // The row is committed: the AI side refuses from here. The
                // delegate is told next, whatever its last answer was.
                self.bot_boot.team.owed = true;
                self.bot_boot.team.retry_at = None;
                self.bot_boot.team.round = 0;
                self.schedule_team_barrier();
            } else if team.stage == TeamPauseStage::Pending {
                // A repeat while the change is still being delivered keeps it
                // owed; it does not bring the next send forward.
                self.bot_boot.team.owed = true;
            }
            Ok(BotResponse::TeamPause { team })
        })())
    }

    /// Whether this owner's team is paused, from the durable row. Rows 10
    /// and 13 of the contract's admission inventory read it before anything
    /// is sent to the delegate.
    pub(super) fn team_paused(&self, owner: BotOwner) -> Result<bool, BotError> {
        Ok(self
            .repository
            .team_pause(owner)?
            .is_some_and(|row| row.paused))
    }

    /// Send the team value when it is owed. Called on every pass of the
    /// writer's loop and after a team command; it does nothing when nothing
    /// is owed.
    pub(super) fn schedule_team_barrier(&mut self) {
        let team = &self.bot_boot.team;
        let Some((owner, boot)) = team.run else {
            return;
        };
        let now = Instant::now();
        if !team.owed || team.busy.is_some() || team.retry_at.is_some_and(|at| at > now) {
            return;
        }
        let barrier = match self.repository.team_pause_barrier(ai_owner(owner)) {
            Ok(barrier) => barrier,
            Err(_) => {
                self.bot_boot.team.failed(now, TEAM_STORAGE);
                return;
            }
        };
        let serial = self
            .bot_boot
            .team
            .serial
            .checked_add(1)
            .expect("team barrier sequence exhausted");
        match self.submit_team_barrier(serial, boot, barrier) {
            Some(true) => {
                self.bot_boot.team.serial = serial;
                self.bot_boot.team.busy = Some(serial);
            }
            // The lane is full: offered again on a later pass, soon.
            Some(false) => {
                self.bot_boot.team.retry_at = Some(now + Duration::from_millis(250));
            }
            // No delegate in this composition: nothing can be told.
            None => {}
        }
    }

    /// The delegate's answer to one send.
    pub(super) fn complete_team_barrier(
        &mut self,
        serial: u64,
        boot: DelegateBootId,
        sent: BotTeamBarrier,
        result: Result<BotReply, koru_delegate_api::Failure>,
    ) {
        if self.bot_boot.team.busy != Some(serial) {
            return;
        }
        self.bot_boot.team.busy = None;
        let now = Instant::now();
        let now_ms = Timestamp::now().millis();
        let current = self.bot_boot.team.run == Some((sent.owner, boot));
        if current {
            self.bot_boot.team.attempted = true;
        }
        let owner = ai_owner(sent.owner);
        match result {
            // Only the value that was sent, echoed whole, counts.
            Ok(BotReply::TeamBarrier { barrier: echo }) if echo == sent => {
                // The repository compares the echo with the row again, in its
                // transaction: an echo of an older value acknowledges nothing.
                let acknowledged = self.repository.acknowledge_team_pause(owner, &echo, now_ms);
                let latest = self.repository.team_pause_barrier(owner);
                match (acknowledged, latest) {
                    (Ok(_), Ok(latest)) => {
                        let team = &mut self.bot_boot.team;
                        team.round = 0;
                        team.retry_at = None;
                        team.last_answer = None;
                        if current {
                            team.told = true;
                        }
                        if latest != echo {
                            // The row moved while this was in flight.
                            team.owed = true;
                        } else if current {
                            team.owed = false;
                        }
                    }
                    _ => self.bot_boot.team.failed(now, TEAM_STORAGE),
                }
            }
            Ok(_) => self.bot_boot.team.failed(now, TEAM_UNEXPECTED),
            Err(failure) => {
                // The delegate holds a newer epoch (this ai.db was restored
                // to an older one while the delegate kept running), or other
                // content at this epoch. The value as sent will never be
                // taken. The row is written past what the delegate holds,
                // keeping its own `paused`, and that is sent on the next,
                // backed-off pass: the team cannot stay held behind a row
                // that says it is not paused, or the reverse.
                let held = team_held_epoch(&failure)
                    .or_else(|| (failure.code == TEAM_CONFLICT).then_some(sent.epoch));
                // Only while the refused value is still what the row says: a
                // row that moved on meanwhile is simply sent next.
                let still_current = matches!(self.repository.team_pause_barrier(owner), Ok(latest) if latest == sent);
                if let Some(held) = held.filter(|_| still_current) {
                    let _ = self.repository.advance_team_pause_past(
                        owner,
                        held,
                        BotRequestId::mint(),
                        &rewrite_fingerprint(held),
                        now_ms,
                    );
                }
                self.bot_boot.team.failed(now, &failure.code);
            }
        }
    }
}

#[cfg(test)]
mod tests;
