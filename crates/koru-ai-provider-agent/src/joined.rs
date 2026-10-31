// SPDX-License-Identifier: MPL-2.0

//! **One run per key at a time; the rest join it.** A second manual
//! refresh of an agent's models while the first is still rebuilding them
//! waits for the first's answer instead of asking the host again
//! (agent-models D13).

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How long a joiner waits for the run it joined. Past this — a run that
/// never answered — it runs the work itself rather than wait forever.
const JOIN_BUDGET: Duration = Duration::from_secs(90);

pub(crate) struct Joined<K, T> {
    running: Mutex<HashMap<K, Arc<Slot<T>>>>,
}

struct Slot<T> {
    answer: Mutex<Option<T>>,
    ready: Condvar,
}

impl<K: Eq + Hash + Clone, T: Clone> Joined<K, T> {
    pub(crate) fn new() -> Self {
        Self {
            running: Mutex::new(HashMap::new()),
        }
    }

    /// Run `work` for `key`, or wait for the run already under way for it
    /// and answer what that run answers.
    pub(crate) fn run(&self, key: K, work: impl FnOnce() -> T) -> T {
        let (slot, first) = {
            let mut running = lock(&self.running);
            match running.get(&key) {
                Some(slot) => (Arc::clone(slot), false),
                None => {
                    let slot = Arc::new(Slot {
                        answer: Mutex::new(None),
                        ready: Condvar::new(),
                    });
                    running.insert(key.clone(), Arc::clone(&slot));
                    (slot, true)
                }
            }
        };
        if !first {
            let deadline = Instant::now() + JOIN_BUDGET;
            let mut answer = lock(&slot.answer);
            while answer.is_none() {
                let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                    drop(answer);
                    return work();
                };
                answer = slot
                    .ready
                    .wait_timeout(answer, left)
                    .unwrap_or_else(|held| held.into_inner())
                    .0;
            }
            return answer.clone().expect("checked above");
        }
        // The key is released however the work ends, so a run that
        // panicked does not leave every later caller waiting on it.
        let _release = Release {
            running: &self.running,
            key,
        };
        let answer = work();
        *lock(&slot.answer) = Some(answer.clone());
        slot.ready.notify_all();
        answer
    }
}

struct Release<'a, K: Eq + Hash, T> {
    running: &'a Mutex<HashMap<K, Arc<Slot<T>>>>,
    key: K,
}

impl<K: Eq + Hash, T> Drop for Release<'_, K, T> {
    fn drop(&mut self) {
        lock(self.running).remove(&self.key);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|held| held.into_inner())
}

#[cfg(test)]
mod tests;
