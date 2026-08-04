// QUIC listener identity resolution (docs/design/quic-transport-policy.md,
// decision A).
//
// The QUIC listener needs a certificate/key to present. When the operator sets
// `quic cert file` / `quic key file` the daemon loads those files. When neither
// is set the daemon mints a fresh self-signed certificate in memory at bind
// time (as `QuicAcceptor::bind` already does with rcgen) - ephemeral, not
// written to disk and regenerated on every start. This file resolves which of
// the two the operator asked for; building the listener from it lands under a
// later QUIC task.

/// The certificate source the QUIC listener presents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum QuicIdentity {
    /// No cert configured: the listener generates a fresh self-signed
    /// certificate in memory at bind time. Nothing is written to disk and the
    /// identity changes on every restart, so an operator who needs a stable
    /// trust-on-first-use identity must configure explicit cert/key files.
    Ephemeral,
    /// Operator-supplied certificate and private-key paths, used verbatim.
    Files { cert: PathBuf, key: PathBuf },
}

impl RuntimeOptions {
    /// Resolves the certificate source the QUIC listener presents.
    ///
    /// Returns [`QuicIdentity::Files`] with the configured paths when both
    /// `quic cert file` and `quic key file` are set - the operator owns the
    /// identity. Otherwise returns [`QuicIdentity::Ephemeral`]: a fresh
    /// in-memory self-signed certificate the listener generates at bind time,
    /// never persisted and regenerated every start (decision A).
    #[allow(dead_code)] // REASON: consumed by the QUIC listener build-out (QUIC-6c); exercised by tests now
    pub(crate) fn resolve_quic_identity(&self) -> QuicIdentity {
        match (&self.quic_cert_file, &self.quic_key_file) {
            (Some(cert), Some(key)) => QuicIdentity::Files {
                cert: cert.clone(),
                key: key.clone(),
            },
            _ => QuicIdentity::Ephemeral,
        }
    }

    /// Returns the port the QUIC listener binds.
    ///
    /// The `quic port` global directive overrides it independently; unset, the
    /// QUIC listener shares the daemon TCP `port` (873 by default). A configured
    /// `quic port = 0` was already coerced to 873 at parse time, mirroring the
    /// TCP `port = 0` path (oc extension - decision on 2026-07-30).
    #[allow(dead_code)] // REASON: consumed by the QUIC listener build-out (QUIC-6c); exercised by tests now
    pub(crate) fn effective_quic_port(&self) -> u16 {
        self.quic_port.unwrap_or(self.port)
    }
}
