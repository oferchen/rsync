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
//! This target feeds raw fuzzer bytes (after a UTF-8 conversion) into
//! both parsers. `parse_bandwidth_limit` exercises the colon-separated
//! `RATE:BURST` split that `parse_bandwidth_argument` does not see, so
//! both functions are driven independently to maximise coverage of the
//! suffix, fractional, exponent, and adjustment branches.
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

    let _ = parse_bandwidth_argument(text);
    let _ = parse_bandwidth_limit(text);
});
