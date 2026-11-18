// SPDX-License-Identifier: MPL-2.0
//! Coalesced post-commit notification. Settings stay authoritative on disk;
//! only an explicit change schedules reads, on the launcher's lifecycle lane.
use super::*;
use std::sync::Mutex;

pub(super) struct Refresh {
    call: Arc<dyn Fn() -> Result<(), Error> + Send + Sync>,
    work: Mutex<Work>,
}
#[derive(Default)]
struct Work {
    generation: u64,
    running: bool,
}

impl Refresh {
    pub(super) fn new(call: Arc<dyn Fn() -> Result<(), Error> + Send + Sync>) -> Arc<Self> {
        Arc::new(Self {
            call,
            work: Mutex::new(Work::default()),
        })
    }
    fn request(self: &Arc<Self>) {
        let Ok(mut work) = self.work.lock() else {
            return;
        };
        work.generation = work.generation.wrapping_add(1);
        if work.running {
            return;
        }
        work.running = true;
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                let Some(owner) = weak.upgrade() else {
                    return;
                };
                let generation = owner.work.lock().expect("capacity work").generation;
                let call = owner.call.clone();
                drop(owner);
                let result = tokio::task::spawn_blocking(move || call()).await;
                let Some(owner) = weak.upgrade() else {
                    return;
                };
                if matches!(result, Ok(Ok(()))) {
                    let mut work = owner.work.lock().expect("capacity work");
                    if work.generation == generation {
                        work.running = false;
                        return;
                    }
                } else {
                    // A failed control call does not undo the settings commit.
                    // Retain one job until the engine returns, or the gateway
                    // drops. A newer change is read by the same job.
                    drop(owner);
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        });
    }
}

impl Gateway {
    pub(crate) fn capacity_changed(&self) {
        self.capacity.request();
    }
}

#[cfg(test)]
mod tests;
