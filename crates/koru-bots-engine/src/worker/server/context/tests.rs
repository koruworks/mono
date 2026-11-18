// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::{capacity::SessionKey, workspace::Page};
use std::{
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

fn assignment() -> (Worker, Reservation, Sources) {
    let worker = Worker {
        bot: Uuid::new_v4(),
        incarnation: Uuid::new_v4(),
    };
    (
        worker,
        Reservation {
            key: SessionKey {
                bot: worker.bot,
                thread: Uuid::new_v4(),
            },
            session: Uuid::new_v4(),
            assignment: Uuid::new_v4(),
        },
        Sources {
            scope: "fixture".into(),
            grant: serde_json::json!({}),
            role: None,
            knowledge: vec![],
            memory: None,
        },
    )
}
fn complete(jobs: &mut Jobs) {
    let result = jobs.receive.recv_timeout(Duration::from_secs(2)).unwrap();
    jobs.send.try_send(result).unwrap();
    jobs.poll_current(|_, _| true);
}

#[test]
fn pending_reads_keep_capacity_after_assignment_ends_and_polls_do_not_duplicate_them() {
    let (started, starts) = mpsc::sync_channel(8);
    let mut jobs = Jobs::with_reader(Arc::new(move |_| {
        let (release, released) = mpsc::sync_channel(1);
        started
            .send(release)
            .map_err(|_| Error::EngineUnavailable)?;
        released.recv().map_err(|_| Error::EngineUnavailable)?;
        Ok(Context {
            role: None,
            knowledge: vec![],
            memory: None,
        })
    }));
    let mut releases = vec![];
    let (worker, reservation, sources) = assignment();
    for index in 0..8 {
        let r = Reservation {
            assignment: Uuid::new_v4(),
            ..reservation
        };
        assert!(matches!(
            jobs.request(worker, r, sources.clone()),
            Reply::ContextPending
        ));
        releases.push(starts.recv_timeout(Duration::from_secs(2)).unwrap());
        assert!(matches!(
            jobs.request(worker, r, sources.clone()),
            Reply::ContextPending
        ));
        assert_eq!(jobs.jobs.len(), index + 1);
    }
    jobs.poll_current(|_, _| false);
    assert_eq!(jobs.jobs.len(), 8);
    assert!(matches!(
        jobs.request(worker, reservation, sources.clone()),
        Reply::ContextPending
    ));
    assert_eq!(jobs.jobs.len(), 8);
    assert!(starts.try_recv().is_err());
    releases.remove(0).send(()).unwrap();
    complete(&mut jobs);
    jobs.poll_current(|_, _| false);
    assert_eq!(jobs.jobs.len(), 7);
    assert!(matches!(
        jobs.request(worker, reservation, sources),
        Reply::ContextPending
    ));
    releases.push(starts.recv_timeout(Duration::from_secs(2)).unwrap());
    for release in releases {
        release.send(()).unwrap();
    }
    while jobs.jobs.values().any(|job| job.result.is_none()) {
        complete(&mut jobs);
    }
    jobs.poll_current(|_, _| false);
    assert!(jobs.jobs.is_empty());
}

#[test]
fn a_changed_configuration_discards_completed_context_and_reads_again() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let seen = Arc::new(Mutex::new(vec![]));
    let observed = seen.clone();
    let mut jobs = Jobs::with_reader(Arc::new(move |sources| {
        count.fetch_add(1, Ordering::Relaxed);
        observed.lock().unwrap().push(sources.role);
        Ok(Context {
            role: sources.role.map(|id| Page {
                id,
                text: "synthetic".into(),
            }),
            knowledge: vec![],
            memory: None,
        })
    }));
    let (worker, reservation, mut sources) = assignment();
    let old = Uuid::new_v4();
    let current = Uuid::new_v4();
    sources.role = Some(old);
    assert!(matches!(
        jobs.request(worker, reservation, sources.clone()),
        Reply::ContextPending
    ));
    complete(&mut jobs);
    sources.role = Some(current);
    assert!(matches!(
        jobs.request(worker, reservation, sources.clone()),
        Reply::ContextPending
    ));
    complete(&mut jobs);
    let Reply::Context(context) = jobs.request(worker, reservation, sources) else {
        panic!()
    };
    assert_eq!(context.role.unwrap().id, current);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(*seen.lock().unwrap(), vec![Some(old), Some(current)]);
}
