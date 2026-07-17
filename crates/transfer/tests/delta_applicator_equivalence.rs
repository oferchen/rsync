//! Equivalence safety gate for `DeltaApplicator`.
//!
//! Proves the (previously unwired) `DeltaApplicator` reconstructs files
//! byte-for-byte identically to the live receiver token loop in
//! `receiver/transfer/sync.rs:518-634`, including the compressed (`-z`)
//! `see_token` dictionary-sync step (upstream `token.c:631`
//! `see_deflate_token()`).
//!
//! For the SAME basis + SAME constructed token stream, each test runs two
//! appliers, then asserts byte-identical output files AND identical
//! `literal_bytes`:
//!
//! 1. a faithful reference applier (`reference_apply`) that drives the same
//!    public primitives the live loop uses - `TokenReader::read_token`,
//!    `ChecksumVerifier`, `MapFile`, `SparseWriteState`, and crucially
//!    `TokenReader::see_token` after every block ref - exactly as
//!    `apply_delta_tokens` does (mirrored line-for-line, cited below), and
//! 2. `DeltaApplicator` + `apply_delta_stream`.

#![deny(unsafe_code)]

use std::fs::{File, OpenOptions};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::num::{NonZeroU8, NonZeroU32};
use std::path::Path;

use checksums::RollingDigest;
use engine::signature::{FileSignature, SignatureBlock};
use protocol::ChecksumAlgorithm;
use protocol::wire::CompressedTokenEncoder;
use signature::SignatureLayout;
use tempfile::{TempDir, tempdir};

use transfer::delta_apply::{
    ChecksumVerifier, DeltaApplicator, DeltaApplyConfig, SparseWriteState, TokenReader,
    apply_delta_stream,
};
use transfer::delta_apply::{DeltaToken, LiteralData};
use transfer::map_file::MapFile;

const BLOCK_LEN: u32 = 700;

/// Builds a deterministic basis payload of `len` bytes.
fn basis_payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| ((i * 37 + 11) % 251) as u8).collect()
}

/// Builds a `FileSignature` over `payload` with `BLOCK_LEN` blocks and a
/// trailing remainder. Strong/rolling sums are not consulted for the COPY
/// path (the block index alone selects the source range), so they are zeroed.
fn make_signature(payload: &[u8]) -> FileSignature {
    let block_len = BLOCK_LEN as usize;
    let file_len = payload.len() as u64;
    let full_blocks = payload.len() / block_len;
    let remainder = (payload.len() % block_len) as u32;
    let block_count = full_blocks as u64 + u64::from(remainder > 0);
    let layout = SignatureLayout::from_raw_parts(
        NonZeroU32::new(BLOCK_LEN).expect("block length nonzero"),
        remainder,
        block_count,
        NonZeroU8::new(16).expect("strong sum length nonzero"),
    );
    let blocks: Vec<SignatureBlock> = (0..block_count)
        .map(|idx| SignatureBlock::from_raw_parts(idx, RollingDigest::ZERO, &[0u8; 16]))
        .collect();
    FileSignature::from_raw_parts(layout, blocks, file_len)
}

/// A single delta operation in the abstract token stream.
#[derive(Clone)]
enum Op {
    Literal(Vec<u8>),
    Block(usize),
}

/// Encodes the op sequence as a plain (uncompressed) token wire stream.
fn encode_plain(ops: &[Op]) -> Vec<u8> {
    let mut wire = Vec::new();
    for op in ops {
        match op {
            Op::Literal(data) => {
                let len = i32::try_from(data.len()).expect("literal len fits i32");
                wire.extend_from_slice(&len.to_le_bytes());
                wire.extend_from_slice(data);
            }
            Op::Block(idx) => {
                let token = -(i32::try_from(*idx).expect("idx fits i32") + 1);
                wire.extend_from_slice(&token.to_le_bytes());
            }
        }
    }
    wire.extend_from_slice(&0_i32.to_le_bytes());
    wire
}

/// Encodes the op sequence as a compressed (`-z`) token wire stream using the
/// real `CompressedTokenEncoder`. The encoder's own `see_token` is fed the
/// basis bytes after each block match so the sender-side deflate dictionary
/// matches what the receiver's inflate dictionary will see - mirroring the
/// generator's encode loop. Without that the compressed stream would not be
/// decodable, which is exactly the invariant the receiver `see_token` upholds.
fn encode_compressed(ops: &[Op], payload: &[u8], block_len: usize) -> Vec<u8> {
    let mut wire = Vec::new();
    let mut encoder = CompressedTokenEncoder::default();
    for op in ops {
        match op {
            Op::Literal(data) => encoder.send_literal(&mut wire, data).expect("send literal"),
            Op::Block(idx) => {
                encoder
                    .send_block_match(&mut wire, *idx as u32)
                    .expect("send block match");
                let start = idx * block_len;
                let end = (start + block_len).min(payload.len());
                encoder.see_token(&payload[start..end]).expect("see token");
            }
        }
    }
    encoder.finish(&mut wire).expect("finish encoder");
    wire
}

/// Protocol version for the equivalence helpers. These tests only exercise
/// MD5, which is never seeded, so any protocol >= 30 leaves the digest
/// unchanged; the reference and applicator paths stay symmetric.
fn proto32() -> protocol::ProtocolVersion {
    protocol::ProtocolVersion::try_from(32u8).expect("protocol 32")
}

/// Appends the trailing per-file checksum the receiver expects.
fn append_checksum(wire: &mut Vec<u8>, algo: ChecksumAlgorithm, seed: i32, output: &[u8]) {
    let mut verifier = ChecksumVerifier::for_algorithm_seeded(algo, seed, proto32());
    verifier.update(output);
    let mut digest = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
    let len = verifier.finalize_into(&mut digest);
    wire.extend_from_slice(&digest[..len]);
}

/// Computes the expected reconstructed output for an op sequence.
fn expected_output(ops: &[Op], payload: &[u8], block_len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for op in ops {
        match op {
            Op::Literal(data) => out.extend_from_slice(data),
            Op::Block(idx) => {
                let start = idx * block_len;
                let end = (start + block_len).min(payload.len());
                out.extend_from_slice(&payload[start..end]);
            }
        }
    }
    out
}

/// Faithful reference re-implementation of the live token loop
/// `apply_delta_tokens` (`receiver/transfer/sync.rs:518-634`). Mirrors it
/// step-for-step so the equivalence assertion compares `DeltaApplicator`
/// against the live behaviour - including the critical
/// `token_reader.see_token(block_data)` after each block ref (sync.rs:629;
/// upstream `token.c:631`).
#[derive(Debug)]
struct ReferenceResult {
    literal_bytes: u64,
    final_pos: Option<u64>,
}

#[allow(clippy::too_many_arguments)]
fn reference_apply<R: Read>(
    reader: &mut R,
    output_path: &Path,
    sparse: bool,
    signature: Option<&FileSignature>,
    basis_path: Option<&Path>,
    token_reader: &mut TokenReader,
    algo: ChecksumAlgorithm,
    seed: i32,
    expected_size: Option<u64>,
) -> std::io::Result<ReferenceResult> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(output_path)?;
    let mut out = std::io::BufWriter::new(file);
    let mut sparse_state = sparse.then(SparseWriteState::new);
    let mut basis_map = match basis_path {
        Some(p) => Some(MapFile::open(p)?),
        None => None,
    };
    let mut verifier = ChecksumVerifier::for_algorithm_seeded(algo, seed, proto32());
    let mut literal_bytes: u64 = 0;

    let write_chunk = |out: &mut std::io::BufWriter<File>,
                       sparse_state: &mut Option<SparseWriteState>,
                       data: &[u8]|
     -> std::io::Result<()> {
        if let Some(s) = sparse_state.as_mut() {
            s.write(out, data)?;
            Ok(())
        } else {
            out.write_all(data)
        }
    };

    loop {
        match token_reader.read_token(reader)? {
            DeltaToken::End => break,
            DeltaToken::Literal(LiteralData::Ready(data)) => {
                let len = data.len();
                write_chunk(&mut out, &mut sparse_state, &data)?;
                verifier.update(&data);
                literal_bytes += len as u64;
            }
            DeltaToken::Literal(LiteralData::Pending(len)) => {
                // The live loop first tries `reader.try_borrow_exact(len)` on
                // its concrete `ServerReader`; that is a zero-copy I/O
                // optimization with byte-identical output, so the reference
                // (and the generic `DeltaApplicator` path) always read into a
                // buffer instead. See sync.rs:565-578.
                let mut buf = vec![0u8; len];
                reader.read_exact(&mut buf)?;
                write_chunk(&mut out, &mut sparse_state, &buf)?;
                verifier.update(&buf);
                literal_bytes += len as u64;
            }
            DeltaToken::BlockRef(block_idx) => {
                let (sig, map) = (signature.unwrap(), basis_map.as_mut().unwrap());
                let layout = sig.layout();
                let block_count = layout.block_count() as usize;
                let block_len = layout.block_length().get() as u64;
                let offset = block_idx as u64 * block_len;
                let bytes_to_copy = if block_idx == block_count.saturating_sub(1) {
                    let remainder = layout.remainder();
                    if remainder > 0 {
                        remainder as usize
                    } else {
                        block_len as usize
                    }
                } else {
                    block_len as usize
                };
                let block_data = map.map_ptr(offset, bytes_to_copy)?;
                write_chunk(&mut out, &mut sparse_state, block_data)?;
                verifier.update(block_data);
                // upstream: token.c:631 see_deflate_token() (sync.rs:629).
                token_reader.see_token(block_data)?;
            }
        }
    }

    // sync.rs:520-555 trailing checksum verification.
    let checksum_len = verifier.digest_len();
    let mut expected_buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
    reader.read_exact(&mut expected_buf[..checksum_len])?;
    let mut computed = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
    let computed_len = verifier.finalize_into(&mut computed);
    if computed[..computed_len] != expected_buf[..checksum_len] {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "checksum verification failed",
        ));
    }

    // sync.rs:276-289 post-finish sparse size check.
    let mut final_pos = None;
    if let Some(ref mut sparse) = sparse_state {
        let pos = sparse.finish(&mut out)?;
        final_pos = Some(pos);
        if let Some(expected) = expected_size {
            if pos != expected {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "sparse file size mismatch",
                ));
            }
        }
    }

    let file = out
        .into_inner()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    // upstream: fileio.c:43 sparse_end() -> do_ftruncate(f, size). finish() only
    // seeks over the trailing hole; the caller establishes the logical length via
    // set_len, mirroring DeltaApplicator. Punching in-basis holes is a block
    // deallocation that reads back as zeros identically to the truncated tail, so
    // set_len alone reproduces the applicator's byte-for-byte output here.
    if let Some(pos) = final_pos {
        file.set_len(pos)?;
    }
    Ok(ReferenceResult {
        literal_bytes,
        final_pos,
    })
}

/// Writes a basis file from `payload`, returns its path.
fn write_basis(dir: &TempDir, payload: &[u8]) -> std::path::PathBuf {
    let path = dir.path().join("basis.bin");
    let mut f = File::create(&path).expect("create basis");
    f.write_all(payload).expect("write basis");
    f.sync_all().ok();
    path
}

fn read_file(path: &Path) -> Vec<u8> {
    let mut f = File::open(path).expect("open");
    f.seek(SeekFrom::Start(0)).expect("seek");
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).expect("read");
    buf
}

/// Drives `DeltaApplicator` + `apply_delta_stream` and returns the result.
#[allow(clippy::too_many_arguments)]
fn applicator_apply(
    output_path: &Path,
    sparse: bool,
    signature: Option<&FileSignature>,
    basis_path: Option<&Path>,
    wire: &[u8],
    token_reader: &mut TokenReader,
    algo: ChecksumAlgorithm,
    seed: i32,
    expected_size: Option<u64>,
) -> std::io::Result<transfer::delta_apply::DeltaApplyResult> {
    let out = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(output_path)?;
    let config = DeltaApplyConfig {
        sparse,
        ..Default::default()
    };
    let verifier = ChecksumVerifier::for_algorithm_seeded(algo, seed, proto32());
    let mut applicator = DeltaApplicator::new(out, &config, verifier, signature, basis_path)?;
    let mut cursor = Cursor::new(wire.to_vec());
    apply_delta_stream(&mut cursor, &mut applicator, token_reader)?;
    // The output File is committed by re-opening the path below; the
    // reconstructed handle is dropped (flushed) here.
    let (_out, result) = applicator.finish(&mut cursor, expected_size)?;
    Ok(result)
}

/// Core equivalence check for a plain (uncompressed) stream.
fn assert_equivalent_plain(ops: &[Op], payload: &[u8], use_basis: bool) {
    let block_len = BLOCK_LEN as usize;
    let algo = ChecksumAlgorithm::MD5;
    let seed = 0;
    let dir = tempdir().expect("tempdir");
    let basis_path = use_basis.then(|| write_basis(&dir, payload));
    let signature = use_basis.then(|| make_signature(payload));
    let expected = expected_output(ops, payload, block_len);

    let mut wire = encode_plain(ops);
    append_checksum(&mut wire, algo, seed, &expected);

    // Live reference.
    let ref_out = dir.path().join("ref.bin");
    let mut server = Cursor::new(wire.clone());
    let mut ref_reader = TokenReader::new(None).expect("plain reader");
    let ref_res = reference_apply(
        &mut server,
        &ref_out,
        false,
        signature.as_ref(),
        basis_path.as_deref(),
        &mut ref_reader,
        algo,
        seed,
        None,
    )
    .expect("reference apply");

    // DeltaApplicator.
    let app_out = dir.path().join("app.bin");
    let mut app_reader = TokenReader::new(None).expect("plain reader");
    let app_res = applicator_apply(
        &app_out,
        false,
        signature.as_ref(),
        basis_path.as_deref(),
        &wire,
        &mut app_reader,
        algo,
        seed,
        None,
    )
    .expect("applicator apply");

    assert_eq!(
        read_file(&ref_out),
        read_file(&app_out),
        "output bytes differ"
    );
    assert_eq!(
        read_file(&app_out),
        expected,
        "output does not match expected"
    );
    assert_eq!(
        ref_res.literal_bytes, app_res.literal_bytes,
        "literal_bytes differ"
    );
}

#[test]
fn equivalence_plain_literal_only() {
    let payload = basis_payload(4096);
    let ops = vec![
        Op::Literal(b"hello world".to_vec()),
        Op::Literal(vec![0xAB; 300]),
    ];
    assert_equivalent_plain(&ops, &payload, false);
}

#[test]
fn equivalence_plain_copy_only() {
    let payload = basis_payload(BLOCK_LEN as usize * 3);
    let ops = vec![Op::Block(0), Op::Block(1), Op::Block(2)];
    assert_equivalent_plain(&ops, &payload, true);
}

#[test]
fn equivalence_plain_mixed_literal_and_copy() {
    let payload = basis_payload(BLOCK_LEN as usize * 2 + 137);
    let ops = vec![
        Op::Literal(b"prefix".to_vec()),
        Op::Block(0),
        Op::Literal(vec![7u8; 64]),
        Op::Block(2), // trailing remainder block
        Op::Block(1),
    ];
    assert_equivalent_plain(&ops, &payload, true);
}

#[test]
fn equivalence_plain_empty_stream() {
    let payload = basis_payload(1024);
    let ops: Vec<Op> = Vec::new();
    assert_equivalent_plain(&ops, &payload, false);
}

/// The compressed safety gate. Builds the stream with the real
/// `CompressedTokenEncoder`, mixing literals and >=2 block matches so the
/// inflate/deflate dictionary (`see_token`) path is exercised. This test
/// FAILS if `DeltaApplicator` omits `see_token` after a block ref.
#[test]
fn equivalence_compressed_mixed_literal_and_copy() {
    let block_len = BLOCK_LEN as usize;
    let payload = basis_payload(block_len * 4 + 200);
    let ops = vec![
        Op::Literal(b"compressed prefix data that deflates".to_vec()),
        Op::Block(0),
        Op::Block(1),
        Op::Literal(vec![0x5A; 256]),
        Op::Block(3),
        Op::Literal(b"tail literal".to_vec()),
    ];
    let algo = ChecksumAlgorithm::MD5;
    let seed = 0;
    let dir = tempdir().expect("tempdir");
    let basis_path = write_basis(&dir, &payload);
    let signature = make_signature(&payload);
    let expected = expected_output(&ops, &payload, block_len);

    let mut wire = encode_compressed(&ops, &payload, block_len);
    append_checksum(&mut wire, algo, seed, &expected);

    let ref_out = dir.path().join("ref.bin");
    let mut server = Cursor::new(wire.clone());
    let mut ref_reader =
        TokenReader::new(Some(protocol::CompressionAlgorithm::Zlib)).expect("zlib");
    let ref_res = reference_apply(
        &mut server,
        &ref_out,
        false,
        Some(&signature),
        Some(basis_path.as_path()),
        &mut ref_reader,
        algo,
        seed,
        None,
    )
    .expect("reference apply (compressed)");

    let app_out = dir.path().join("app.bin");
    let mut app_reader =
        TokenReader::new(Some(protocol::CompressionAlgorithm::Zlib)).expect("zlib");
    let app_res = applicator_apply(
        &app_out,
        false,
        Some(&signature),
        Some(basis_path.as_path()),
        &wire,
        &mut app_reader,
        algo,
        seed,
        None,
    )
    .expect("applicator apply (compressed)");

    assert_eq!(
        read_file(&ref_out),
        read_file(&app_out),
        "compressed output bytes differ - see_token dictionary desync?"
    );
    assert_eq!(
        read_file(&app_out),
        expected,
        "compressed output != expected"
    );
    assert_eq!(
        ref_res.literal_bytes, app_res.literal_bytes,
        "compressed literal_bytes differ"
    );
}

/// Sparse trailing-zeros file: GAP-2 size check passes on a correct expected
/// size and fires `InvalidData` on a mismatch.
#[test]
fn equivalence_sparse_size_check() {
    let algo = ChecksumAlgorithm::MD5;
    let seed = 0;
    let dir = tempdir().expect("tempdir");

    // Literal run followed by a long zero tail so sparse finish must seek+1.
    let mut literal = b"head".to_vec();
    literal.extend_from_slice(&[0u8; 8192]);
    let ops = vec![Op::Literal(literal.clone())];
    let expected = literal.clone();
    let expected_size = expected.len() as u64;

    let mut wire = encode_plain(&ops);
    append_checksum(&mut wire, algo, seed, &expected);

    // Reference (sparse) with correct expected size: passes and reports
    // final_pos == expected_size.
    let ref_out = dir.path().join("ref.bin");
    let mut server = Cursor::new(wire.clone());
    let mut ref_reader = TokenReader::new(None).expect("plain");
    let ref_res = reference_apply(
        &mut server,
        &ref_out,
        true,
        None,
        None,
        &mut ref_reader,
        algo,
        seed,
        Some(expected_size),
    )
    .expect("reference sparse apply");
    assert_eq!(ref_res.final_pos, Some(expected_size));

    // DeltaApplicator (sparse) with correct expected size: passes.
    let app_out = dir.path().join("app.bin");
    let mut app_reader = TokenReader::new(None).expect("plain");
    let app_res = applicator_apply(
        &app_out,
        true,
        None,
        None,
        &wire,
        &mut app_reader,
        algo,
        seed,
        Some(expected_size),
    )
    .expect("applicator sparse apply");
    assert_eq!(app_res.final_pos, Some(expected_size));
    assert_eq!(read_file(&ref_out), read_file(&app_out));
    assert_eq!(read_file(&app_out), expected);

    // DeltaApplicator with a WRONG expected size: GAP-2 check fires.
    let app_out2 = dir.path().join("app2.bin");
    let mut app_reader2 = TokenReader::new(None).expect("plain");
    let err = applicator_apply(
        &app_out2,
        true,
        None,
        None,
        &wire,
        &mut app_reader2,
        algo,
        seed,
        Some(expected_size + 1),
    )
    .expect_err("size mismatch must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

/// A corrupted trailing wire checksum makes BOTH appliers return `InvalidData`.
#[test]
fn equivalence_checksum_mismatch_both_invalid() {
    let payload = basis_payload(2048);
    let ops = vec![Op::Literal(b"some literal content".to_vec())];
    let algo = ChecksumAlgorithm::MD5;
    let seed = 0;
    let dir = tempdir().expect("tempdir");
    let expected = expected_output(&ops, &payload, BLOCK_LEN as usize);

    let mut wire = encode_plain(&ops);
    append_checksum(&mut wire, algo, seed, &expected);
    // Corrupt the last checksum byte.
    let last = wire.len() - 1;
    wire[last] ^= 0xFF;

    let ref_out = dir.path().join("ref.bin");
    let mut server = Cursor::new(wire.clone());
    let mut ref_reader = TokenReader::new(None).expect("plain");
    let ref_err = reference_apply(
        &mut server,
        &ref_out,
        false,
        None,
        None,
        &mut ref_reader,
        algo,
        seed,
        None,
    )
    .expect_err("reference must reject");
    assert_eq!(ref_err.kind(), std::io::ErrorKind::InvalidData);

    let app_out = dir.path().join("app.bin");
    let mut app_reader = TokenReader::new(None).expect("plain");
    let app_err = applicator_apply(
        &app_out,
        false,
        None,
        None,
        &wire,
        &mut app_reader,
        algo,
        seed,
        None,
    )
    .expect_err("applicator must reject");
    assert_eq!(app_err.kind(), std::io::ErrorKind::InvalidData);
}
