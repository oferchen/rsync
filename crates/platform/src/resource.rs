//! Process `RLIMIT_NOFILE` access (Unix only).
//!
//! upstream: `main.c:1824-1839` `raise_fd_limit()` reads and writes this
//! limit with `getrlimit`/`setrlimit`.

use std::io;

/// A process's open-file limit; `None` encodes `RLIM_INFINITY`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NofileLimit {
    /// Soft limit, enforced by the kernel.
    pub current: Option<u64>,
    /// Hard limit, the ceiling for the soft limit.
    pub maximum: Option<u64>,
}

/// Returns the calling process's `RLIMIT_NOFILE`.
#[must_use]
pub fn nofile_limit() -> NofileLimit {
    let limit = rustix::process::getrlimit(rustix::process::Resource::Nofile);
    NofileLimit {
        current: limit.current,
        maximum: limit.maximum,
    }
}

/// Sets the calling process's `RLIMIT_NOFILE`, returning the OS error unchanged.
pub fn set_nofile_limit(limit: NofileLimit) -> io::Result<()> {
    rustix::process::setrlimit(
        rustix::process::Resource::Nofile,
        rustix::process::Rlimit {
            current: limit.current,
            maximum: limit.maximum,
        },
    )
    .map_err(io::Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A soft limit written through the wrapper must be the one read back,
    /// and asking for more than the hard limit must fail rather than clamp:
    /// `raise_fd_limit()` relies on both to stay raise-only and clamped.
    #[test]
    fn set_nofile_limit_round_trips_and_refuses_past_the_hard_limit() {
        let original = nofile_limit();
        let lowered = NofileLimit {
            current: Some(64),
            maximum: original.maximum,
        };
        set_nofile_limit(lowered).expect("lowering the soft limit is permitted");
        assert_eq!(nofile_limit(), lowered);

        if let Some(hard) = original.maximum {
            let past = NofileLimit {
                current: Some(hard + 1),
                maximum: original.maximum,
            };
            assert!(set_nofile_limit(past).is_err(), "soft above hard must fail");
        }
        set_nofile_limit(original).expect("restore");
    }
}
