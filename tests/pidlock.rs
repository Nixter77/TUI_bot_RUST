//! Two --live processes must not share the TestNet book.

use tui_bot::pidlock::acquire_live_lock;

#[test]
fn second_acquire_fails_while_first_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("live.lock");
    let first = acquire_live_lock(Some(&path)).unwrap();
    let second = acquire_live_lock(Some(&path));
    assert!(second.is_err(), "second live lock must refuse: {second:?}");
    let err = second.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    drop(first);
    acquire_live_lock(Some(&path)).expect("lock free after drop");
}

#[test]
fn stale_lock_from_dead_pid_is_stolen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("live.lock");
    std::fs::write(&path, "1\n").unwrap();
    acquire_live_lock(Some(&path)).expect("pid 1 is not alive");
}
