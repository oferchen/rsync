#[test]
fn connection_limiter_propagates_io_errors() {
    let temp = tempdir().expect("lock dir");
    let lock_path = temp.path().join("daemon.lock");
    let limiter = Arc::new(ConnectionLimiter::open(lock_path.clone()).expect("open lock"));

    fs::remove_file(&lock_path).expect("remove original lock file");
    fs::create_dir(&lock_path).expect("replace lock file with directory");

    match limiter.acquire("docs", NonZeroU32::new(1).unwrap()) {
        Err(ModuleConnectionError::Io(_)) => {}
        Err(other) => panic!("expected io error, got {other:?}"),
        Ok(_) => panic!("expected io error, got success"),
    }
}

