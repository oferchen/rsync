#![no_main]

//! Fuzz target for the `--bwlimit` CLI string parser.
//!
//! `parse_bandwidth_argument` and `parse_bandwidth_limit` are the public
//! entry points used by the CLI and daemon config to decode the
//! `RATE[:BURST]` syntax accepted by upstream rsync's
//! `util2.c:parse_size_arg()`. Both consume untrusted user input from
//! the command line and `oc-rsyncd.conf`, so any panic on arbitrary
//! UTF-8 is a usability / DoS finding.
//!
//! # Oracle
//!
//! `parse_bandwidth_limit` is defined to delegate its rate component to
//! `parse_bandwidth_argument` whenever the input carries no `:` burst
//! separator. This target treats `parse_bandwidth_argument` as the
//! reference implementation and asserts the delegation contract: for any
//! input with no `:` and no surrounding whitespace (the two cases
//! `parse_bandwidth_limit` handles specially before delegating), the two
//! parsers must reach the same verdict - the same rate on success and the
//! same error kind on failure. A wrong-but-non-panicking rate (say, a
//! quantization applied on one path but not the other) fails the assert
//! that a discard-the-result target would miss.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run bwlimit
//! ```
//!
//! # Reference
//!
//! - FCV-15 audit (#2442) - `--bwlimit` flagged as a CLI string parsing
//!   gap with no fuzz coverage.
//! - upstream: util2.c:parse_size_arg()

use libfuzzer_sys::fuzz_target;

use bandwidth::{parse_bandwidth_argument, parse_bandwidth_limit};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    let argument = parse_bandwidth_argument(text);
    let limit = parse_bandwidth_limit(text);

    // The differential only holds on the delegation path: no `:` separator
    // and no surrounding whitespace (which `parse_bandwidth_limit` rejects
    // up front while `parse_bandwidth_argument` tolerates). Outside that
    // path the two parsers legitimately diverge, so the oracle stays silent
    // and the call above still exercises both for panic-freedom.
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    if text.contains(':') || trimmed.len() != text.len() {
        return;
    }

    match (argument, limit) {
        (Ok(rate), Ok(components)) => assert_eq!(
            rate,
            components.rate(),
            "bwlimit rate parsers disagree on {text:?}",
        ),
        (Err(arg_err), Err(limit_err)) => assert_eq!(
            arg_err, limit_err,
            "bwlimit parsers disagree on error for {text:?}",
        ),
        (argument, limit) => panic!(
            "bwlimit parsers disagree on success for {text:?}: \
             argument_ok={} limit_ok={}",
            argument.is_ok(),
            limit.is_ok(),
        ),
    }
});
