#[test]
fn connection_limiter_propagates_io_errors() {
    let temp = tempdir().expect("lock dir");
    let lock_path = temp.path().join("daemon.lock");
    let limiter = Arc::new(ConnectionLimiter::open(lock_path.clone()));

    fs::create_dir(&lock_path).expect("replace lock file with directory");

    match limiter.acquire(
        "docs",
        MaxConnections::Limited(NonZeroU32::new(1).expect("non-zero")),
    ) {
        Err(ModuleConnectionError::Io(_)) => {}
        Err(other) => panic!("expected io error, got {other:?}"),
        Ok(_) => panic!("expected io error, got success"),
    }
}

/// The lock file is resolved when the claim opens it: a `/cygdrive` value that
/// names no drive fails that connection with not-found, as Cygwin's open
/// fails with `ENOENT`, while building the limiter touches nothing.
#[cfg(windows)]
#[test]
fn connection_limiter_resolves_the_lock_file_at_the_claim() {
    let limiter = Arc::new(ConnectionLimiter::open(PathBuf::from(
        "/cygdrive/cd/rsyncd.lock",
    )));

    match limiter.acquire(
        "docs",
        MaxConnections::Limited(NonZeroU32::new(1).expect("non-zero")),
    ) {
        Err(ModuleConnectionError::Io(error)) => {
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "{error}");
        }
        Err(other) => panic!("expected io error, got {other:?}"),
        Ok(_) => panic!("expected io error, got success"),
    }
}
