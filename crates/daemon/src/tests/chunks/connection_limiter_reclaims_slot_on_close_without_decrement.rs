// Regression test for the max-connections slot leak: the old limiter stored a
// count in the lock file and relied on a `Drop` guard to decrement it, so a
// `SIGKILL` or `panic = "abort"` skipped the decrement and permanently lost a
// slot. The fcntl model persists no count - a slot is held only by an open file
// descriptor's record lock, which the kernel releases when the descriptor is
// closed or the process dies. This proves reclamation happens with no explicit
// decrement and no on-disk state that a crash could leave stale.
//
// upstream: connection.c:26 `claim_connection()` never writes a count; it relies
// solely on `lock_range()` (util1.c:632, `fcntl` `F_SETLK`/`F_WRLCK`), whose
// locks the kernel drops on process death.
//
// Requires open file description locks so the two acquisitions here (one process)
// contend; only Linux provides them.
#[cfg(target_os = "linux")]
#[test]
fn connection_limiter_reclaims_slot_on_close_without_decrement() {
    let temp = tempdir().expect("lock dir");
    let lock_path = temp.path().join("daemon.lock");
    let limiter = Arc::new(ConnectionLimiter::open(lock_path.clone()).expect("open lock file"));
    let limit = NonZeroU32::new(1).expect("non-zero");

    let held = limiter.acquire("docs", limit).expect("first slot claimed");

    // At capacity: the sole slot's byte range is locked.
    assert!(matches!(
        limiter.acquire("docs", limit),
        Err(ModuleConnectionError::Limit(l)) if l == limit
    ));

    // No count is persisted, so a crash here would leave nothing to inflate the
    // limit on restart.
    assert_eq!(
        fs::metadata(&lock_path).expect("lock file exists").len(),
        0,
        "fcntl limiter must not persist a connection count"
    );

    // Closing the descriptor is exactly what the kernel does on process death.
    // The guard performs no decrement; releasing the lock frees the slot.
    drop(held);

    limiter
        .acquire("docs", limit)
        .expect("slot reclaimed after descriptor closed");

    assert_eq!(
        fs::metadata(&lock_path).expect("lock file exists").len(),
        0,
        "reclaiming a slot must not write any count"
    );
}
