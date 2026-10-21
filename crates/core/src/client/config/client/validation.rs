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

    /// Returns the block strong checksum (`xfer_sum`) for a local delta.
    ///
    /// A local copy runs upstream's full protocol against its forked child, so
    /// a fully-auto choice negotiates the first common entry of
    /// `valid_checksums_items[]` (xxh128) and a set choice forces its transfer
    /// component. The child sends the `C` capability, so MD5 hashes the seed
    /// first; the seed is `--checksum-seed` or the server's generated one.
    ///
    /// upstream: compat.c:538-574 `negotiate_the_strings()`, checksum.c:192-207
    /// `parse_checksum_choice()`, checksum.c:322-412 `get_checksum2()`,
    /// options.c:3228 the `C` flag, compat.c:823-824 the seed.
    #[must_use]
    pub fn transfer_signature_algorithm(&self) -> SignatureAlgorithm {
        let algorithm = self
            .checksum_protocol_override()
            .unwrap_or(protocol::ChecksumAlgorithm::XXH128);
        let seed = transfer::setup::resolve_checksum_seed(self.checksum_seed);
        transfer::ChecksumFactory::new(algorithm, seed, true).signature_algorithm()
    }

    /// Returns the checksum seed configured via `--checksum-seed`, if any.
    ///
    /// upstream: options.c:151 declares `int checksum_seed`, a signed global.
    /// It is forwarded to the remote at options.c:3057 as
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

    fn transfer_algorithm(choice: &str, seed: Option<i32>) -> SignatureAlgorithm {
        ClientConfig::builder()
            .checksum_choice(StrongChecksumChoice::parse(choice).expect("valid choice"))
            .checksum_seed(seed)
            .build()
            .transfer_signature_algorithm()
    }

    /// upstream: compat.c:538 negotiates xxh128 for an unforced local copy and
    /// checksum.c:192-207 forces the first `--checksum-choice` component, with
    /// a lone `auto` meaning md5 (checksum.c:136-141). get_checksum2 seeds
    /// every sum with `checksum_seed`, MD5 seed-first under the `C` flag.
    #[test]
    fn transfer_signature_algorithm_mirrors_local_negotiation() {
        use checksums::strong::Md5Seed;

        assert_eq!(
            transfer_algorithm("auto", Some(5)),
            SignatureAlgorithm::Xxh3_128 { seed: 5 }
        );
        assert_eq!(
            transfer_algorithm("auto,auto", Some(-1)),
            SignatureAlgorithm::Xxh3_128 { seed: u64::MAX }
        );
        assert_eq!(
            transfer_algorithm("md5", Some(5)),
            SignatureAlgorithm::Md5 {
                seed_config: Md5Seed::proper(5)
            }
        );
        assert_eq!(
            transfer_algorithm("xxh3,md4", Some(5)),
            SignatureAlgorithm::Xxh3 { seed: 5 }
        );
        assert_eq!(
            transfer_algorithm("auto,md4", Some(5)),
            SignatureAlgorithm::Md5 {
                seed_config: Md5Seed::proper(5)
            }
        );
        assert_eq!(
            transfer_algorithm("md4", Some(5)),
            SignatureAlgorithm::Md4Seeded { seed: 5 }
        );
        assert!(matches!(
            transfer_algorithm("auto", None),
            SignatureAlgorithm::Xxh3_128 { .. }
        ));
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
