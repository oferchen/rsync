use crate::{
    message::{Message, Role},
    rsync_error,
};
use engine::signature::SignatureAlgorithm;

/// Enumerates the strong checksum algorithms recognised by the client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrongChecksumAlgorithm {
    /// Automatically selects the negotiated algorithm (locally resolved to MD5).
    Auto,
    /// MD4 strong checksum.
    Md4,
    /// MD5 strong checksum.
    Md5,
    /// SHA-1 strong checksum.
    Sha1,
    /// XXH64 strong checksum.
    Xxh64,
    /// XXH3/64 strong checksum.
    Xxh3,
    /// XXH3/128 strong checksum.
    Xxh128,
}

impl StrongChecksumAlgorithm {
    /// Converts the selection into the [`SignatureAlgorithm`] used by the transfer engine.
    #[must_use]
    pub const fn to_signature_algorithm(self) -> SignatureAlgorithm {
        use checksums::strong::Md5Seed;
        match self {
            StrongChecksumAlgorithm::Auto | StrongChecksumAlgorithm::Md5 => {
                SignatureAlgorithm::Md5 {
                    seed_config: Md5Seed::none(),
                }
            }
            StrongChecksumAlgorithm::Md4 => SignatureAlgorithm::Md4,
            StrongChecksumAlgorithm::Sha1 => SignatureAlgorithm::Sha1,
            StrongChecksumAlgorithm::Xxh64 => SignatureAlgorithm::Xxh64 { seed: 0 },
            StrongChecksumAlgorithm::Xxh3 => SignatureAlgorithm::Xxh3 { seed: 0 },
            StrongChecksumAlgorithm::Xxh128 => SignatureAlgorithm::Xxh3_128 { seed: 0 },
        }
    }

    /// Returns the canonical flag spelling for the algorithm.
    #[must_use]
    pub const fn canonical_name(self) -> &'static str {
        match self {
            StrongChecksumAlgorithm::Auto => "auto",
            StrongChecksumAlgorithm::Md4 => "md4",
            StrongChecksumAlgorithm::Md5 => "md5",
            StrongChecksumAlgorithm::Sha1 => "sha1",
            StrongChecksumAlgorithm::Xxh64 => "xxh64",
            StrongChecksumAlgorithm::Xxh3 => "xxh3",
            StrongChecksumAlgorithm::Xxh128 => "xxh128",
        }
    }

    /// Converts to the protocol-layer [`ChecksumAlgorithm`](protocol::ChecksumAlgorithm)
    /// for negotiation override.
    ///
    /// Returns `None` for [`Auto`](Self::Auto) since automatic negotiation should not
    /// be overridden.
    pub const fn to_protocol_algorithm(self) -> Option<protocol::ChecksumAlgorithm> {
        match self {
            StrongChecksumAlgorithm::Auto => None,
            StrongChecksumAlgorithm::Md4 => Some(protocol::ChecksumAlgorithm::MD4),
            StrongChecksumAlgorithm::Md5 => Some(protocol::ChecksumAlgorithm::MD5),
            StrongChecksumAlgorithm::Sha1 => Some(protocol::ChecksumAlgorithm::SHA1),
            StrongChecksumAlgorithm::Xxh64 => Some(protocol::ChecksumAlgorithm::XXH64),
            StrongChecksumAlgorithm::Xxh3 => Some(protocol::ChecksumAlgorithm::XXH3),
            StrongChecksumAlgorithm::Xxh128 => Some(protocol::ChecksumAlgorithm::XXH128),
        }
    }
}

/// Resolved checksum-choice configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StrongChecksumChoice {
    transfer: StrongChecksumAlgorithm,
    file: StrongChecksumAlgorithm,
}

impl StrongChecksumChoice {
    /// Parses a `--checksum-choice` argument and resolves the negotiated algorithms.
    pub fn parse(text: &str) -> Result<Self, Message> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(rsync_error!(
                1,
                "invalid --checksum-choice value '': value must name a checksum algorithm"
            )
            .with_role(Role::Client));
        }

        let mut parts = trimmed.splitn(2, ',');
        // SAFETY: splitn on non-empty string always yields at least one element
        let transfer = Self::parse_single(
            parts
                .next()
                .expect("splitn on non-empty string yields at least one element"),
        )?;
        let file = match parts.next() {
            Some(part) => Self::parse_single(part)?,
            None => transfer,
        };

        Ok(Self { transfer, file })
    }

    fn parse_single(label: &str) -> Result<StrongChecksumAlgorithm, Message> {
        let normalized = label.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "auto" => Ok(StrongChecksumAlgorithm::Auto),
            "md4" => Ok(StrongChecksumAlgorithm::Md4),
            "md5" => Ok(StrongChecksumAlgorithm::Md5),
            "sha1" => Ok(StrongChecksumAlgorithm::Sha1),
            "xxh64" | "xxhash" => Ok(StrongChecksumAlgorithm::Xxh64),
            "xxh3" | "xxh3-64" => Ok(StrongChecksumAlgorithm::Xxh3),
            "xxh128" | "xxh3-128" => Ok(StrongChecksumAlgorithm::Xxh128),
            _ => Err(rsync_error!(
                1,
                format!("invalid --checksum-choice value '{normalized}': unsupported checksum")
            )
            .with_role(Role::Client)),
        }
    }

    /// Returns the transfer-algorithm selection (first component).
    #[must_use]
    pub const fn transfer(self) -> StrongChecksumAlgorithm {
        self.transfer
    }

    /// Returns the checksum used for `--checksum` validation (second component).
    #[must_use]
    #[doc(alias = "--checksum-choice")]
    pub const fn file(self) -> StrongChecksumAlgorithm {
        self.file
    }

    /// Resolves the file checksum algorithm into a [`SignatureAlgorithm`].
    #[must_use]
    pub const fn file_signature_algorithm(self) -> SignatureAlgorithm {
        self.file.to_signature_algorithm()
    }

    /// Renders the selection into the canonical argument form accepted by `--checksum-choice`.
    #[must_use]
    pub fn to_argument(self) -> String {
        let transfer = self.transfer.canonical_name();
        let file = self.file.canonical_name();
        if self.transfer == self.file {
            transfer.to_owned()
        } else {
            format!("{transfer},{file}")
        }
    }

    /// Returns the transfer algorithm as a protocol-layer override for negotiation.
    ///
    /// When the transfer algorithm is [`Auto`](StrongChecksumAlgorithm::Auto), returns
    /// `None` to allow automatic negotiation. Otherwise returns the corresponding
    /// [`ChecksumAlgorithm`](protocol::ChecksumAlgorithm) to force during negotiation.
    pub const fn transfer_protocol_override(self) -> Option<protocol::ChecksumAlgorithm> {
        self.transfer.to_protocol_algorithm()
    }
}

impl Default for StrongChecksumChoice {
    fn default() -> Self {
        Self {
            transfer: StrongChecksumAlgorithm::Auto,
            file: StrongChecksumAlgorithm::Auto,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod strong_checksum_algorithm_tests {
        use super::*;

        #[test]
        fn canonical_names() {
            assert_eq!(StrongChecksumAlgorithm::Auto.canonical_name(), "auto");
            assert_eq!(StrongChecksumAlgorithm::Md4.canonical_name(), "md4");
            assert_eq!(StrongChecksumAlgorithm::Md5.canonical_name(), "md5");
            assert_eq!(StrongChecksumAlgorithm::Sha1.canonical_name(), "sha1");
            assert_eq!(StrongChecksumAlgorithm::Xxh64.canonical_name(), "xxh64");
            assert_eq!(StrongChecksumAlgorithm::Xxh3.canonical_name(), "xxh3");
            assert_eq!(StrongChecksumAlgorithm::Xxh128.canonical_name(), "xxh128");
        }

        #[test]
        fn to_signature_algorithm() {
            let _ = StrongChecksumAlgorithm::Auto.to_signature_algorithm();
            let _ = StrongChecksumAlgorithm::Md4.to_signature_algorithm();
            let _ = StrongChecksumAlgorithm::Md5.to_signature_algorithm();
            let _ = StrongChecksumAlgorithm::Sha1.to_signature_algorithm();
            let _ = StrongChecksumAlgorithm::Xxh64.to_signature_algorithm();
            let _ = StrongChecksumAlgorithm::Xxh3.to_signature_algorithm();
            let _ = StrongChecksumAlgorithm::Xxh128.to_signature_algorithm();
        }

        #[test]
        fn clone_and_copy() {
            let alg = StrongChecksumAlgorithm::Md5;
            let cloned = alg;
            let copied = alg;
            assert_eq!(alg, cloned);
            assert_eq!(alg, copied);
        }

        #[test]
        fn debug_format() {
            assert_eq!(format!("{:?}", StrongChecksumAlgorithm::Auto), "Auto");
            assert_eq!(format!("{:?}", StrongChecksumAlgorithm::Xxh128), "Xxh128");
        }
    }

    mod strong_checksum_choice_tests {
        use super::*;

        #[test]
        fn parse_single_algorithm() {
            let choice = StrongChecksumChoice::parse("md5").unwrap();
            assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Md5);
            assert_eq!(choice.file(), StrongChecksumAlgorithm::Md5);
        }

        #[test]
        fn parse_two_algorithms() {
            let choice = StrongChecksumChoice::parse("xxh3,md5").unwrap();
            assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Xxh3);
            assert_eq!(choice.file(), StrongChecksumAlgorithm::Md5);
        }

        #[test]
        fn parse_with_whitespace() {
            let choice = StrongChecksumChoice::parse("  sha1  ").unwrap();
            assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Sha1);
        }

        #[test]
        fn parse_xxhash_alias() {
            let choice = StrongChecksumChoice::parse("xxhash").unwrap();
            assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Xxh64);
        }

        #[test]
        fn parse_xxh3_64_alias() {
            let choice = StrongChecksumChoice::parse("xxh3-64").unwrap();
            assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Xxh3);
        }

        #[test]
        fn parse_xxh3_128_alias() {
            let choice = StrongChecksumChoice::parse("xxh3-128").unwrap();
            assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Xxh128);
        }

        #[test]
        fn parse_empty_returns_error() {
            assert!(StrongChecksumChoice::parse("").is_err());
        }

        #[test]
        fn parse_invalid_returns_error() {
            assert!(StrongChecksumChoice::parse("invalid").is_err());
        }

        #[test]
        fn default_is_auto() {
            let choice = StrongChecksumChoice::default();
            assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Auto);
            assert_eq!(choice.file(), StrongChecksumAlgorithm::Auto);
        }

        #[test]
        fn to_argument_same_algorithm() {
            let choice = StrongChecksumChoice::parse("md5").unwrap();
            assert_eq!(choice.to_argument(), "md5");
        }

        #[test]
        fn to_argument_different_algorithms() {
            let choice = StrongChecksumChoice::parse("xxh3,md5").unwrap();
            assert_eq!(choice.to_argument(), "xxh3,md5");
        }

        #[test]
        fn file_signature_algorithm() {
            let choice = StrongChecksumChoice::parse("md5").unwrap();
            let _ = choice.file_signature_algorithm();
        }
    }
}
