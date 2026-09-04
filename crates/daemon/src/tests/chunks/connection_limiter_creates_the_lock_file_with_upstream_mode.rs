/// WHY (upstream fidelity + security): upstream passes `0600` explicitly to
/// `open_no_attacker_symlinks()` when creating the `lock file`
/// (connection.c:35), rather than letting the open inherit `0666 & ~umask`. A
/// daemon running with a permissive umask would otherwise leave a shared lock
/// file group- or world-readable.
///
/// The assertion is umask-independent by construction: a umask can only CLEAR
/// permission bits, and `0600` has no group or other bits left to clear, so
/// `0600 & !umask == 0600` for every umask that grants the owner read+write.
/// The pre-fix `OpenOptions::create(true)` requested `0666`, which lands on
/// `0644` under the common `022` - so this discriminates rather than restating
/// whatever the environment happens to produce.
#[cfg(unix)]
#[test]
fn connection_limiter_creates_the_lock_file_with_upstream_mode() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempdir().expect("lock dir");
    let lock_path = temp.path().join("daemon.lock");

    let limiter = ConnectionLimiter::open(lock_path.clone());
    drop(limiter);

    let mode = fs::metadata(&lock_path)
        .expect("stat lock file")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "lock file must be created 0600 per connection.c:35, got {mode:o}"
    );
}
