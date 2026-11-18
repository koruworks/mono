// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::{collections::BTreeMap, sync::atomic::Ordering, thread::JoinHandle};

struct Entry {
    stop: Arc<AtomicBool>,
    commands: mpsc::SyncSender<Control>,
    handle: Option<JoinHandle<Result<(), Failure>>>,
}

/// Owned by one Bot worker. The engine must reserve global capacity before
/// admission. Local pending starts count immediately, before a provider can
/// block in discovery, so heartbeat/idle exit cannot race a slow start.
pub struct Pool {
    worker: Uuid,
    database: Arc<Mutex<BotDatabase>>,
    entries: BTreeMap<Uuid, Entry>,
}

#[derive(Debug)]
pub struct Finished {
    pub thread: Uuid,
    pub result: Result<(), Failure>,
    pub retired: bool,
}

impl Pool {
    pub fn new(worker: Uuid, database: BotDatabase) -> Result<Self, Error> {
        if worker.is_nil() {
            return Err(Error::InvalidRequest);
        }
        Ok(Self {
            worker,
            database: Arc::new(Mutex::new(database)),
            entries: BTreeMap::new(),
        })
    }
    pub fn occupied(&self) -> u32 {
        self.entries.len().min(u32::MAX as usize) as u32
    }
    #[cfg(feature = "provider-runtime")]
    pub(crate) fn journal_database(&self) -> Arc<Mutex<BotDatabase>> {
        Arc::clone(&self.database)
    }
    pub fn admit(&mut self, start: Start) -> Result<(), Error> {
        if start.thread.is_nil() || start.worker != self.worker {
            return Err(Error::InvalidRequest);
        }
        if self.entries.contains_key(&start.thread) {
            return Err(Error::Conflict);
        }
        let thread = start.thread;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let database = self.database.clone();
        let (commands, receive) = mpsc::sync_channel(32);
        let handle = std::thread::Builder::new()
            .name("koru-bot session".into())
            .spawn(move || run::run(start, database, stopped, receive))
            .map_err(|_| Error::EngineUnavailable)?;
        self.entries.insert(
            thread,
            Entry {
                stop,
                commands,
                handle: Some(handle),
            },
        );
        Ok(())
    }
    pub fn stop(&self, thread: Uuid) {
        if let Some(entry) = self.entries.get(&thread) {
            entry.stop.store(true, Ordering::Release);
        }
    }
    pub fn control(&self, thread: Uuid, turn: Turn, action: Action) -> Result<(), Error> {
        if turn.worker != self.worker {
            return Err(Error::StaleTurn);
        }
        self.entries
            .get(&thread)
            .ok_or(Error::StaleTurn)?
            .commands
            .try_send(Control { turn, action })
            .map_err(|_| Error::EngineUnavailable)
    }
    /// Nonblocking on the worker dispatch thread. A panic or failed shutdown
    /// leaves a fenced entry occupying capacity until this Bot worker dies.
    pub fn finished(&mut self) -> Vec<Finished> {
        let ready: Vec<_> = self
            .entries
            .iter()
            .filter_map(|(thread, entry)| {
                entry
                    .handle
                    .as_ref()
                    .is_some_and(JoinHandle::is_finished)
                    .then_some(*thread)
            })
            .collect();
        let mut finished = vec![];
        for thread in ready {
            let handle = self
                .entries
                .get_mut(&thread)
                .unwrap()
                .handle
                .take()
                .unwrap();
            let result = handle.join().unwrap_or(Err(Failure::RetirementUnconfirmed));
            let retired = !matches!(result, Err(Failure::RetirementUnconfirmed));
            if retired {
                self.entries.remove(&thread);
            }
            finished.push(Finished {
                thread,
                result,
                retired,
            });
        }
        finished
    }
}
impl Drop for Pool {
    fn drop(&mut self) {
        for entry in self.entries.values() {
            entry.stop.store(true, Ordering::Release);
        }
    }
}
