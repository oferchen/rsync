//! Configuration enums for the embedded SSH transport.
//!
//! These types mirror OpenSSH client options and control host key
//! verification policy and IP version preference for DNS resolution.

/// Host key verification policy.
///
/// Controls behavior when the remote server's host key is not recognized.
/// Mirrors the SSH `StrictHostKeyChecking` option semantics.
///
/// The four variants correspond one-to-one with upstream's
/// `SSH_STRICT_HOSTKEY_*` constants (openssh/readconf.h:225-228).
///
/// upstream: openssh/readconf.c:1019-1028 `multistate_strict_hostkey[]`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictHostKeyChecking {
    /// Reject connections to hosts with unknown or mismatched keys.
    ///
    /// Safest option - requires the host key to already exist in the
    /// known hosts file. New hosts must be added manually.
    Yes,
    /// Accept unknown host keys without prompting and persist them.
    ///
    /// Insecure - vulnerable to MITM on first connect. Changed keys
    /// are still rejected. Useful for automated/batch transfers.
    No,
    /// Accept and persist an unknown host key, but reject a changed one.
    ///
    /// Upstream's `accept-new`. Distinct from [`Self::No`] only on the
    /// changed-key path, where upstream refuses for every policy except
    /// `off`/`no` (openssh/sshconnect.c:1329-1331). oc refuses a changed key
    /// under every policy, so the two variants differ here only in that
    /// `No` is documented as the blanket opt-out.
    AcceptNew,
    /// Prompt the user interactively when encountering an unknown host key.
    ///
    /// Default mode, matching OpenSSH behavior. Falls back to rejection
    /// when no TTY is available (e.g., backgrounded or piped).
    Ask,
}

/// A `StrictHostKeyChecking` value that upstream's multistate table does
/// not contain. Carries the rejected spelling so the caller can name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownStrictHostKeyChecking(pub String);

impl StrictHostKeyChecking {
    /// Every spelling upstream accepts, in table order.
    ///
    /// upstream: openssh/readconf.c:1019-1028. Seven spellings collapse onto four
    /// states: `off`/`no`/`false` all mean [`Self::No`], and `yes`/`true`
    /// both mean [`Self::Yes`].
    pub const ACCEPTED_VALUES: [&'static str; 7] =
        ["true", "false", "yes", "no", "ask", "off", "accept-new"];

    /// Maps an operator-supplied spelling onto a policy.
    ///
    /// Matching is case-insensitive, mirroring upstream's `strcasecmp`
    /// (openssh/readconf.c:1114). An unrecognised value is an error rather than a
    /// silent fallback: upstream reports `unsupported option` and counts a
    /// bad option, which terminates the run (openssh/readconf.c:1270-1275).
    pub fn parse(value: &str) -> Result<Self, UnknownStrictHostKeyChecking> {
        // upstream: openssh/readconf.c:1019-1028 multistate_strict_hostkey[]
        if value.eq_ignore_ascii_case("yes") || value.eq_ignore_ascii_case("true") {
            Ok(Self::Yes)
        } else if value.eq_ignore_ascii_case("no")
            || value.eq_ignore_ascii_case("false")
            || value.eq_ignore_ascii_case("off")
        {
            Ok(Self::No)
        } else if value.eq_ignore_ascii_case("accept-new") {
            Ok(Self::AcceptNew)
        } else if value.eq_ignore_ascii_case("ask") {
            Ok(Self::Ask)
        } else {
            Err(UnknownStrictHostKeyChecking(value.to_owned()))
        }
    }
}

impl Default for StrictHostKeyChecking {
    fn default() -> Self {
        Self::Ask
    }
}

/// IP version preference for DNS resolution.
///
/// Controls whether the SSH transport resolves hostnames to IPv4 or IPv6
/// addresses. Mirrors the SSH `-4`/`-6` flag behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpPreference {
    /// Let the system choose based on available addresses.
    Auto,
    /// Prefer IPv6 addresses when both are available. Mirrors `ssh -6` with
    /// fallback to IPv4.
    PreferV6,
    /// Only resolve and connect to IPv4 addresses. Mirrors `ssh -4`.
    ForceV4,
    /// Only resolve and connect to IPv6 addresses. Mirrors `ssh -6`.
    ForceV6,
}

impl Default for IpPreference {
    fn default() -> Self {
        Self::Auto
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every spelling upstream's multistate table accepts, with the state it
    /// maps to. Seven spellings collapse onto four states.
    ///
    /// upstream: openssh/readconf.c:1019-1028 `multistate_strict_hostkey[]`, whose
    /// values are the `SSH_STRICT_HOSTKEY_*` constants at openssh/readconf.h:225-228.
    #[test]
    fn parse_accepts_every_upstream_spelling() {
        let table = [
            ("true", StrictHostKeyChecking::Yes),
            ("false", StrictHostKeyChecking::No),
            ("yes", StrictHostKeyChecking::Yes),
            ("no", StrictHostKeyChecking::No),
            ("ask", StrictHostKeyChecking::Ask),
            ("off", StrictHostKeyChecking::No),
            ("accept-new", StrictHostKeyChecking::AcceptNew),
        ];
        for (spelling, expected) in table {
            assert_eq!(
                StrictHostKeyChecking::parse(spelling),
                Ok(expected),
                "spelling {spelling}"
            );
        }
        // The advertised list must BE the accepted set, not a stale subset: a
        // value in ACCEPTED_VALUES that parse() rejects would point operators
        // at a spelling the parser refuses.
        for spelling in StrictHostKeyChecking::ACCEPTED_VALUES {
            assert!(
                StrictHostKeyChecking::parse(spelling).is_ok(),
                "advertised value {spelling} is not accepted"
            );
        }
        assert_eq!(table.len(), StrictHostKeyChecking::ACCEPTED_VALUES.len());
    }

    /// Matching is case-insensitive.
    ///
    /// upstream: openssh/readconf.c:1114 `strcasecmp(arg, multistate_ptr[i].key)`.
    #[test]
    fn parse_is_case_insensitive() {
        assert_eq!(
            StrictHostKeyChecking::parse("YES"),
            Ok(StrictHostKeyChecking::Yes)
        );
        assert_eq!(
            StrictHostKeyChecking::parse("Accept-New"),
            Ok(StrictHostKeyChecking::AcceptNew)
        );
        assert_eq!(
            StrictHostKeyChecking::parse("OFF"),
            Ok(StrictHostKeyChecking::No)
        );
    }

    /// An unrecognised value is REFUSED, never silently mapped onto a policy.
    ///
    /// Regression pin for the defect this fixes: the previous mapping had a
    /// `_ => Ask` arm, so `accept-new` and every typo became an interactive
    /// prompt. Upstream reports `unsupported option` and counts a bad option,
    /// terminating the run (openssh/readconf.c:1270-1275, :2611-2613).
    #[test]
    fn parse_refuses_an_unknown_value_rather_than_defaulting() {
        for bogus in ["accept_new", "acceptnew", "maybe", "", "ask-new", "1"] {
            assert_eq!(
                StrictHostKeyChecking::parse(bogus),
                Err(UnknownStrictHostKeyChecking(bogus.to_owned())),
                "value {bogus:?} must be refused"
            );
        }
    }

    /// `accept-new` is its own state, distinct from all three originals. If it
    /// collapsed onto any of them the connection policy would change.
    #[test]
    fn accept_new_is_a_distinct_state() {
        let parsed = StrictHostKeyChecking::parse("accept-new").expect("accepted");
        assert_ne!(parsed, StrictHostKeyChecking::Yes);
        assert_ne!(parsed, StrictHostKeyChecking::No);
        assert_ne!(parsed, StrictHostKeyChecking::Ask);
    }

    /// The default stays `ask`, matching upstream's filled default.
    #[test]
    fn default_is_ask() {
        assert_eq!(StrictHostKeyChecking::default(), StrictHostKeyChecking::Ask);
    }
}
