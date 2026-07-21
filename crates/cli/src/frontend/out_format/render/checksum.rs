#![deny(unsafe_code)]

//! Full-file checksum rendering for the `%C` out-format placeholder.

use std::fmt::Write as FmtWrite;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;

use checksums::strong::{Md4, Md5, Sha1, StrongDigest, Xxh3, Xxh3_128, Xxh64};
use core::client::{ClientEntryKind, ClientEvent, ClientEventKind, StrongChecksumAlgorithm};

use crate::frontend::out_format::tokens::OutFormatContext;

/// Byte order upstream `util2.c:sum_as_hex` applies when formatting a digest.
///
/// Mirrors `checksum.c:canonical_checksum`: MD4/MD5/SHA return `-1` (the public
/// hex is the digest's natural order), the xxHash family returns `1` (the public
/// hex is the digest reversed), and non-canonical types return `0` (upstream
/// emits `NULL`, rendered here as spaces).
#[derive(Clone, Copy)]
enum SumByteOrder {
    /// Emit the digest bytes in natural order (MD4/MD5/SHA, canonical `-1`).
    Forward,
    /// Emit the digest bytes reversed (xxHash family, canonical `1`).
    Reversed,
}

/// Computes and formats the `%C` full-file checksum for an event.
///
/// Mirrors upstream `log.c:685-697`: `%C` reports the negotiated checksum
/// (`file_sum_nni` under `--checksum`, otherwise `xfer_sum_nni`) of a regular
/// file, using the digest length and byte order of `util2.c:sum_as_hex`. A
/// non-regular entry, an untransferred file without `--checksum`, or a
/// non-canonical algorithm renders `sum_len * 2` spaces.
///
/// The digest is recomputed over the committed destination. Every canonical
/// algorithm's whole-file sum is unseeded (`checksum.c:558 sum_init` resets the
/// xxHash state with seed `0`; `file_checksum` likewise resets with `0`; MD5/MD4
/// take no seed), so the recomputed bytes are identical to the `F_SUM` /
/// `sender_file_sum` the protocol carries for the same content.
pub(super) fn format_full_checksum(event: &ClientEvent, context: &OutFormatContext) -> String {
    let algorithm = context.full_checksum_algorithm();
    let spaces = || " ".repeat(hex_width(algorithm));

    // upstream: log.c:686 - only a regular file (`S_ISREG`) carries a %C sum.
    if let Some(metadata) = event.metadata()
        && metadata.kind() != ClientEntryKind::File
    {
        return spaces();
    }

    // upstream: log.c:687-690 - render `F_SUM` for any regular file under
    // `--checksum`, else `sender_file_sum` only for a transferred file
    // (`ITEM_TRANSFER`, i.e. a `DataCopied` event).
    let emit_sum = context.always_checksum() || matches!(event.kind(), ClientEventKind::DataCopied);
    if !emit_sum {
        return spaces();
    }

    // upstream: util2.c:98 - `sum_as_hex` returns NULL for a non-canonical
    // algorithm, which `log.c:691-696` renders as spaces.
    let Some(order) = canonical_order(algorithm) else {
        return spaces();
    };

    match hash_destination(algorithm, &event.destination_path()) {
        Some(digest) => render_hex(&digest, order),
        // A digest we cannot recompute (e.g. the destination is unreadable)
        // falls back to the space-padded field width.
        None => spaces(),
    }
}

/// Renders the digest bytes as hex in the requested byte order.
fn render_hex(digest: &[u8], order: SumByteOrder) -> String {
    let mut rendered = String::with_capacity(digest.len() * 2);
    match order {
        SumByteOrder::Forward => {
            for byte in digest {
                // write! to String is infallible
                let _ = write!(rendered, "{byte:02x}");
            }
        }
        SumByteOrder::Reversed => {
            for byte in digest.iter().rev() {
                let _ = write!(rendered, "{byte:02x}");
            }
        }
    }
    rendered
}

/// Returns the space-padded field width (`sum_len * 2`) for an algorithm.
///
/// upstream: log.c:691-694 - the empty `%C` field is `csum_len_for_type(...) * 2`
/// spaces wide.
const fn hex_width(algorithm: StrongChecksumAlgorithm) -> usize {
    digest_len(algorithm) * 2
}

/// Digest length in bytes, mirroring `checksum.c:csum_len_for_type`.
const fn digest_len(algorithm: StrongChecksumAlgorithm) -> usize {
    match algorithm {
        // `auto` negotiates xxh128 for a modern protocol-31+ transfer.
        StrongChecksumAlgorithm::Auto
        | StrongChecksumAlgorithm::Xxh128
        | StrongChecksumAlgorithm::Md5
        | StrongChecksumAlgorithm::Md4 => 16,
        StrongChecksumAlgorithm::Sha1 => 20,
        StrongChecksumAlgorithm::Xxh64 | StrongChecksumAlgorithm::Xxh3 => 8,
        // upstream: checksum.c:217 - `csum_len_for_type(CSUM_NONE)` is 1.
        StrongChecksumAlgorithm::None => 1,
    }
}

/// Byte order for an algorithm, or `None` when it is non-canonical.
///
/// upstream: checksum.c:255-278 `canonical_checksum`.
const fn canonical_order(algorithm: StrongChecksumAlgorithm) -> Option<SumByteOrder> {
    match algorithm {
        StrongChecksumAlgorithm::Md5
        | StrongChecksumAlgorithm::Md4
        | StrongChecksumAlgorithm::Sha1 => Some(SumByteOrder::Forward),
        StrongChecksumAlgorithm::Auto
        | StrongChecksumAlgorithm::Xxh128
        | StrongChecksumAlgorithm::Xxh64
        | StrongChecksumAlgorithm::Xxh3 => Some(SumByteOrder::Reversed),
        StrongChecksumAlgorithm::None => None,
    }
}

/// Computes the unseeded whole-file digest of `path` with `algorithm`.
///
/// Returns `None` for the non-canonical [`None`](StrongChecksumAlgorithm::None)
/// algorithm or when the file cannot be read.
fn hash_destination(algorithm: StrongChecksumAlgorithm, path: &Path) -> Option<Vec<u8>> {
    match algorithm {
        StrongChecksumAlgorithm::Md5 => stream_digest::<Md5>(path),
        StrongChecksumAlgorithm::Md4 => stream_digest::<Md4>(path),
        StrongChecksumAlgorithm::Sha1 => stream_digest::<Sha1>(path),
        StrongChecksumAlgorithm::Xxh64 => stream_digest::<Xxh64>(path),
        StrongChecksumAlgorithm::Xxh3 => stream_digest::<Xxh3>(path),
        StrongChecksumAlgorithm::Auto | StrongChecksumAlgorithm::Xxh128 => {
            stream_digest::<Xxh3_128>(path)
        }
        StrongChecksumAlgorithm::None => None,
    }
}

/// Streams `path` through a [`StrongDigest`] and returns its digest bytes.
fn stream_digest<S: StrongDigest>(path: &Path) -> Option<Vec<u8>> {
    let mut file = File::open(path).ok()?;
    let mut hasher = S::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => hasher.update(&buffer[..read]),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    Some(hasher.finalize().as_ref().to_vec())
}
