use super::*;
use oc_rsync_engine::signature::SignatureAlgorithm;

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
    #[must_use]
    #[doc(alias = "--checksum-seed")]
    pub const fn checksum_seed(&self) -> Option<u32> {
        self.checksum_seed
    }
}
