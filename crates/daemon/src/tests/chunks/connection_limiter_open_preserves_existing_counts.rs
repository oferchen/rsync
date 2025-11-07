#[test]
fn connection_limiter_open_preserves_existing_counts() {
    let temp = tempdir().expect("lock dir");
    let lock_path = temp.path().join("daemon.lock");
    fs::write(&lock_path, b"docs 1\nother 2\n").expect("seed lock file");

    let limiter = ConnectionLimiter::open(lock_path.clone()).expect("open lock file");
    drop(limiter);

    let contents = fs::read_to_string(&lock_path).expect("read lock file");
    assert_eq!(contents, "docs 1\nother 2\n");
}

