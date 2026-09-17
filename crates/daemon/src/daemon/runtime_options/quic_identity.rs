// QUIC listener identity resolution (docs/design/quic-transport-policy.md).
//
// The QUIC listener presents an operator-supplied certificate/key pair loaded
// from the `quic cert file` / `quic key file` directives. There is no
// in-memory/ephemeral fallback: if QUIC is requested without both files the
// daemon refuses to start (fail loudly, no silent degrade), mirroring oc's
// hard-fail-no-fallback posture for the QUIC transport. An auto-generated
// daemon certificate was considered (decision A) and dropped 2026-09-18: it
// created more problems than it solved.

/// The operator-supplied certificate/key pair the QUIC listener presents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct QuicIdentity {
    /// Path to the PEM certificate chain (leaf first).
    pub(crate) cert: PathBuf,
    /// Path to the PEM private key (PKCS#8, PKCS#1, or SEC1).
    pub(crate) key: PathBuf,
}

impl RuntimeOptions {
    /// Resolves the operator-configured QUIC certificate/key pair.
    ///
    /// Returns `Some` only when BOTH `quic cert file` and `quic key file` are
    /// set. Returns `None` when neither or only one is set - there is no
    /// ephemeral fallback, so a caller that finds QUIC requested (see
    /// [`RuntimeOptions::quic_listener_enabled`]) but resolution `None` must
    /// fail loudly rather than synthesize a certificate.
    #[allow(dead_code)] // REASON: consumed by the QUIC listener wiring under cfg(all(unix, feature = "quic"))
    pub(crate) fn resolve_quic_identity(&self) -> Option<QuicIdentity> {
        match (&self.quic_cert_file, &self.quic_key_file) {
            (Some(cert), Some(key)) => Some(QuicIdentity {
                cert: cert.clone(),
                key: key.clone(),
            }),
            _ => None,
        }
    }

    /// Reports whether the operator requested a UDP/QUIC listener.
    ///
    /// The dedicated `quic = yes` enable directive from
    /// docs/design/quic-transport-policy.md (Decision, daemon-side config) is
    /// not yet parsed, so this interim predicate treats QUIC as requested when
    /// the operator configured any QUIC directive: a certificate path, a key
    /// path, or an explicit `quic port`. A daemon with no QUIC directives never
    /// opens the UDP socket, so a default `--all-features` build stays TCP-only
    /// and byte-identical. When the enable directive lands, this is the single
    /// seam to consult it instead.
    ///
    /// "Requested" is not "serviceable": QUIC has no ephemeral fallback, so a
    /// request without both cert and key ([`RuntimeOptions::resolve_quic_identity`]
    /// returning `None`) is a fatal misconfiguration, not a silent no-op.
    #[allow(dead_code)] // REASON: consumed by the QUIC listener wiring under cfg(all(unix, feature = "quic"))
    pub(crate) fn quic_listener_enabled(&self) -> bool {
        self.quic_cert_file.is_some() || self.quic_key_file.is_some() || self.quic_port.is_some()
    }

    /// Validates that a requested QUIC listener is serviceable, returning the
    /// operator-facing reason it is not (or `None` when it is).
    ///
    /// QUIC has no ephemeral fallback, so a request (any `quic *` directive)
    /// without BOTH a certificate and a key cannot stand up a listener. The
    /// single owner of that rule: the startup path calls this and refuses to
    /// start when it returns `Some`, rather than synthesizing an identity or
    /// silently skipping the listener.
    #[allow(dead_code)] // REASON: consumed by the QUIC listener wiring under cfg(all(unix, feature = "quic"))
    pub(crate) fn quic_config_error(&self) -> Option<String> {
        if self.quic_listener_enabled() && self.resolve_quic_identity().is_none() {
            Some(
                "QUIC listener requested but no certificate configured: set both \
                 `quic cert file` and `quic key file` (there is no ephemeral fallback)"
                    .to_owned(),
            )
        } else {
            None
        }
    }

    /// Returns the port the QUIC listener binds.
    ///
    /// The `quic port` global directive overrides it independently; unset, the
    /// QUIC listener shares the daemon TCP `port` (873 by default). A configured
    /// `quic port = 0` was already coerced to 873 at parse time, mirroring the
    /// TCP `port = 0` path (oc extension - decision on 2026-07-30).
    #[allow(dead_code)] // REASON: consumed by the QUIC listener wiring under cfg(all(unix, feature = "quic"))
    pub(crate) fn effective_quic_port(&self) -> u16 {
        self.quic_port.unwrap_or(self.port)
    }
}
