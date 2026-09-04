/// Constructing a limiter over an unopenable `lock file` cannot fail.
///
/// upstream has no build-time open at all: the lock file is opened inside
/// `claim_connection()` (connection.c:33) and a failed open there is
/// `return 0` - that one connection is refused while the daemon keeps
/// listening. Making construction fallible turned an operator-named path the
/// daemon could not open into a daemon that never reached `listen()`.
///
/// The eager create is kept because it establishes the 0600 mode, but its
/// failure is deliberately ignored; [`ConnectionLimiter::acquire`] re-opens per
/// connection and reports the real error there, which is upstream's site.
#[test]
fn connection_limiter_open_survives_an_unopenable_lock_path() {
    let temp = tempdir().expect("lock dir");
    let lock_path = temp.path().join("daemon.lock");
    // Already a directory, so every open of it fails - the create in `open`
    // included.
    fs::create_dir(&lock_path).expect("lock path as a directory");

    let limiter = Arc::new(ConnectionLimiter::open(lock_path));

    // The failure surfaces per connection, exactly where upstream decides it.
    match limiter.acquire(
        "docs",
        MaxConnections::Limited(NonZeroU32::new(1).expect("non-zero")),
    ) {
        Err(ModuleConnectionError::Io(_)) => {}
        Err(other) => panic!("expected io error, got {other:?}"),
        Ok(_) => panic!("expected io error, got success"),
    }
}
