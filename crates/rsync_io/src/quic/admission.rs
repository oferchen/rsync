//! Server admission of new QUIC connections: when to answer a client's first
//! Initial with a stateless Retry, and how much quinn may buffer for
//! connection attempts the driver has not yet disposed of.
//!
//! The RFC 9000 section 8 anti-amplification limit (3x the bytes received
//! from an unvalidated address) is enforced by quinn, but it does not stop a
//! spoofed-source flood from making the server hold handshake state. A Retry
//! (RFC 9000 section 8.1.2) costs the server no state: the client must echo
//! the token from a reachable address before any connection state exists.
//! It also costs every client one extra round trip, so it is sent only when
//! the server is under load unless the operator asks otherwise.

use std::num::NonZeroUsize;
use std::time::Duration;

use quinn_proto::ServerConfig;

use super::driver::ACCEPT_BACKLOG;

/// Handshakes in flight at which [`RetryPolicy::UnderLoad`] starts sending
/// Retry.
///
/// Taken from HAProxy's `tune.quic.retry-threshold`, whose default of 100
/// half-open connections triggers its dynamic Retry. It sits below the
/// driver's 128-connection accept backlog, so Retry engages before new
/// connections are refused outright.
pub const RETRY_UNDER_LOAD_THRESHOLD: usize = 100;

/// How long a Retry token stays valid: quinn's default of 15 seconds, kept
/// unchanged. A client echoes the token in its very next Initial, so one
/// round trip plus loss recovery fits easily; a longer window only widens
/// replay of a captured token. The token key is the random per-process key
/// quinn's `ServerConfig::with_crypto` generates, so tokens never outlive the
/// process that issued them.
pub const RETRY_TOKEN_LIFETIME: Duration = Duration::from_secs(15);

/// Bytes quinn may buffer for one connection attempt it has handed to the
/// driver but the driver has not yet accepted, refused or retried.
///
/// 64 KiB holds a client's whole Initial flight (a post-quantum ClientHello
/// spans two or three datagrams) many times over. quinn's default is 10 MiB.
pub const INCOMING_BUFFER_PER_ATTEMPT: u64 = 64 * 1024;

/// When the server answers a new connection with a stateless Retry, forcing
/// the client to prove it can receive at its source address first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryPolicy {
    /// Never send Retry; accept every unvalidated client directly.
    Never,
    /// Send Retry while at least `threshold` handshakes are in flight.
    UnderLoad {
        /// Handshakes in flight at which Retry starts.
        threshold: usize,
    },
    /// Send Retry to every client whose address is not yet validated.
    Always,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::UnderLoad {
            threshold: RETRY_UNDER_LOAD_THRESHOLD,
        }
    }
}

/// What the driver does with a new connection attempt it has room for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Admission {
    /// Start the handshake now.
    Accept,
    /// Answer with a stateless Retry.
    Retry,
}

impl RetryPolicy {
    /// Decides whether a new connection attempt is accepted or retried.
    ///
    /// A client whose address is already validated (it echoed a Retry token,
    /// or presented a NEW_TOKEN from an earlier connection) is always
    /// accepted: retrying it again is illegal and pointless. `handshaking` is
    /// the number of connections on the endpoint still in their handshake.
    pub(super) fn admit(self, address_validated: bool, handshaking: usize) -> Admission {
        if address_validated {
            return Admission::Accept;
        }
        let retry = match self {
            Self::Never => false,
            Self::UnderLoad { threshold } => handshaking >= threshold,
            Self::Always => true,
        };
        if retry {
            Admission::Retry
        } else {
            Admission::Accept
        }
    }
}

/// Bounds on what quinn holds for connection attempts not yet disposed of.
///
/// quinn's defaults (65536 attempts, 10 MiB each, 100 MiB in total) are
/// sized for a general-purpose server. The driver disposes of every attempt
/// before it reads the next datagram, so these are defence in depth, sized
/// to what the daemon will actually serve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IncomingLimits {
    max_incoming: usize,
    buffer_per_attempt: u64,
    buffer_total: u64,
}

impl IncomingLimits {
    /// Sizes the limits from the daemon's `max connections`: no more
    /// attempts than sessions the daemon would serve, and never more than the
    /// driver's accept backlog. Unlimited (`None`) uses the accept backlog.
    #[must_use]
    pub fn for_max_connections(max_connections: Option<NonZeroUsize>) -> Self {
        let max_incoming =
            max_connections.map_or(ACCEPT_BACKLOG, |max| max.get().min(ACCEPT_BACKLOG));
        Self {
            max_incoming,
            buffer_per_attempt: INCOMING_BUFFER_PER_ATTEMPT,
            buffer_total: INCOMING_BUFFER_PER_ATTEMPT * max_incoming as u64,
        }
    }

    /// Connection attempts quinn may hold at once.
    #[must_use]
    pub const fn max_incoming(&self) -> usize {
        self.max_incoming
    }

    /// Bytes quinn may buffer across all held attempts.
    #[must_use]
    pub const fn buffer_total(&self) -> u64 {
        self.buffer_total
    }

    /// Writes these limits and the Retry token lifetime into `config`.
    pub(super) fn apply(&self, config: &mut ServerConfig) {
        config
            .max_incoming(self.max_incoming)
            .incoming_buffer_size(self.buffer_per_attempt)
            .incoming_buffer_size_total(self.buffer_total)
            .retry_token_lifetime(RETRY_TOKEN_LIFETIME);
    }
}

impl Default for IncomingLimits {
    fn default() -> Self {
        Self::for_max_connections(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: usize = RETRY_UNDER_LOAD_THRESHOLD;

    /// `auto` must cost nobody a round trip until the endpoint is loaded, and
    /// must Retry from exactly the threshold: one below is still accepted.
    #[test]
    fn under_load_retries_from_exactly_the_threshold() {
        let policy = RetryPolicy::default();
        assert_eq!(policy, RetryPolicy::UnderLoad { threshold: T });
        assert_eq!(policy.admit(false, 0), Admission::Accept);
        assert_eq!(policy.admit(false, T - 1), Admission::Accept);
        assert_eq!(policy.admit(false, T), Admission::Retry);
        assert_eq!(policy.admit(false, T + 1), Admission::Retry);
    }

    /// `always` validates every unvalidated client, even on an idle endpoint.
    #[test]
    fn always_retries_every_unvalidated_client() {
        assert_eq!(RetryPolicy::Always.admit(false, 0), Admission::Retry);
        assert_eq!(RetryPolicy::Always.admit(false, T), Admission::Retry);
    }

    /// `never` accepts even a loaded endpoint's new clients directly.
    #[test]
    fn never_accepts_however_loaded() {
        assert_eq!(RetryPolicy::Never.admit(false, 0), Admission::Accept);
        assert_eq!(RetryPolicy::Never.admit(false, T), Admission::Accept);
        assert_eq!(
            RetryPolicy::Never.admit(false, usize::MAX),
            Admission::Accept
        );
    }

    /// A client that already proved its address is accepted under every
    /// policy: quinn forbids a second Retry, and it would only add latency.
    #[test]
    fn validated_address_is_accepted_under_every_policy() {
        for policy in [
            RetryPolicy::Never,
            RetryPolicy::default(),
            RetryPolicy::Always,
        ] {
            assert_eq!(
                policy.admit(true, usize::MAX),
                Admission::Accept,
                "{policy:?}"
            );
        }
    }

    /// The limits follow `max connections` but never exceed the accept
    /// backlog, and stay far below quinn's 65536-attempt, 100 MiB defaults.
    #[test]
    fn limits_follow_max_connections_up_to_the_backlog() {
        let four = IncomingLimits::for_max_connections(NonZeroUsize::new(4));
        assert_eq!(four.max_incoming(), 4);
        assert_eq!(four.buffer_total(), 4 * INCOMING_BUFFER_PER_ATTEMPT);

        let huge = IncomingLimits::for_max_connections(NonZeroUsize::new(1 << 20));
        assert_eq!(huge.max_incoming(), ACCEPT_BACKLOG);

        let unlimited = IncomingLimits::default();
        assert_eq!(unlimited.max_incoming(), ACCEPT_BACKLOG);
        assert!(unlimited.buffer_total() < 100 << 20);
    }
}
