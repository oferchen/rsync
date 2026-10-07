//! Internationalized domain name (IDN) host conversion.
//!
//! Upstream folds the non-ASCII labels of a host name to their IDNA A-label
//! (Punycode) form at three places: the daemon socket connect
//! (socket.c:346-353 `open_socket_out()`), the host handed to the remote shell
//! for a daemon-over-rsh connection (main.c:527-536 `do_cmd()`), and every
//! `hosts allow`/`hosts deny` token before it is matched (access.c:48-54
//! `match_hostname()`). All three call the one helper, util1.c:951-1022
//! `idn_to_ascii()`, and so do oc's sites through [`host_to_ascii`](crate::idn::host_to_ascii).
//!
//! Upstream links libidn2 for this and compiles it out under `--disable-idn`
//! (configure.ac:637-655). oc uses the pure-Rust `idna` crate behind the `idn`
//! cargo feature, on by default; with the feature off every host is kept as
//! typed and `--version` reports `no IDN`, as a `--disable-idn` build does.
//!
//! # Divergence from libidn2
//!
//! Upstream calls libidn2 with `IDN2_NFC_INPUT | IDN2_NONTRANSITIONAL`
//! (util1.c:989-993): UTS #46 non-transitional mapping, NFC, then the IDNA2008
//! label rules. The `idna` crate runs UTS #46 non-transitional processing with
//! NFC and the bidi, joiner and hyphen checks, which this module enables to
//! match. It has no IDNA2008 code-point table, so a character that UTS #46
//! accepts but IDNA2008 disallows is converted here where libidn2 refuses it
//! and upstream keeps the name as typed. Measured against upstream 3.5.1 built
//! with libidn2 2.3.8: U+2603 SNOWMAN and U+1F600 become `xn--n3h` and
//! `xn--e28h`, and U+00BD (mapped to `1` U+2044 `2`) becomes `xn--12-c6t`.
//! Such a name either way reaches the resolver as something no
//! registry will have delegated. Two narrower differences follow from oc
//! reading arguments as UTF-8: upstream decodes a command-line host from the
//! locale's charset (`idn2_lookup_ul`), and a host that is not valid UTF-8
//! never reaches this module.

use std::borrow::Cow;

/// Returns `name` with each non-ASCII label folded to its IDNA A-label, or
/// `name` unchanged when it needs no conversion or cannot be converted.
///
/// Mirrors upstream util1.c:951-1022 `idn_to_ascii()`:
///
/// - Labels are split on the ASCII `.` only. An all-ASCII label is copied
///   verbatim, so an address, a mask, an `xn--` name, mixed case and any
///   wildmatch characters come through exactly as typed.
/// - A non-ASCII label is mapped with UTS #46 non-transitional processing and
///   NFC, so a decomposed spelling folds to the same A-label as its composed
///   one.
/// - A converted label is used only when it comes back as a bare A-label of
///   `[-a-z0-9]`. The IDNA mapping folds some characters onto ASCII (U+FF0A
///   FULLWIDTH ASTERISK becomes `*`), and a `hosts allow` token must not gain a
///   wildcard or address syntax its author never typed.
/// - Any failure, or a result longer than upstream's 1024-byte buffer, keeps
///   the whole name as typed, which fails to resolve or match instead of
///   matching too much.
///
/// Without the `idn` feature every name is returned unchanged, as upstream
/// built with `--disable-idn` does.
#[must_use]
pub fn host_to_ascii(name: &str) -> Cow<'_, str> {
    #[cfg(feature = "idn")]
    if let Some(converted) = labels_to_ascii(name) {
        return Cow::Owned(converted);
    }
    Cow::Borrowed(name)
}

/// Whether this build converts IDN hosts, reported as the `IDN` capability by
/// `--version`.
///
/// upstream: usage.c:159-162 prints `no IDN` when built without `SUPPORT_IDN`.
pub const SUPPORTED: bool = cfg!(feature = "idn");

/// Size of the fixed buffer every upstream call site hands to
/// `idn_to_ascii()` (`char idn_host[1024]`, socket.c:347; main.c:528;
/// access.c:37). A result that does not fit keeps the name as typed.
#[cfg(feature = "idn")]
const HOST_BUF_LEN: usize = 1024;

/// Size of upstream's per-label scratch buffer (`char label[256]`,
/// util1.c:973). A longer non-ASCII label keeps the name as typed.
#[cfg(feature = "idn")]
const LABEL_BUF_LEN: usize = 256;

/// Converts every non-ASCII label of `name`, returning `None` when nothing was
/// converted or any label refuses conversion.
///
/// upstream: util1.c:966-1021 - the `+ 2` reserves room for the following `.`
/// or the terminating NUL.
#[cfg(feature = "idn")]
fn labels_to_ascii(name: &str) -> Option<String> {
    let mut out = String::with_capacity(name.len());
    let mut converted = false;

    for (index, label) in name.split('.').enumerate() {
        if index > 0 {
            out.push('.');
        }
        if label.is_ascii() {
            out.push_str(label);
        } else {
            if label.len() >= LABEL_BUF_LEN {
                return None;
            }
            let a_label = label_to_ascii(label).filter(|a| is_a_label(a))?;
            out.push_str(&a_label);
            converted = true;
        }
        if out.len() + 2 > HOST_BUF_LEN {
            return None;
        }
    }

    converted.then_some(out)
}

/// Runs UTS #46 ToASCII on one label with the options closest to upstream's
/// `IDN2_NFC_INPUT | IDN2_NONTRANSITIONAL` (util1.c:993).
///
/// - `AsciiDenyList::EMPTY`: libidn2 applies the STD3 ASCII rules only under
///   `IDN2_USE_STD3_ASCII_RULES`, which upstream does not pass. Any ASCII the
///   mapping produces outside `[-a-z0-9]` is refused by [`is_a_label`].
/// - `Hyphens::Check`: IDNA2008 forbids a leading or trailing hyphen and `--`
///   in the third and fourth positions of a U-label, as libidn2 enforces.
/// - `DnsLength::Verify`: libidn2 refuses an A-label over 63 octets.
#[cfg(feature = "idn")]
fn label_to_ascii(label: &str) -> Option<Cow<'_, str>> {
    use idna::uts46::{AsciiDenyList, DnsLength, Hyphens, Uts46};

    Uts46::new()
        .to_ascii(
            label.as_bytes(),
            AsciiDenyList::EMPTY,
            Hyphens::Check,
            DnsLength::Verify,
        )
        .ok()
}

/// Returns whether `label` holds nothing but the `[-a-z0-9]` of an A-label.
///
/// upstream: util1.c:936-948 `is_a_label()`.
#[cfg(feature = "idn")]
fn is_a_label(label: &str) -> bool {
    !label.is_empty()
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ASCII host must reach the resolver byte for byte, case included:
    /// upstream copies an ASCII label verbatim (util1.c:983-987), and the UTS
    /// cell `idn` asserts `XN--IKU-EQAB.Example` survives unchanged.
    #[test]
    fn ascii_host_is_borrowed_unchanged() {
        for host in [
            "localhost",
            "XN--IKU-EQAB.Example",
            "xn--0.example",
            "127.0.0.1",
            "a*.example",
            "",
        ] {
            assert!(matches!(host_to_ascii(host), Cow::Borrowed(h) if h == host));
        }
    }

    #[cfg(feature = "idn")]
    mod with_idn {
        use super::*;

        const A_LABEL: &str = "xn--iku-eqab.example";

        /// The headline conversion: a U-label becomes its A-label, so the
        /// resolver, which only speaks ASCII, can find the host.
        #[test]
        fn u_label_becomes_a_label() {
            assert_eq!(host_to_ascii("\u{10c}i\u{10d}ku.example"), A_LABEL);
        }

        /// Only the non-ASCII label is rewritten; the ASCII label keeps its
        /// case because upstream never touches it (UTS `idn`:
        /// `ČIČKU.Example` -> `xn--iku-eqab.Example`).
        #[test]
        fn only_non_ascii_labels_are_rewritten() {
            assert_eq!(
                host_to_ascii("\u{10c}I\u{10c}KU.Example"),
                "xn--iku-eqab.Example"
            );
        }

        /// A decomposed spelling is canonically equivalent, so NFC must fold
        /// it to the same A-label (upstream's `IDN2_NFC_INPUT`).
        #[test]
        fn decomposed_spelling_folds_to_the_same_a_label() {
            assert_eq!(host_to_ascii("c\u{30c}ic\u{30c}ku.example"), A_LABEL);
            assert_eq!(
                host_to_ascii("C\u{30c}IC\u{30c}KU.Example"),
                "xn--iku-eqab.Example"
            );
        }

        /// Fullwidth forms map back onto ASCII, which is how a non-ASCII
        /// `hosts allow` token can name a host the resolver reports in ASCII.
        #[test]
        fn fullwidth_label_folds_to_ascii() {
            assert_eq!(
                host_to_ascii("\u{ff4c}\u{ff4f}\u{ff43}\u{ff41}\u{ff4c}"),
                "local"
            );
            assert_eq!(host_to_ascii("\u{ff2c}\u{ff2f}.example"), "lo.example");
        }

        /// A mapping that yields ASCII outside `[-a-z0-9]` must keep the name
        /// as typed: a fullwidth `*` or `/` would otherwise widen a
        /// `hosts allow` entry into a wildcard or an address/mask.
        #[test]
        fn mapping_onto_metacharacters_keeps_the_name() {
            for host in [
                "\u{ff0a}",
                "\u{ff0a}.example",
                "127.0.0.0\u{ff0f}8",
                "a\u{ff0e}b.example",
            ] {
                assert!(matches!(host_to_ascii(host), Cow::Borrowed(h) if h == host));
            }
        }

        /// Invalid input is kept as typed, as upstream does when libidn2
        /// refuses a label, rather than being rewritten into another name.
        #[test]
        fn unconvertible_label_keeps_the_name() {
            for host in [
                // Maps to nothing, leaving an empty label.
                "\u{200b}.example",
                // ARABIC TATWEEL then a Latin letter breaks the bidi rule.
                "\u{640}x.example",
                // A U-label may not end with a hyphen.
                "\u{e9}-.example",
            ] {
                assert!(matches!(host_to_ascii(host), Cow::Borrowed(h) if h == host));
            }
        }

        /// A failure in any label keeps the whole name, not just that label.
        #[test]
        fn one_bad_label_keeps_every_label() {
            let host = "\u{10d}i\u{10d}ku.\u{200b}.example";
            assert!(matches!(host_to_ascii(host), Cow::Borrowed(h) if h == host));
        }

        /// upstream: util1.c:994 - a non-ASCII label of 256 bytes or more does
        /// not fit the label buffer and keeps the name. Code points the
        /// mapping drops (U+200B) let a label that long still convert to a
        /// short A-label, so the size check is the only thing refusing it.
        #[test]
        fn oversized_label_keeps_the_name() {
            let fits = format!("\u{e9}{}.example", "\u{200b}".repeat(84));
            assert_eq!(host_to_ascii(&fits), "xn--9ca.example");
            let oversized = format!("\u{e9}{}.example", "\u{200b}".repeat(85));
            assert!(matches!(host_to_ascii(&oversized), Cow::Borrowed(_)));
        }

        /// upstream: socket.c:347 - a result that does not fit the 1024-byte
        /// buffer keeps the name, while one that fits is converted.
        #[test]
        fn result_must_fit_the_upstream_buffer() {
            let fits = format!("{}.{}", "a".repeat(1000), "\u{e9}");
            assert!(matches!(host_to_ascii(&fits), Cow::Owned(_)));
            let overflows = format!("{}.{}", "a".repeat(1020), "\u{e9}");
            assert!(matches!(host_to_ascii(&overflows), Cow::Borrowed(_)));
        }

        #[test]
        fn is_a_label_accepts_only_lowercase_ldh() {
            assert!(is_a_label("xn--iku-eqab"));
            assert!(is_a_label("0-9"));
            assert!(!is_a_label(""));
            assert!(!is_a_label("Xn--iku"));
            assert!(!is_a_label("*"));
            assert!(!is_a_label("a.b"));
        }
    }

    #[cfg(not(feature = "idn"))]
    #[test]
    fn non_ascii_host_is_kept_without_the_feature() {
        let host = "\u{10c}i\u{10d}ku.example";
        assert!(matches!(host_to_ascii(host), Cow::Borrowed(h) if h == host));
        assert!(!SUPPORTED);
    }
}
