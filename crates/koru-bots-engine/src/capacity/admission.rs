// SPDX-License-Identifier: MPL-2.0

use super::{IDLE_WINDOW_MS, SessionKey};
use crate::{Database, Error};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::collections::BTreeMap;
use uuid::Uuid;

/// One engine-owned record of every reserved or live provider session. Workers
/// report lifecycle edges against its incarnation; they do not choose capacity.
/// Queued work stays in engine.db and is never copied into a second queue.
pub struct Admission {
    limit: u32,
    sessions: BTreeMap<SessionKey, Slot>,
}

#[cfg(test)]
mod tests;

struct Slot {
    /// Capacity identity survives a failed-start credential replacement.
    session: Uuid,
    provider: Option<Uuid>,
    assignment: Uuid,
    worker: Option<Uuid>,
    phase: Phase,
}

#[derive(Clone, Copy)]
enum Phase {
    Starting,
    Running,
    Idle {
        since: u64,
        resumable: bool,
        saved: bool,
    },
    Stopping,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Reservation {
    pub key: SessionKey,
    pub session: Uuid,
    pub assignment: Uuid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Start {
        reservation: Reservation,
        reuse: bool,
    },
    End(Reservation),
}

impl Admission {
    pub fn new(limit: u32) -> Self {
        Self {
            limit: limit.max(1),
            sessions: BTreeMap::new(),
        }
    }

    pub fn set_capacity(&mut self, limit: u32) {
        self.limit = limit.max(1);
    }

    pub fn occupied(&self) -> usize {
        self.sessions.len()
    }

    pub fn provider(&self, reservation: Reservation, worker: Uuid) -> Result<Option<Uuid>, Error> {
        self.sessions
            .get(&reservation.key)
            .filter(|slot| {
                slot.session == reservation.session
                    && slot.assignment == reservation.assignment
                    && slot.worker == Some(worker)
            })
            .map(|slot| slot.provider)
            .ok_or(Error::StaleTurn)
    }

    /// Minting is serialized by the engine. A failed native start may replace
    /// its credential while retaining the same already-counted capacity slot.
    pub fn bind_provider(
        &mut self,
        reservation: Reservation,
        worker: Uuid,
        provider: Uuid,
    ) -> Result<(), Error> {
        if provider.is_nil() {
            return Err(Error::InvalidRequest);
        }
        let slot = self.matching(reservation, worker)?;
        if !matches!(slot.phase, Phase::Starting | Phase::Running) {
            return Err(Error::StaleTurn);
        }
        slot.provider = Some(provider);
        Ok(())
    }

    pub fn retiring(&self, bot: Uuid, worker: Uuid) -> Vec<Reservation> {
        self.sessions
            .iter()
            .filter_map(|(key, slot)| {
                (key.bot == bot
                    && slot.worker == Some(worker)
                    && matches!(slot.phase, Phase::Stopping))
                .then_some(Reservation {
                    key: *key,
                    session: slot.session,
                    assignment: slot.assignment,
                })
            })
            .collect()
    }

    pub fn occupied_by(&self, bot: Uuid, worker: Uuid) -> u32 {
        self.sessions
            .iter()
            .filter(|(key, slot)| key.bot == bot && slot.worker == Some(worker))
            .count()
            .min(u32::MAX as usize) as u32
    }

    /// The launcher allocated this incarnation before allowing registration.
    /// Replacement cannot inherit the old worker's live session reservations.
    pub fn bind_worker(&mut self, bot: Uuid, worker: Uuid) {
        for (key, slot) in &mut self.sessions {
            if key.bot == bot && slot.worker.is_none() {
                slot.worker = Some(worker);
            }
        }
    }

    pub fn assigned(&self, bot: Uuid, worker: Uuid) -> Vec<Reservation> {
        self.sessions
            .iter()
            .filter_map(|(key, slot)| {
                (key.bot == bot
                    && slot.worker == Some(worker)
                    && matches!(slot.phase, Phase::Starting))
                .then_some(Reservation {
                    key: *key,
                    session: slot.session,
                    assignment: slot.assignment,
                })
            })
            .collect()
    }

    pub fn owns(&self, reservation: Reservation, worker: Uuid) -> bool {
        self.sessions.get(&reservation.key).is_some_and(|slot| {
            slot.session == reservation.session
                && slot.assignment == reservation.assignment
                && slot.worker == Some(worker)
                && !matches!(slot.phase, Phase::Stopping)
        })
    }

    pub fn started(&mut self, reservation: Reservation, worker: Uuid) -> Result<(), Error> {
        let slot = self.matching(reservation, worker)?;
        if !matches!(slot.phase, Phase::Starting | Phase::Running) {
            return Err(Error::StaleTurn);
        }
        slot.phase = Phase::Running;
        Ok(())
    }

    /// The carrier first verifies that the turn finished and every reported
    /// call is bound. Blockers are deliberately not captured in this record.
    pub fn idle(
        &mut self,
        reservation: Reservation,
        worker: Uuid,
        since: u64,
        resumable: bool,
        saved: bool,
    ) -> Result<(), Error> {
        let slot = self.matching(reservation, worker)?;
        if !matches!(slot.phase, Phase::Running) {
            return Err(Error::StaleTurn);
        }
        slot.phase = Phase::Idle {
            since,
            resumable,
            saved,
        };
        Ok(())
    }

    pub fn stopping(&mut self, reservation: Reservation, worker: Uuid) -> Result<(), Error> {
        self.matching(reservation, worker)?.phase = Phase::Stopping;
        Ok(())
    }

    /// Only confirmed provider retirement reaches this edge. A stale worker
    /// cannot release the reservation of a replacement process.
    pub fn retired(&mut self, reservation: Reservation, worker: Uuid) -> Result<(), Error> {
        self.matching(reservation, worker)?;
        self.sessions.remove(&reservation.key);
        Ok(())
    }

    pub fn worker_ended(&mut self, worker: Uuid) {
        self.sessions.retain(|_, slot| slot.worker != Some(worker));
    }

    /// Launcher command was not enqueued; no worker ever owned this slot.
    pub(crate) fn withdraw(&mut self, reservation: Reservation) -> Result<(), Error> {
        let slot = self
            .sessions
            .get(&reservation.key)
            .ok_or(Error::StaleTurn)?;
        if slot.session != reservation.session
            || slot.assignment != reservation.assignment
            || slot.worker.is_some()
            || !matches!(slot.phase, Phase::Starting)
        {
            return Err(Error::StaleTurn);
        }
        self.sessions.remove(&reservation.key);
        Ok(())
    }

    fn matching(&mut self, reservation: Reservation, worker: Uuid) -> Result<&mut Slot, Error> {
        self.sessions
            .get_mut(&reservation.key)
            .filter(|slot| {
                slot.session == reservation.session
                    && slot.assignment == reservation.assignment
                    && slot.worker == Some(worker)
            })
            .ok_or(Error::StaleTurn)
    }

    pub fn next(&mut self, db: &Database, now: u64) -> Result<Option<Decision>, Error> {
        if let Some(mut key) = self.candidate(db, now, false)? {
            if self.sessions.len() >= self.limit as usize && !self.sessions.contains_key(&key) {
                // A blocked oldest request must not hide a lower-ranked request
                // that can reuse its own slot without exceeding capacity.
                if let Some(reusable) = self.candidate(db, now, true)? {
                    key = reusable;
                } else {
                    return self.end_idle(db, now, true);
                }
            }
            let reuse = self.sessions.contains_key(&key);
            let slot = self.sessions.entry(key).or_insert(Slot {
                session: Uuid::new_v4(),
                provider: None,
                assignment: Uuid::new_v4(),
                worker: None,
                phase: Phase::Starting,
            });
            slot.assignment = Uuid::new_v4();
            slot.phase = Phase::Starting;
            return Ok(Some(Decision::Start {
                reservation: Reservation {
                    key,
                    session: slot.session,
                    assignment: slot.assignment,
                },
                reuse,
            }));
        }
        self.end_idle(db, now, false)
    }

    fn candidate(
        &self,
        db: &Database,
        now: u64,
        reuse_only: bool,
    ) -> Result<Option<SessionKey>, Error> {
        #[derive(Serialize)]
        struct Live {
            bot: Uuid,
            thread: Uuid,
            session: Option<Uuid>,
            idle: bool,
            active: bool,
        }
        let live: Vec<_> = self
            .sessions
            .iter()
            .map(|(key, slot)| Live {
                bot: key.bot,
                thread: key.thread,
                session: slot.provider,
                idle: matches!(slot.phase, Phase::Idle { .. }),
                active: matches!(slot.phase, Phase::Starting | Phase::Running),
            })
            .collect();
        let live = serde_json::to_string(&live).map_err(|_| Error::InvalidRequest)?;
        let row: Option<(String,String)> = db.connection.query_row(
            "WITH live AS (SELECT json_extract(value,'$.bot') bot,json_extract(value,'$.thread') thread,
               json_extract(value,'$.session') session,json_extract(value,'$.idle') idle,json_extract(value,'$.active') active FROM json_each(?1))
             SELECT i.bot,i.thread FROM inbox i JOIN obligations o ON o.id=i.id JOIN messages m ON m.id=i.message
             JOIN bots b ON b.id=i.bot JOIN threads t ON t.id=i.thread LEFT JOIN team tm ON tm.owner=b.owner
             LEFT JOIN live l ON l.bot=i.bot AND l.thread=i.thread
             WHERE o.state IN ('owed','delivered') AND b.paused=0 AND b.lifecycle='active' AND t.lifecycle='active' AND COALESCE(tm.paused,0)=0
               AND EXISTS(SELECT 1 FROM participants p WHERE p.thread=i.thread AND p.participant=i.bot AND p.kind='bot')
               AND (o.state='delivered' OR json_extract(CAST(m.payload AS TEXT),'$.deadline_ms') IS NULL OR json_extract(CAST(m.payload AS TEXT),'$.deadline_ms')>=?2)
               AND (l.bot IS NULL OR l.idle=1) AND (?3=0 OR l.idle=1)
               AND NOT EXISTS(SELECT 1 FROM turns r WHERE r.bot=i.bot AND r.thread=i.thread AND r.state IN ('running','needs_you','parked'))
               AND NOT EXISTS(SELECT 1 FROM operations w WHERE w.bot=i.bot AND w.thread=i.thread AND w.state IN ('leased','unresolved'))
               AND NOT EXISTS(SELECT 1 FROM deliveries d WHERE d.session=l.session AND d.state IN ('offered','unknown'))
             GROUP BY i.bot,i.thread,t.kind
             ORDER BY t.kind='routine',EXISTS(SELECT 1 FROM live a WHERE a.bot=i.bot AND a.active=1),MIN(i.accepted_at_ms),i.bot,i.thread LIMIT 1",
            params![live, now.min(i64::MAX as u64) as i64, reuse_only], |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()?;
        row.map(|(bot, thread)| {
            Ok(SessionKey {
                bot: Uuid::parse_str(&bot).map_err(|_| Error::StorageUnavailable)?,
                thread: Uuid::parse_str(&thread).map_err(|_| Error::StorageUnavailable)?,
            })
        })
        .transpose()
    }

    fn end_idle(
        &mut self,
        db: &Database,
        now: u64,
        pressured: bool,
    ) -> Result<Option<Decision>, Error> {
        let mut candidates: Vec<_> = self
            .sessions
            .iter()
            .filter_map(|(key, slot)| {
                if let Phase::Idle {
                    since,
                    resumable,
                    saved,
                } = slot.phase
                {
                    Some((*key, slot.provider, since, resumable, saved))
                } else {
                    None
                }
            })
            .collect();
        candidates.sort_by_key(|(key, _, since, _, _)| (*since, *key));
        for (key, provider, since, resumable, saved) in candidates {
            let (blocked, side): (bool, bool) = db.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM deliveries WHERE session=?1 AND state IN ('offered','unknown'))
                   OR EXISTS(SELECT 1 FROM operations WHERE bot=?2 AND thread=?3 AND state IN ('leased','unresolved')),
                   EXISTS(SELECT 1 FROM threads WHERE id=?3 AND kind IN ('routine','between_bots'))",
                params![provider.map(|id| id.to_string()),key.bot.to_string(),key.thread.to_string()], |r| Ok((r.get(0)?,r.get(1)?)),
            )?;
            if !blocked
                && ((resumable && saved && (pressured || side))
                    || now.saturating_sub(since) >= IDLE_WINDOW_MS)
            {
                let slot = self.sessions.get_mut(&key).unwrap();
                slot.phase = Phase::Stopping;
                return Ok(Some(Decision::End(Reservation {
                    key,
                    session: slot.session,
                    assignment: slot.assignment,
                })));
            }
        }
        Ok(None)
    }
}
