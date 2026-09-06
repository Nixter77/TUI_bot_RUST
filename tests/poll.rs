//! Snapshot poller: latest-wins slot, keys must not wait on the pull.

use rust_decimal::Decimal;
use std::sync::mpsc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tui_bot::models::MarketSnapshot;
use tui_bot::poll::{Pulled, SnapshotPoller};

#[test]
fn take_is_empty_until_pull_finishes() {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let poller = SnapshotPoller::start(Duration::from_secs(30), move || {
        let _ = started_tx.send(());
        release_rx.recv().expect("release");
        Pulled {
            snapshot: MarketSnapshot::empty(Decimal::ZERO),
            tradfi: vec!["SKIPUSDT".into()],
        }
    })
    .unwrap();
    assert!(poller.take().is_none());
    poller.bump();
    started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("pull started");
    assert!(poller.take().is_none(), "in-flight pull must not publish");
    release_tx.send(()).unwrap();
    let mut got = None;
    for _ in 0..50 {
        if let Some(p) = poller.take() {
            got = Some(p);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let got = got.expect("pulled snapshot");
    assert_eq!(got.tradfi, vec!["SKIPUSDT".to_string()]);
    assert!(poller.take().is_none());
}

#[test]
fn later_pull_overwrites_unread_snapshot() {
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    let poller = SnapshotPoller::start(Duration::from_secs(30), move || {
        let i = n2.fetch_add(1, Ordering::SeqCst) + 1;
        Pulled {
            snapshot: MarketSnapshot::empty(Decimal::from(i as i64)),
            tradfi: vec![format!("{i}")],
        }
    })
    .unwrap();
    poller.bump();
    std::thread::sleep(Duration::from_millis(80));
    poller.bump();
    std::thread::sleep(Duration::from_millis(80));
    let got = poller.take().expect("latest");
    assert_eq!(got.tradfi, vec!["2".to_string()]);
}

#[test]
fn panicking_pull_does_not_kill_poller() {
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    let poller = SnapshotPoller::start(Duration::from_secs(30), move || {
        let i = n2.fetch_add(1, Ordering::SeqCst) + 1;
        if i == 1 {
            panic!("boom");
        }
        Pulled {
            snapshot: MarketSnapshot::empty(Decimal::ZERO),
            tradfi: vec!["ok".into()],
        }
    })
    .unwrap();
    poller.bump();
    std::thread::sleep(Duration::from_millis(80));
    assert!(poller.take().is_none(), "panic must not publish");
    poller.bump();
    let mut got = None;
    for _ in 0..50 {
        if let Some(p) = poller.take() {
            got = Some(p);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(got.expect("recovered").tradfi, vec!["ok".to_string()]);
}

#[test]
fn panicking_pull_is_counted_for_the_tui() {
    let poller: SnapshotPoller<MarketSnapshot> = SnapshotPoller::start(Duration::from_secs(30), || -> Pulled<MarketSnapshot> {
        panic!("boom");
    })
    .unwrap();
    poller.bump();
    let mut n = 0;
    for _ in 0..50 {
        n = poller.take_panics();
        if n > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(n >= 1, "TUI must see the panic, got {n}");
    assert_eq!(poller.take_panics(), 0, "count is consumed");
}
