use super::*;
use engine::signature::SignatureAlgorithm;

impl ClientConfig {
    /// Reports whether strong checksum comparison should be used when evaluating updates.
    #[must_use]
    #[doc(alias = "--checksum")]
    pub const fn checksum(&self) -> bool {
        self.checksum
    }

    /// Returns the negotiated strong checksum choice.
    #[must_use]
    #[doc(alias = "--checksum-choice")]
    pub const fn checksum_choice(&self) -> StrongChecksumChoice {
        self.checksum_choice
    }

    /// Returns the strong checksum algorithm applied during local validation.
    #[must_use]
    pub const fn checksum_signature_algorithm(&self) -> SignatureAlgorithm {
        let algorithm = self.checksum_choice.file_signature_algorithm();
        // upstream: checksum.c:342 `XXH64(buf, len, checksum_seed)` - and the
        // XXH3 siblings just below it - pass the signed seed straight into an
        // unsigned 64-bit parameter, so C sign-extends and `as u64` must too.
        match (algorithm, self.checksum_seed) {
            (SignatureAlgorithm::Xxh64 { .. }, Some(seed)) => {
                SignatureAlgorithm::Xxh64 { seed: seed as u64 }
            }
            (SignatureAlgorithm::Xxh3 { .. }, Some(seed)) => {
                SignatureAlgorithm::Xxh3 { seed: seed as u64 }
            }
            (SignatureAlgorithm::Xxh3_128 { .. }, Some(seed)) => {
                SignatureAlgorithm::Xxh3_128 { seed: seed as u64 }
            }
            (other, _) => other,
        }
    }

    /// Returns the checksum seed configured via `--checksum-seed`, if any.
    ///
    /// upstream: options.c:151 declares `int checksum_seed`, a signed global.
    /// It is forwarded to the remote at options.c:3047 as
    /// `"--checksum-seed=%d"`.
    #[doc(alias = "--checksum-seed")]
    pub const fn checksum_seed(&self) -> Option<i32> {
        self.checksum_seed
    }

    /// Returns the protocol-layer checksum algorithm override for negotiation.
    ///
    /// When the user specified a non-Auto `--checksum-choice`, this returns the
    /// corresponding [`ChecksumAlgorithm`](protocol::ChecksumAlgorithm) to force
    /// during protocol 30+ capability negotiation. Returns `None` when the default
    /// automatic negotiation should be used.
    #[doc(alias = "--checksum-choice")]
    pub const fn checksum_protocol_override(&self) -> Option<protocol::ChecksumAlgorithm> {
        self.checksum_choice.transfer_protocol_override()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ClientConfig {
        ClientConfig::default()
    }

    #[test]
    fn checksum_default_is_false() {
        let config = default_config();
        assert!(!config.checksum());
    }

    #[test]
    fn checksum_choice_default() {
        let config = default_config();
        let _choice = config.checksum_choice();
    }

    #[test]
    fn checksum_seed_default_is_none() {
        let config = default_config();
        assert!(config.checksum_seed().is_none());
    }

    #[test]
    fn checksum_signature_algorithm_returns_algorithm() {
        let config = default_config();
        let _algorithm = config.checksum_signature_algorithm();
    }
}
