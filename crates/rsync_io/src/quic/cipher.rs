//! Client-side QUIC cipher-suite selection.
//!
//! rustls' ring provider lists its TLS 1.3 cipher suites in a fixed,
//! non-adaptive preference order - AES-256-GCM, AES-128-GCM, then
//! ChaCha20-Poly1305 - regardless of whether the host CPU can accelerate AES.
//! rustls' own `manual/defaults.rs` documents this and instructs the
//! application to change the order itself: "if you know your application will
//! run on a platform without [hardware AES], you should definitely change the
//! default order to prefer chacha20-poly1305". The bare default therefore
//! prefers AES-GCM even on hosts where un-accelerated AES-GCM is slower and
//! less side-channel resistant than ChaCha20-Poly1305.
//!
//! This module supplies the two pieces that bare default lacks:
//!
//! - [`QuicCipher`] - the explicit `--quic-cipher` override that fixes the
//!   negotiated 1-RTT AEAD family, and
//! - [`client_provider`] - a CPU-adaptive default (used when no override is
//!   given) that keeps the AES-first order on hosts with hardware AES
//!   (byte-identical to the bare default) and reorders ChaCha20-Poly1305 ahead
//!   of AES-GCM on hosts without it, per the rustls recommendation.

use std::io;
use std::sync::Arc;

use rustls::SupportedCipherSuite;
use rustls::crypto::CryptoProvider;
use rustls::crypto::ring::cipher_suite::{
    TLS13_AES_128_GCM_SHA256, TLS13_AES_256_GCM_SHA384, TLS13_CHACHA20_POLY1305_SHA256,
};

/// Client-side QUIC cipher-suite family override (`--quic-cipher`).
///
/// Fixes the negotiated 1-RTT (application) AEAD to one family. Unset (`None`
/// at the call sites) leaves the CPU-adaptive default in [`client_provider`]
/// untouched. Because QUIC always protects Initial packets with AES-128-GCM
/// (RFC 9001 section 5.2), that suite is always offered; see [`Self::suites`]
/// for how each family still forces the 1-RTT choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuicCipher {
    /// Force AES-GCM for 1-RTT (offers AES-256-GCM then AES-128-GCM, no
    /// ChaCha20).
    Aes,
    /// Force ChaCha20-Poly1305 for 1-RTT (offers it first, keeping only the
    /// mandatory AES-128-GCM behind it for Initial packets).
    ChaCha20,
}

impl QuicCipher {
    /// The accepted family names, in a stable order for CLI value restriction
    /// and help text (`--quic-cipher <aes|chacha20>`).
    pub const NAMES: [&'static str; 2] = ["aes", "chacha20"];

    /// Parses a cipher family name (`--quic-cipher`), failing loudly on
    /// anything but the two recognized names (repo policy: never silently fall
    /// back on a bad argument).
    pub fn parse(value: &str) -> io::Result<Self> {
        match value.trim() {
            "aes" => Ok(Self::Aes),
            "chacha20" => Ok(Self::ChaCha20),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unrecognized QUIC cipher {other:?}; expected one of: aes, chacha20"),
            )),
        }
    }

    /// The TLS 1.3 cipher suites this family offers, highest priority first.
    ///
    /// QUIC constraint (RFC 9001 section 5.2): Initial packets are always
    /// protected with AES-128-GCM, so quinn-proto requires the client provider
    /// to contain `TLS13_AES_128_GCM_SHA256` or it refuses the config
    /// (`NoInitialCipherSuite`). Both families therefore keep AES-128-GCM in the
    /// offered set; it protects only the Initial packets. The negotiated 1-RTT
    /// (application) suite is the first entry, because a TLS 1.3 server honours
    /// the client's suite order by default (rustls `ignore_client_order` is
    /// false):
    ///
    /// - `Aes` offers AES-256-GCM then AES-128-GCM - no ChaCha20 - so 1-RTT is
    ///   always AES-GCM.
    /// - `ChaCha20` offers ChaCha20-Poly1305 then AES-128-GCM, ChaCha20 first,
    ///   and drops AES-256-GCM, so 1-RTT is ChaCha20-Poly1305 against any
    ///   client-order-honouring server; a server that overrides to its own
    ///   preference can fall back only to the mandatory AES-128-GCM, never
    ///   AES-256-GCM.
    fn suites(self) -> Vec<SupportedCipherSuite> {
        match self {
            Self::Aes => vec![TLS13_AES_256_GCM_SHA384, TLS13_AES_128_GCM_SHA256],
            Self::ChaCha20 => vec![TLS13_CHACHA20_POLY1305_SHA256, TLS13_AES_128_GCM_SHA256],
        }
    }
}

/// Reports whether the host CPU exposes hardware AES acceleration.
///
/// Uses the std `is_*_feature_detected!` runtime probes - safe macros, no
/// `unsafe` and no `cpufeatures` dependency, so the `#![deny(unsafe_code)]`
/// crate policy holds. Architectures without a probe answer `false`, which
/// selects the ChaCha20-first order rustls recommends for hosts without
/// hardware AES.
fn hardware_aes() -> bool {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        std::arch::is_x86_feature_detected!("aes")
    }
    #[cfg(target_arch = "aarch64")]
    {
        std::arch::is_aarch64_feature_detected!("aes")
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
    {
        false
    }
}

/// Builds the client crypto provider for an optional `--quic-cipher` override.
///
/// - `Some(choice)` offers the chosen family's suites (see [`QuicCipher::suites`]),
///   fixing the 1-RTT AEAD while keeping the AES-128-GCM suite QUIC requires
///   for Initial packets.
/// - `None` yields the CPU-adaptive default: the bare ring provider
///   (AES-256-GCM, AES-128-GCM, ChaCha20-Poly1305 - byte-identical to today)
///   when the host has hardware AES, or a ChaCha20-Poly1305-first reordering
///   otherwise. AES-GCM stays available in the reordered case, only lower
///   priority, so a peer that lacks ChaCha20 still negotiates.
pub(crate) fn client_provider(cipher: Option<QuicCipher>) -> Arc<CryptoProvider> {
    let base = rustls::crypto::ring::default_provider();
    match cipher {
        Some(choice) => Arc::new(CryptoProvider {
            cipher_suites: choice.suites(),
            ..base
        }),
        // Hardware AES present: keep the default order unchanged so the
        // handshake is byte-identical to the pre-`--quic-cipher` client.
        None if hardware_aes() => Arc::new(base),
        // No hardware AES: prefer ChaCha20-Poly1305 (rustls manual/defaults.rs),
        // keeping the AES-GCM suites available at lower priority. The config is
        // built TLS-1.3-only, so these three are exactly the negotiable suites.
        None => Arc::new(CryptoProvider {
            cipher_suites: vec![
                TLS13_CHACHA20_POLY1305_SHA256,
                TLS13_AES_256_GCM_SHA384,
                TLS13_AES_128_GCM_SHA256,
            ],
            ..base
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::CipherSuite;

    fn suite_ids(provider: &CryptoProvider) -> Vec<CipherSuite> {
        provider.cipher_suites.iter().map(|s| s.suite()).collect()
    }

    #[test]
    fn parse_accepts_the_two_families() {
        assert_eq!(QuicCipher::parse("aes").expect("aes"), QuicCipher::Aes);
        assert_eq!(
            QuicCipher::parse("chacha20").expect("chacha20"),
            QuicCipher::ChaCha20
        );
        // Surrounding whitespace is tolerated, mirroring CongestionAlgorithm.
        assert_eq!(
            QuicCipher::parse("  aes ").expect("padded"),
            QuicCipher::Aes
        );
    }

    #[test]
    fn parse_rejects_unknown_family() {
        let err = QuicCipher::parse("blowfish").expect_err("must reject");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// The AES override offers only AES-GCM suites - no ChaCha20 - so the 1-RTT
    /// AEAD is always AES-GCM. AES-256-GCM leads (negotiated under client-order)
    /// and AES-128-GCM follows (also the mandatory QUIC Initial suite). Encodes
    /// WHY the override exists: it forces the family, not merely reorders.
    #[test]
    fn aes_override_offers_only_aes_gcm() {
        let ids = suite_ids(&client_provider(Some(QuicCipher::Aes)));
        assert_eq!(
            ids,
            vec![
                CipherSuite::TLS13_AES_256_GCM_SHA384,
                CipherSuite::TLS13_AES_128_GCM_SHA256,
            ]
        );
        assert!(!ids.contains(&CipherSuite::TLS13_CHACHA20_POLY1305_SHA256));
    }

    /// The ChaCha20 override offers ChaCha20-Poly1305 FIRST so the 1-RTT AEAD is
    /// ChaCha20 (the server honours client order), keeps AES-128-GCM only behind
    /// it for the mandatory QUIC Initial suite, and drops AES-256-GCM entirely.
    /// Encodes WHY the AES-128 suite survives: removing it would make quinn-proto
    /// reject the config (`NoInitialCipherSuite`), so this is the tightest
    /// ChaCha20-forcing set QUIC permits.
    #[test]
    fn chacha20_override_prefers_chacha20_keeping_only_aes128_for_initial() {
        let ids = suite_ids(&client_provider(Some(QuicCipher::ChaCha20)));
        assert_eq!(
            ids,
            vec![
                CipherSuite::TLS13_CHACHA20_POLY1305_SHA256,
                CipherSuite::TLS13_AES_128_GCM_SHA256,
            ]
        );
        assert_eq!(ids[0], CipherSuite::TLS13_CHACHA20_POLY1305_SHA256);
        assert!(!ids.contains(&CipherSuite::TLS13_AES_256_GCM_SHA384));
    }

    /// Every provider this module builds MUST offer AES-128-GCM, or quinn-proto
    /// rejects the QUIC client config (RFC 9001 Initial-packet protection). This
    /// pins that invariant across the override families and the adaptive default
    /// so a future suite-list edit cannot silently reintroduce the bug the
    /// `NoInitialCipherSuite` handshake failure exposed.
    #[test]
    fn every_provider_offers_the_quic_initial_suite() {
        for cipher in [None, Some(QuicCipher::Aes), Some(QuicCipher::ChaCha20)] {
            let ids = suite_ids(&client_provider(cipher));
            assert!(
                ids.contains(&CipherSuite::TLS13_AES_128_GCM_SHA256),
                "provider for {cipher:?} lacks the mandatory QUIC Initial suite"
            );
        }
    }

    /// With no override, the negotiable set always contains both families -
    /// only the preference order is CPU-adaptive - so an un-opted peer still
    /// negotiates whatever it supports.
    #[test]
    fn default_offers_both_families_with_adaptive_order() {
        let ids = suite_ids(&client_provider(None));
        assert!(ids.contains(&CipherSuite::TLS13_CHACHA20_POLY1305_SHA256));
        assert!(ids.contains(&CipherSuite::TLS13_AES_256_GCM_SHA384));
        let chacha = ids
            .iter()
            .position(|s| *s == CipherSuite::TLS13_CHACHA20_POLY1305_SHA256)
            .expect("chacha present");
        let aes = ids
            .iter()
            .position(|s| *s == CipherSuite::TLS13_AES_256_GCM_SHA384)
            .expect("aes present");
        if hardware_aes() {
            // Byte-identical to the bare ring default: AES ahead of ChaCha20.
            assert!(aes < chacha, "hardware AES keeps the AES-first default");
        } else {
            assert!(chacha < aes, "no hardware AES prefers ChaCha20");
        }
    }

    /// On a host with hardware AES (all CI/dev machines), the unset default is
    /// the bare ring provider's exact suite list - the proof that
    /// `--quic-cipher` unset is byte-identical to the pre-feature handshake.
    #[test]
    fn default_matches_bare_ring_provider_on_hardware_aes() {
        if !hardware_aes() {
            return;
        }
        let bare = suite_ids(&Arc::new(rustls::crypto::ring::default_provider()));
        let adaptive = suite_ids(&client_provider(None));
        assert_eq!(adaptive, bare);
    }
}
