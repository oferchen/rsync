#![no_main]

//! Fuzz target for filter rule parsing.
//!
//! Exercises `parse_rules` with arbitrary strings to find panics or crashes
//! in the merge-file format parser. The parser handles short-form prefixes
//! (`+`, `-`, `P`, `R`, `.`, `:`, `H`, `S`, `!`) and long-form keywords
//! (`include`, `exclude`, etc.), plus modifier characters.
//!
//! Any input that causes a panic is a bug - parse errors should be returned
//! as `Err(MergeFileError)`, never as a panic.

use libfuzzer_sys::fuzz_target;
use std::path::Path;

fuzz_target!(|data: &[u8]| {
    // parse_rules expects &str, so only test valid UTF-8 slices.
    // Invalid UTF-8 is not reachable through normal rsync filter file reading.
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };

    let source = Path::new("<fuzz>");

    // Must not panic regardless of input content.
    let _ = filters::parse_rules(input, source);

    // If parsing succeeds, the rules must be compilable into a FilterSet
    // without panicking (though glob compilation errors are acceptable).
    if let Ok(rules) = filters::parse_rules(input, source) {
        let _ = filters::FilterSet::from_rules(rules);
    }
});
