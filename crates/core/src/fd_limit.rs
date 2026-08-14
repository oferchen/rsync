//! Process file-descriptor limit management.
//!
//! upstream: `main.c:1793-1808` `raise_fd_limit()`, invoked as the first
//! statement of `main()` (`main.c:1817`).

/// Soft `RLIMIT_NOFILE` this process aims for.
///
/// upstream: `main.c:1795` - "covers a MAXPATHLEN-deep walk + cache +
/// headroom". Deliberately not unbounded: some systems set an enormous
/// hard limit (2^20 and up) that upstream declines to adopt wholesale.
pub const FD_LIMIT_TARGET: u64 = 4096;

/// Raise the soft `RLIMIT_NOFILE` toward [`FD_LIMIT_TARGET`].
///
/// The symlink-race-safe descent in `fast_io::dir_sandbox` holds one open
/// dirfd per path component plus an ancestor cache, so it needs far more
/// descriptors than a single path-based `open()`. On a host with a low
/// default soft limit - upstream names OpenBSD's 128 - a deep tree hits
/// `EMFILE` where legacy rsync would not.
///
/// Three properties, all upstream's:
///
/// - **Capped** at [`FD_LIMIT_TARGET`], not raised to the hard limit.
/// - **Clamped** to the hard limit, which an unprivileged process cannot
///   exceed. Asking for more makes `setrlimit` fail outright, which would
///   leave the soft limit untouched - so the clamp is what keeps the
///   mitigation working on a host with a low admin-set ceiling, not a
///   politeness.
/// - **Raise-only.** An operator or parent process that deliberately set a
///   higher limit keeps it; one that set a *lower* one is overridden only
///   up to the cap, never reduced.
///
/// Best-effort throughout: every failure path leaves the process running
/// with whatever limit it already had. `main()` calls this before any
/// thread or child process is spawned, so the raised limit is inherited by
/// the sender, generator, receiver and daemon children.
///
/// upstream: `main.c:1793-1808`.
#[cfg(unix)]
pub fn raise_fd_limit() {
    use rustix::process::{Resource, getrlimit, setrlimit};

    let mut limit = getrlimit(Resource::Nofile);

    // `None` encodes `RLIM_INFINITY`. An infinite hard limit clamps
    // nothing, mirroring C's `want > rl.rlim_max` being false there.
    let want = match limit.maximum {
        Some(maximum) => FD_LIMIT_TARGET.min(maximum),
        None => FD_LIMIT_TARGET,
    };

    // An infinite soft limit already exceeds any finite target, so there is
    // nothing to raise - again matching `rl.rlim_cur < want` in C.
    let Some(current) = limit.current else {
        return;
    };
    if current >= want {
        return;
    }

    limit.current = Some(want);
    let _ = setrlimit(Resource::Nofile, limit);
}

/// No-op on platforms without `RLIMIT_NOFILE`.
///
/// Windows has no per-process descriptor rlimit to raise; the C runtime's
/// file-handle ceiling is not settable through this interface. The
/// `dir_sandbox` carrier this guards is itself `#[cfg(unix)]`.
#[cfg(not(unix))]
pub fn raise_fd_limit() {}

#[cfg(all(test, unix))]
mod tests {
    use super::{FD_LIMIT_TARGET, raise_fd_limit};
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

    /// Set the soft limit, leaving the hard limit alone.
    fn set_soft(current: u64) {
        let limit = getrlimit(Resource::Nofile);
        setrlimit(
            Resource::Nofile,
            Rlimit {
                current: Some(current),
                maximum: limit.maximum,
            },
        )
        .expect("set soft RLIMIT_NOFILE");
    }

    /// A soft limit below the target is raised to it.
    ///
    /// The starting limit is lowered explicitly rather than taken from the
    /// ambient environment: macOS ships a soft limit far above
    /// `FD_LIMIT_TARGET`, so a test that skipped this step would assert
    /// nothing on the platform it runs on most often.
    #[test]
    fn raises_a_low_soft_limit_to_the_target() {
        let original = getrlimit(Resource::Nofile);
        let hard = original.maximum.unwrap_or(u64::MAX);
        assert!(
            hard >= FD_LIMIT_TARGET,
            "environment cannot exercise this case: hard limit {hard} < target {FD_LIMIT_TARGET}"
        );

        set_soft(64);
        raise_fd_limit();

        assert_eq!(
            getrlimit(Resource::Nofile).current,
            Some(FD_LIMIT_TARGET),
            "a soft limit below the target must be raised to it"
        );
    }

    /// A soft limit already above the target is left alone.
    ///
    /// upstream: `main.c:1804` `if (rl.rlim_cur < want)` - raise-only. An
    /// unconditional assignment would silently *lower* the limit of an
    /// operator who raised it on purpose, which is a regression the
    /// low-limit test above cannot see.
    #[test]
    fn never_lowers_a_soft_limit_above_the_target() {
        let original = getrlimit(Resource::Nofile);
        let hard = original.maximum.unwrap_or(u64::MAX);
        let generous = FD_LIMIT_TARGET * 2;
        assert!(
            hard >= generous,
            "environment cannot exercise this case: hard limit {hard} < {generous}"
        );

        set_soft(generous);
        raise_fd_limit();

        assert_eq!(
            getrlimit(Resource::Nofile).current,
            Some(generous),
            "a soft limit above the target must survive untouched"
        );
    }

    /// The request is clamped to the hard limit.
    ///
    /// This is the cell that matters most. `setrlimit` refuses a soft limit
    /// above the hard limit outright, and the call site discards its error
    /// deliberately (upstream does too), so an unclamped request would not
    /// raise the limit *at all* and would do it silently - the mitigation
    /// becomes inert on exactly the low-ceiling hosts it exists for.
    ///
    /// Lowering the hard limit is irreversible for the process, which is
    /// safe here only because nextest runs each test in its own process.
    ///
    /// upstream: `main.c:1802` `if (want > rl.rlim_max) want = rl.rlim_max;`
    #[test]
    fn clamps_the_request_to_a_hard_limit_below_the_target() {
        let ceiling = 256;
        assert!(ceiling < FD_LIMIT_TARGET);

        setrlimit(
            Resource::Nofile,
            Rlimit {
                current: Some(64),
                maximum: Some(ceiling),
            },
        )
        .expect("lower the hard RLIMIT_NOFILE");

        raise_fd_limit();

        assert_eq!(
            getrlimit(Resource::Nofile).current,
            Some(ceiling),
            "the raise must clamp to the hard limit, not fail silently at 64"
        );
    }

    /// An equal soft and hard limit at the target is a no-op, not a failure.
    #[test]
    fn leaves_a_soft_limit_exactly_at_the_target_alone() {
        let original = getrlimit(Resource::Nofile);
        let hard = original.maximum.unwrap_or(u64::MAX);
        assert!(
            hard >= FD_LIMIT_TARGET,
            "environment cannot exercise this case"
        );

        set_soft(FD_LIMIT_TARGET);
        raise_fd_limit();

        assert_eq!(
            getrlimit(Resource::Nofile).current,
            Some(FD_LIMIT_TARGET),
            "an already-satisfied limit must be left as it is"
        );
    }
}
