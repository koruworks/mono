// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn known_saving_diagnostics_are_translated_without_rewriting_transport_details() {
    use koru_i18n::{Key, Lang, t};
    for lang in Lang::ALL {
        assert_eq!(
            saving_reason(koru_workspace::Unconfirmed::NOT_BUFFERED, lang),
            t(lang, Key::DocumentNotBuffered)
        );
        assert_eq!(
            saving_reason(koru_workspace::Unconfirmed::SAVING_PENDING, lang),
            t(lang, Key::DocumentSavingPending)
        );
        assert_eq!(
            saving_reason("transport details", lang),
            "transport details"
        );
    }
}

struct DropProbe(mpsc::Sender<std::thread::ThreadId>);
impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.send(std::thread::current().id()).unwrap();
    }
}

#[test]
fn canceled_or_refused_prepared_profile_disposes_on_its_worker() {
    for cancel_before_ready in [false, true] {
        let state = Arc::new(Mutex::new(None));
        let output = Arc::clone(&state);
        let (release, released) = mpsc::channel();
        let (ready, prepared) = mpsc::channel();
        let (dropped, disposal) = mpsc::channel();
        let (start, started) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            started.recv().unwrap();
            publish_prepared(DropProbe(dropped), output, ready, released, || {});
        });
        let worker_id = worker.thread().id();
        if cancel_before_ready {
            drop(release);
            start.send(()).unwrap();
        } else {
            start.send(()).unwrap();
            prepared
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            // Final outgoing-save refusal leaves the result worker-owned.
            assert!(state.lock().unwrap().is_some());
            drop(release);
        }
        assert_eq!(
            disposal
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            worker_id
        );
        worker.join().unwrap();
        assert!(state.lock().unwrap().is_none());
    }
}
