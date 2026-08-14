use crate::{
    message::{Message, Role},
    rsync_error,
};
use engine::signature::SignatureAlgorithm;

/// Enumerates the strong checksum algorithms recognised by the client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrongChecksumAlgorithm {
    /// Automatically selects the negotiated algorithm.
    ///
    /// Mirrors upstream `checksum.c` negotiation (`negotiate_the_strings`,
    /// `parse_checksum_choice`): when no explicit `--checksum-choice` is given
    /// and both peers advertise it, the strongest mutually supported checksum
    /// wins. For a local copy - where upstream still negotiates over the forked
    /// child's pipe with `do_negotiated_strings` set (protocol 31+) - that is
    /// `xxh128` (`CSUM_XXH3_128`). MD5/MD4 remain the fallback for peers too old
    /// to negotiate, reached via an explicit `--checksum-choice`.
    Auto,
    /// No transfer checksum; disables delta and forces whole-file transfers.
    ///
    /// Mirrors upstream `CSUM_NONE` (see `checksum.c:63`). When selected as the
    /// transfer algorithm, upstream `checksum.c:197-198` unconditionally sets
    /// `whole_file = 1`.
    None,
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
            // `auto` resolves to the strongest mutually negotiated checksum,
            // which for a modern (protocol 31+) transfer is xxh128. Upstream
            // `parse_checksum_choice` sets `file_sum_nni` to the negotiated
            // `valid_checksums.negotiated_nni`, whose ordering lists
            // `CSUM_XXH3_128` first (`checksum.c`).
            StrongChecksumAlgorithm::Auto => SignatureAlgorithm::Xxh3_128 { seed: 0 },
            StrongChecksumAlgorithm::Md5 => SignatureAlgorithm::Md5 {
                seed_config: Md5Seed::none(),
            },
            // `none` disables delta (whole_file is forced upstream), but a
            // signature algorithm is still required by the engine's type
            // signature. MD4 matches upstream's default fallback semantics
            // and is never actually computed when whole-file is in effect.
            StrongChecksumAlgorithm::None | StrongChecksumAlgorithm::Md4 => SignatureAlgorithm::Md4,
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
            StrongChecksumAlgorithm::None => "none",
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
            StrongChecksumAlgorithm::None => Some(protocol::ChecksumAlgorithm::None),
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
            // upstream: checksum.c:139 parse_checksum_choice returns
            // RERR_UNSUPPORTED (errcode.h:28) for an unusable name, not RERR_SYNTAX.
            return Err(rsync_error!(
                4,
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
            "none" => Ok(StrongChecksumAlgorithm::None),
            "md4" => Ok(StrongChecksumAlgorithm::Md4),
            "md5" => Ok(StrongChecksumAlgorithm::Md5),
            "sha1" => Ok(StrongChecksumAlgorithm::Sha1),
            "xxh64" | "xxhash" => Ok(StrongChecksumAlgorithm::Xxh64),
            "xxh3" | "xxh3-64" => Ok(StrongChecksumAlgorithm::Xxh3),
            "xxh128" | "xxh3-128" => Ok(StrongChecksumAlgorithm::Xxh128),
            // upstream: checksum.c:139 parse_checksum_choice returns
            // RERR_UNSUPPORTED (errcode.h:28) for an unknown name, not RERR_SYNTAX.
            _ => Err(rsync_error!(
                4,
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
    ///
    /// upstream: checksum.c:178-189 parse_checksum_choice - `file_sum_nni`
    /// comes from the second comma component. An `auto` sub-component resolves
    /// via `parse_csum_name` to the implied checksum, which is md5 at proto>=30
    /// (checksum.c:118-122), but ONLY when the choice is "set". The fully-auto
    /// forms (`auto` / `auto,auto`) null `checksum_choice` (options.c:1997-2003)
    /// and negotiate the strongest mutually supported checksum instead, which
    /// for a modern local copy is xxh128. Mirror that split: resolve a lone
    /// `auto` file sub-component to md5 on the suppressed path, and fall back to
    /// the negotiated xxh128 only when both components are auto.
    #[must_use]
    pub const fn file_signature_algorithm(self) -> SignatureAlgorithm {
        use checksums::strong::Md5Seed;
        match (self.transfer, self.file) {
            (StrongChecksumAlgorithm::Auto, StrongChecksumAlgorithm::Auto) => {
                SignatureAlgorithm::Xxh3_128 { seed: 0 }
            }
            (_, StrongChecksumAlgorithm::Auto) => SignatureAlgorithm::Md5 {
                seed_config: Md5Seed::none(),
            },
            (_, file) => file.to_signature_algorithm(),
        }
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

    /// Reports whether the transfer algorithm is the `none` sentinel.
    ///
    /// Upstream `checksum.c:216-217` forces `whole_file = 1` whenever the
    /// negotiated transfer checksum is `CSUM_NONE`. The config builder uses
    /// this to promote `whole_file` at build time so the delta pipeline is
    /// never engaged when the user explicitly disables the transfer
    /// checksum.
    #[must_use]
    pub const fn transfer_is_none(self) -> bool {
        matches!(self.transfer, StrongChecksumAlgorithm::None)
    }

    /// Returns the transfer-checksum override that drives protocol 30+
    /// negotiation, mirroring upstream's send-gate and `auto` resolution.
    ///
    /// upstream: options.c:1997-2003 nulls `checksum_choice` only when the raw
    /// string is exactly `auto` or `auto,auto`; any other value stays set and
    /// suppresses the vstring exchange (compat.c:541 `if (!checksum_choice)`).
    /// On that suppressed path `parse_checksum_choice` resolves the transfer
    /// (first) component with `parse_csum_name`, where `auto` becomes the
    /// implied md5 at proto>=30 (checksum.c:118-122). Returns `None` only for
    /// the fully-auto choice so the negotiator's `send_checksum` gate
    /// (`checksum_override.is_none()`) stays true and the exchange picks the
    /// strongest mutual checksum (xxh128 for a modern peer). Every set choice
    /// returns `Some`, suppressing the exchange and forcing the resolved
    /// transfer checksum.
    pub const fn transfer_protocol_override(self) -> Option<protocol::ChecksumAlgorithm> {
        match (self.transfer, self.file) {
            (StrongChecksumAlgorithm::Auto, StrongChecksumAlgorithm::Auto) => None,
            (StrongChecksumAlgorithm::Auto, _) => Some(protocol::ChecksumAlgorithm::MD5),
            (transfer, _) => transfer.to_protocol_algorithm(),
        }
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
            assert_eq!(StrongChecksumAlgorithm::None.canonical_name(), "none");
        }

        #[test]
        fn to_signature_algorithm() {
            let _ = StrongChecksumAlgorithm::Auto.to_signature_algorithm();
            let _ = StrongChecksumAlgorithm::None.to_signature_algorithm();
            let _ = StrongChecksumAlgorithm::Md4.to_signature_algorithm();
            let _ = StrongChecksumAlgorithm::Md5.to_signature_algorithm();
            let _ = StrongChecksumAlgorithm::Sha1.to_signature_algorithm();
            let _ = StrongChecksumAlgorithm::Xxh64.to_signature_algorithm();
            let _ = StrongChecksumAlgorithm::Xxh3.to_signature_algorithm();
            let _ = StrongChecksumAlgorithm::Xxh128.to_signature_algorithm();
        }

        #[test]
        fn auto_resolves_to_xxh128() {
            // upstream: checksum.c negotiate_the_strings/parse_checksum_choice -
            // the negotiated (auto) checksum is the strongest mutually supported,
            // which for a modern local copy is xxh128, not md5.
            assert_eq!(
                StrongChecksumAlgorithm::Auto.to_signature_algorithm(),
                SignatureAlgorithm::Xxh3_128 { seed: 0 }
            );
        }

        #[test]
        fn explicit_md5_override_is_preserved() {
            // An explicit --checksum-choice=md5 must still force MD5, not xxh128.
            assert_eq!(
                StrongChecksumAlgorithm::Md5.to_signature_algorithm(),
                SignatureAlgorithm::Md5 {
                    seed_config: checksums::strong::Md5Seed::none(),
                }
            );
        }

        #[test]
        fn explicit_md4_override_is_preserved() {
            assert_eq!(
                StrongChecksumAlgorithm::Md4.to_signature_algorithm(),
                SignatureAlgorithm::Md4
            );
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
        fn parse_none() {
            // upstream: checksum.c:63 - { CSUM_NONE, 0, "none", NULL }.
            let choice = StrongChecksumChoice::parse("none").unwrap();
            assert_eq!(choice.transfer(), StrongChecksumAlgorithm::None);
            assert_eq!(choice.file(), StrongChecksumAlgorithm::None);
            assert!(choice.transfer_is_none());
            assert_eq!(choice.to_argument(), "none");
            assert_eq!(
                choice.transfer_protocol_override(),
                Some(protocol::ChecksumAlgorithm::None),
            );
        }

        #[test]
        fn transfer_is_none_false_for_other_algorithms() {
            assert!(
                !StrongChecksumChoice::parse("md5")
                    .unwrap()
                    .transfer_is_none()
            );
            assert!(!StrongChecksumChoice::default().transfer_is_none());
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
            assert_eq!(
                choice.file_signature_algorithm(),
                SignatureAlgorithm::Md5 {
                    seed_config: checksums::strong::Md5Seed::none(),
                }
            );
        }

        #[test]
        fn default_file_signature_algorithm_is_xxh128() {
            // The default (Auto) --checksum whole-file compare uses xxh128,
            // matching upstream's negotiated strongest checksum for a local copy.
            let choice = StrongChecksumChoice::default();
            assert_eq!(
                choice.file_signature_algorithm(),
                SignatureAlgorithm::Xxh3_128 { seed: 0 }
            );
        }

        #[test]
        fn explicit_md5_file_signature_algorithm_is_md5() {
            let choice = StrongChecksumChoice::parse("md5").unwrap();
            assert_eq!(
                choice.file_signature_algorithm(),
                SignatureAlgorithm::Md5 {
                    seed_config: checksums::strong::Md5Seed::none(),
                }
            );
        }

        // upstream: options.c:1997-2003 + compat.c:541 - "auto,md5" keeps
        // checksum_choice non-null, so the vstring exchange is SUPPRESSED
        // (send_checksum = checksum_override.is_none() in negotiate.rs). The
        // transfer "auto" resolves to implied md5 (checksum.c:118-122) and the
        // file component is the explicit md5. WHY: returning None here would
        // re-enable the vstring and desync against an upstream peer that sends
        // nothing; resolving to xxh128 would compute the wrong whole-file sum.
        #[test]
        fn auto_md5_suppresses_negotiation_and_resolves_both_to_md5() {
            let choice = StrongChecksumChoice::parse("auto,md5").unwrap();
            assert_eq!(
                choice.transfer_protocol_override(),
                Some(protocol::ChecksumAlgorithm::MD5),
                "auto,md5 must force md5 and suppress the vstring (override.is_some())",
            );
            assert_eq!(
                choice.file_signature_algorithm(),
                SignatureAlgorithm::Md5 {
                    seed_config: checksums::strong::Md5Seed::none(),
                },
                "auto,md5 file sum is the explicit md5, not xxh128",
            );
        }

        // upstream: checksum.c:178-189 - the SECOND component of "md5,auto" is
        // parsed with parse_csum_name("auto"), which is implied md5 at proto>=30
        // (checksum.c:118-122), NOT the negotiated xxh128. WHY: the choice is
        // "set" (checksum_choice non-null), so the file sum follows the
        // suppressed-path resolution, not the fully-auto negotiated one.
        #[test]
        fn md5_auto_resolves_file_component_to_md5() {
            let choice = StrongChecksumChoice::parse("md5,auto").unwrap();
            assert_eq!(
                choice.transfer_protocol_override(),
                Some(protocol::ChecksumAlgorithm::MD5),
            );
            assert_eq!(
                choice.file_signature_algorithm(),
                SignatureAlgorithm::Md5 {
                    seed_config: checksums::strong::Md5Seed::none(),
                },
                "a lone auto file sub-component on a set choice resolves to md5, not xxh128",
            );
        }

        // upstream: options.c:1997-2003 - ONLY "auto" and "auto,auto" null
        // checksum_choice, leaving send_checksum true so negotiate_the_strings
        // exchanges lists and picks the strongest mutual checksum (xxh128 for a
        // modern local copy). WHY: these two forms must keep override == None,
        // or the negotiation the transfer relies on never runs.
        #[test]
        fn fully_auto_forms_still_negotiate() {
            for text in ["auto", "auto,auto"] {
                let choice = StrongChecksumChoice::parse(text).unwrap();
                assert_eq!(
                    choice.transfer_protocol_override(),
                    None,
                    "{text} must negotiate (override None => send_checksum true)",
                );
                assert_eq!(
                    choice.file_signature_algorithm(),
                    SignatureAlgorithm::Xxh3_128 { seed: 0 },
                    "{text} file sum is the negotiated xxh128",
                );
            }
        }
    }
}
