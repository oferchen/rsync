#![no_main]

//! Fuzz target for the `rsyncd.conf` daemon configuration parser.
//!
//! `RsyncdConfig::parse` is the entry point for parsing the daemon admin
//! configuration file. Although the file itself is owned by the operator,
//! the parser is also reachable pre-auth from daemon startup paths and
//! drives module access decisions, so any panic on malformed input would
//! either crash the daemon or expose a denial-of-service vector when a
//! corrupted config is reloaded. The parser walks line-by-line state
//! transitions across global parameters and `[module]` sections, so byte
//! coverage flushes out boundary bugs in section dispatch, key/value
//! splitting, and list / boolean parameter decoding.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run rsyncd_conf
//! ```

use std::path::Path;

use libfuzzer_sys::fuzz_target;

use daemon::rsyncd_config::RsyncdConfig;

fuzz_target!(|data: &[u8]| {
    // The parser operates on `&str`, so reject non-UTF-8 inputs early.
    // libFuzzer will still drive the byte space toward valid UTF-8 thanks
    // to coverage feedback on the conversion path.
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    let path = Path::new("fuzz-rsyncd.conf");
    let _ = RsyncdConfig::parse(text, path);
});
