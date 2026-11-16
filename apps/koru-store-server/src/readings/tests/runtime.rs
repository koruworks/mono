// SPDX-License-Identifier: MPL-2.0

use super::*;

/// The desktop's replication notifier spawns async work after a reading is
/// committed. Drive the real coordinator thread, not `Device::settle` on the
/// test's runtime thread: a missing runtime used to end reading on its first
/// partial commit and leave every file counted as unread.
#[test]
fn coordinator_thread_commits_with_async_replication_notifications() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let first = hash(101);
    let second = hash(102);
    let scripts = library(&[
        (
            &first,
            Script::Pages(vec![(1, "first fixture")], ReadingStatus::Read, None),
        ),
        (
            &second,
            Script::Pages(vec![(1, "second fixture")], ReadingStatus::Read, None),
        ),
    ]);
    let mut store = store();
    let ws = workspace(&mut store);
    let files = [
        a_file(&mut store, ws, "First.pdf", &first, "application/pdf"),
        a_file(&mut store, ws, "Second.pdf", &second, "application/pdf"),
    ];
    for hash in [&first, &second] {
        land(&mut store, ws, hash, 100, "application/pdf");
    }
    let mut device = Device::new(store, stub(scripts));
    device.here(&first);
    device.here(&second);
    while device.projector.tick(0) {}
    while device.projector.tick(QUIET_MILLIS + 1) {}

    let state = Arc::clone(&device.state);
    let (sent, received) = std::sync::mpsc::channel();
    assert!(state.set_replication_notifier(Arc::new(move || {
        let sent = sent.clone();
        tokio::spawn(async move {
            let _ = sent.send(());
        });
    })));
    let stop = Arc::new(AtomicBool::new(false));
    let inbox = Arc::clone(&device.inbox);
    let board = Arc::new(Board::new(&state));
    let thread = spawn_coordinator(device.coordinator, inbox.clone(), board, stop.clone())
        .expect("coordinator started");
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut finished = false;
    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
        if received.recv_timeout(left).is_err() {
            break;
        }
        let mut store = state.store.lock().unwrap();
        finished = files.iter().zip([&first, &second]).all(|(file, hash)| {
            store
                .fork_document(ws, reading_document_id(*file))
                .ok()
                .and_then(|doc| ReadingDoc::from_doc(doc).reading(hash))
                .is_some_and(|reading| reading.status == Some(ReadingStatus::Read))
        });
        if finished {
            break;
        }
    }
    stop.store(true, Ordering::Release);
    inbox.stop();
    let joined = thread.join();
    assert!(joined.is_ok(), "the reading coordinator panicked");
    assert!(finished, "both files must finish and notify replication");
}
