// macOS lacks open file description locks, so two `acquire` calls from this one
// process both resolve to the same process-owned POSIX lock and cannot contend
// on distinct slots. There, in-process limiting is the caller's atomic counter's
// job (see `runtime.rs`); the record lock only coordinates across processes.
// Linux uses `F_OFD_SETLK` and Windows uses the counter fallback, so both
// observe the drop-and-reclaim behaviour asserted here.
#[cfg(not(target_os = "macos"))]
#[test]
fn connection_limiter_enforces_limits_across_guards() {
    let temp = tempdir().expect("lock dir");
    let lock_path = temp.path().join("daemon.lock");
    let limiter = Arc::new(ConnectionLimiter::open(lock_path).expect("open lock file"));
    let limit = NonZeroU32::new(2).expect("non-zero");

    let first = limiter
        .acquire("docs", limit)
        .expect("first connection allowed");
    let second = limiter
        .acquire("docs", limit)
        .expect("second connection allowed");
    assert!(matches!(
        limiter.acquire("docs", limit),
        Err(ModuleConnectionError::Limit(l)) if l == limit
    ));

    drop(second);
    let third = limiter
        .acquire("docs", limit)
        .expect("slot released after guard drop");

    drop(third);
    drop(first);
    assert!(limiter.acquire("docs", limit).is_ok());
}

