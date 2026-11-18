// SPDX-License-Identifier: MPL-2.0
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[tokio::test]
async fn changes_during_a_read_coalesce_and_force_one_fresh_read() {
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let started = entered.clone();
    let (release, wait) = std::sync::mpsc::sync_channel(1);
    let wait = Mutex::new(wait);
    let refresh = Refresh::new(Arc::new(move || {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            started.notify_one();
            wait.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
        }
        Ok(())
    }));
    refresh.request();
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    for _ in 0..100 {
        refresh.request();
    }
    assert_eq!(count.load(Ordering::SeqCst), 1);
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while refresh.work.lock().unwrap().running {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_failed_notification_is_retained_without_another_settings_write() {
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let refresh = Refresh::new(Arc::new(move || {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(Error::EngineUnavailable)
        } else {
            Ok(())
        }
    }));
    refresh.request();
    tokio::time::timeout(Duration::from_secs(4), async {
        while refresh.work.lock().unwrap().running {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 2);
}
