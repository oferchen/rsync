//! The longest path this platform accepts, as upstream's `MAXPATHLEN` resolves
//! it.
//!
//! Callers that need to refuse an over-long path *before* issuing a syscall
//! need the ceiling as a value. Callers that merely react to the kernel having
//! refused one should key on `ENAMETOOLONG` instead - that errno *is* the
//! condition, so it needs no constant and cannot drift.

/// The value upstream's `MAXPATHLEN` resolves to on this platform.
///
/// upstream: `rsync.h:760-762` takes `MAXPATHLEN` from `<sys/param.h>` and
/// supplies 1024 only when that header did not define one. Every Unix oc
/// supports *does* define it, so reading the platform's own value is what
/// mirrors upstream - not the fallback.
///
/// The distinction is load-bearing, not pedantic: `PATH_MAX` is 1024 on macOS
/// but 4096 on Linux. Hardcoding upstream's 1024 fallback would refuse, on
/// Linux, paths that upstream itself accepts - an over-refusal, which is its
/// own divergence rather than a conservative approximation of the right one.
///
/// Windows takes upstream's fallback deliberately. `libc::PATH_MAX` is 260
/// there - the legacy `MAX_PATH` - which is *not* the operative bound for oc,
/// because [`crate::win_path`] addresses long paths through the extended-length
/// `\\?\` prefix. 1024 is upstream's own no-header value and does not pretend
/// to a precision this platform does not offer.
#[must_use]
pub fn max_path_len() -> usize {
    #[cfg(unix)]
    {
        // `PATH_MAX` is `c_int` on every supported Unix, so this widens rather
        // than re-types: it raises neither `useless_conversion` nor
        // `unnecessary_cast` on macOS or Linux.
        libc::PATH_MAX as usize
    }

    #[cfg(not(unix))]
    {
        1024
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WHY: the whole point of reading the platform value is that a fixed
    /// constant is wrong somewhere. This pins that the value is the platform's
    /// own, so a future "simplification" to a literal fails here rather than
    /// silently over-refusing on Linux.
    #[cfg(unix)]
    #[test]
    fn the_bound_is_the_platforms_own_path_max() {
        assert_eq!(max_path_len(), libc::PATH_MAX as usize);
    }

    /// The two platforms oc builds for on Unix disagree, and that disagreement
    /// is exactly why this is a function and not a constant.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_resolves_to_1024() {
        assert_eq!(max_path_len(), 1024);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_resolves_to_4096() {
        assert_eq!(max_path_len(), 4096);
    }

    /// A bound of zero would make every rule over-long; a tiny one would refuse
    /// ordinary paths. Neither platform arm may degrade that far.
    #[test]
    fn the_bound_admits_an_ordinary_path() {
        assert!(
            max_path_len() >= 1024,
            "an ordinary path must fit: {}",
            max_path_len()
        );
    }
}
