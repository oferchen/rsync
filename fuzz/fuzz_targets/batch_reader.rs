#![no_main]

//! Fuzz target for the admin-facing batch file reader.
//!
//! `--read-batch` is an administrative entry point: the operator hands the
//! tool an opaque binary file written by a previous transfer and the reader
//! must safely parse the header (magic, protocol version, compat flags,
//! checksum seed, stream flags) followed by an unstructured tail of file
//! list and delta operation bytes. A panic here lets a tampered batch file
//! crash the admin's replay session, so we fuzz the header parser and the
//! immediate downstream `read_data` / `read_exact` paths.
//!
//! Upstream reference: `batch.c` - batch file body is a raw protocol-stream
//! tee, framed by a small custom header.
//!
//! # Oracle
//!
//! Beyond panic-freedom this target round-trips the batch header. The header
//! encoding is canonical for a fixed protocol version, so re-encoding a
//! parsed [`BatchHeader`] and decoding it again must reproduce an equal
//! header (an encode/decode fixpoint). A decode failure on the reader's own
//! output is itself a finding, not a silent return - it would mean the
//! writer emits bytes the reader cannot accept.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run batch_reader
//! ```

use std::io::{Cursor, Write};

use libfuzzer_sys::fuzz_target;

use batch::{BatchConfig, BatchHeader, BatchMode, BatchReader};

fuzz_target!(|data: &[u8]| {
    // Header round-trip: parse a header off the raw bytes, then assert the
    // writer/reader pair is a fixpoint on it. `BatchHeader::read_from`
    // adopts the protocol version from the wire, so this exercises both the
    // pre-30 (no compat-flags varint) and post-30 header shapes.
    let mut header_cursor = Cursor::new(data);
    if let Ok(header) = BatchHeader::read_from(&mut header_cursor) {
        let mut reencoded = Vec::new();
        header
            .write_to(&mut reencoded)
            .expect("writing a parsed header into a Vec cannot fail");
        let mut verify_cursor = Cursor::new(reencoded.as_slice());
        let reparsed = BatchHeader::read_from(&mut verify_cursor)
            .expect("re-decoding a self-encoded batch header must succeed");
        assert_eq!(
            header, reparsed,
            "batch header did not round-trip: {header:?}",
        );
    }

    // BatchReader::new opens the path on disk, so we materialise the fuzzer
    // input as a temporary file and feed its path through the public API.
    let Ok(dir) = tempfile::tempdir() else {
        return;
    };
    let batch_path = dir.path().join("fuzz_batch");
    {
        let Ok(mut file) = std::fs::File::create(&batch_path) else {
            return;
        };
        if file.write_all(data).is_err() {
            return;
        }
    }

    let Some(path_str) = batch_path.to_str() else {
        return;
    };

    // Protocol 31 is the modal version emitted by upstream rsync 3.4.x and
    // exercises both pre-30 and post-30 codec branches inside the reader.
    let config = BatchConfig::new(BatchMode::Read, path_str.to_owned(), 31);
    let Ok(mut reader) = BatchReader::new(config) else {
        return;
    };

    // Header parsing is the primary attack surface - it validates magic
    // bytes, protocol version range, and stream-flag bit layout.
    if reader.read_header().is_err() {
        return;
    }

    // If the header validated, fuzz the immediate downstream reads. Errors
    // are expected on malformed tails; only panics constitute a finding.
    let mut buf = [0u8; 64];
    while let Ok(n) = reader.read_data(&mut buf) {
        if n == 0 {
            break;
        }
    }
});
