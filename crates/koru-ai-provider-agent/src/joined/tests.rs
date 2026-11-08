// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

/// A second refresh of the same agent while the first is running sends
/// nothing of its own: it answers what the first answers.
#[test]
fn a_second_run_for_the_same_key_joins_the_first() {
    let joined = Arc::new(Joined::<&'static str, u32>::new());
    let runs = Arc::new(AtomicUsize::new(0));
    let (started, first_started) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let first = {
        let joined = joined.clone();
        let runs = runs.clone();
        std::thread::spawn(move || {
            joined.run("opencode", || {
                runs.fetch_add(1, Ordering::SeqCst);
                started.send(()).unwrap();
                released.recv().unwrap();
                7
            })
        })
    };
    first_started.recv().unwrap();
    let second = {
        let joined = joined.clone();
        let runs = runs.clone();
        std::thread::spawn(move || {
            joined.run("opencode", || {
                runs.fetch_add(1, Ordering::SeqCst);
                8
            })
        })
    };
    // Another key is not held up by this one.
    assert_eq!(joined.run("claude", || 1), 1);
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !second.is_finished(),
        "the joiner waits for the first's answer"
    );
    release.send(()).unwrap();
    assert_eq!(first.join().unwrap(), 7);
    assert_eq!(second.join().unwrap(), 7);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    // Once answered, the next run is a run of its own.
    assert_eq!(joined.run("opencode", || 9), 9);
}
