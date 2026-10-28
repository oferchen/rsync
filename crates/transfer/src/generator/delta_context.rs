//! Per-session state the sender's delta path reuses across files.
//!
//! # Upstream Reference
//!
//! - `sender.c:338-390` - `receive_sums()` reads one file's block sums
//! - `match.c:78-96` - `build_hash_table()` reuses one table across files
//! - `match.c:hash_search()` - rolling checksum block matching

use std::io::{self, Read};
use std::num::{NonZeroU8, NonZeroU32};

use engine::delta::{DeltaGenerator, DeltaScript, DeltaSignatureIndex, SignatureLayout};
use engine::signature::{FileSignature, SignatureBlock};
use matching::{ScanBuffers, ScanCounters};

use super::protocol_io::read_signature_blocks_keepalive;
use crate::delta_config::DeltaGeneratorConfig;
use crate::receiver::SumHead;
use crate::role_trailer::error_location;
use crate::shared::ChecksumFactory;

/// Buffers the sender's delta path keeps for a whole session.
///
/// Delta-encoding a file needs the received block sums, a hash index over
/// them, the scan's read buffer and rolling window, and a buffer to re-read
/// matched blocks into. The sender owns one context per session and resets
/// it per file, so only the first file allocates these.
///
/// upstream: match.c:78-96 `build_hash_table()` keeps `hash_table` in a
/// `static` and reallocates it only when its size changes. Upstream still
/// allocates the sums (sender.c:364-365) and the map window
/// (fileio.c:320-367) per file; reusing those as well changes no wire byte.
#[derive(Debug, Default)]
pub(crate) struct SenderDeltaContext {
    blocks: Vec<SignatureBlock>,
    index: Option<DeltaSignatureIndex>,
    scan: ScanBuffers,
    copy_buf: Vec<u8>,
}

impl SenderDeltaContext {
    /// Reads this file's block sums from the receiver, replacing the previous
    /// file's.
    ///
    /// upstream: sender.c:338-390 `receive_sums()`.
    pub(crate) fn receive_sums<R, F>(
        &mut self,
        reader: &mut R,
        sum_head: &SumHead,
        lull_mod: u32,
        on_lull: F,
    ) -> io::Result<()>
    where
        R: Read,
        F: FnMut() -> io::Result<()>,
    {
        read_signature_blocks_keepalive(reader, sum_head, lull_mod, &mut self.blocks, on_lull)
    }

    /// Runs the sequential delta scan of `source` against the received sums.
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:390-431` - delta generation path after `receive_sums()`
    /// - `match.c:hash_search()` - rolling checksum block matching
    pub(crate) fn generate<R: Read>(
        &mut self,
        source: R,
        config: DeltaGeneratorConfig<'_>,
        source_len: u64,
    ) -> io::Result<(DeltaScript<'static>, ScanCounters)> {
        let generator = scan_generator(&config, source_len);
        let index = rebuild_index(&mut self.index, &mut self.blocks, &config)?;
        generator
            .generate_counted_with(source, index, &mut self.scan)
            .map_err(delta_failed)
    }

    /// Runs the delta scan of a memory-mapped `source` with the opt-in
    /// parallel scan when the basis is duplicate-free.
    ///
    /// The caller (`generator::transfer::transfer_loop`) maps the source and
    /// calls this only after the size/core gate in `should_parallel_delta` has
    /// fired. The mapping spans the whole file and the scan touches every
    /// page, so peak RSS is proportional to the file size; the win is CPU
    /// parallelism, not memory. The index is rebuilt exactly as for
    /// [`Self::generate`], then:
    ///
    /// - **Duplicate-free basis** ([`DeltaSignatureIndex::has_duplicate_blocks`]
    ///   is `false`): run [`DeltaGenerator::generate_chunked`], which scans the
    ///   ranges across rayon workers. Reconstruction and the total/literal byte
    ///   counts match the sequential scan on every input. For the eligible
    ///   inputs (matches never straddle a range boundary) the emitted token
    ///   stream also matches, and the wire bytes match in the common case; a
    ///   rare literal-run segmentation seam at a range boundary can still shift
    ///   the literal-token length framing by a few bytes (never the counts or
    ///   the reconstructed data). See [`DeltaGenerator::generate_chunked`] for
    ///   the exact contract.
    /// - **Duplicate-content basis**: fall back to the sequential scan over the
    ///   same slice, because each parallel stripe starts its `want_i` hint fresh
    ///   and could resolve duplicate siblings differently, diverging from
    ///   upstream's token stream.
    ///
    /// `max_chunks` bounds the number of parallel ranges (the caller passes
    /// `rayon::current_num_threads().min(8)`).
    ///
    /// # Upstream Reference
    ///
    /// The sender-side matching contract is unchanged from [`Self::generate`];
    /// this only parallelizes the scan of a single large file. See
    /// `match.c:hash_search()`.
    pub(crate) fn generate_chunked<'a>(
        &mut self,
        source: &'a [u8],
        config: DeltaGeneratorConfig<'_>,
        max_chunks: usize,
    ) -> io::Result<(DeltaScript<'a>, ScanCounters)> {
        let generator = scan_generator(&config, source.len() as u64);
        let index = rebuild_index(&mut self.index, &mut self.blocks, &config)?;
        // A duplicate-content basis keeps the sequential scan, as does a run
        // that reports the `-vv` match counters: only that scan visits
        // upstream's offsets, which the counters are replayed at.
        let result =
            if index.has_duplicate_blocks() || matching::trace_deltasum::counters_reported() {
                generator.generate_counted_with(io::Cursor::new(source), index, &mut self.scan)
            } else {
                generator.generate_chunked_counted(source, index, max_chunks)
            };
        result.map_err(delta_failed)
    }

    /// The buffer the wire-write pass re-reads matched blocks into.
    pub(crate) fn copy_buf(&mut self) -> &mut Vec<u8> {
        &mut self.copy_buf
    }

    /// Creates a context holding already-received `blocks`, for tests that
    /// build signatures directly instead of reading them off the wire.
    #[cfg(test)]
    pub(crate) fn with_blocks(blocks: Vec<SignatureBlock>) -> Self {
        Self {
            blocks,
            ..Self::default()
        }
    }

    /// Rebuilds and returns the index over the held blocks.
    #[cfg(test)]
    pub(crate) fn index(
        &mut self,
        config: &DeltaGeneratorConfig<'_>,
    ) -> io::Result<&DeltaSignatureIndex> {
        rebuild_index(&mut self.index, &mut self.blocks, config)
    }
}

/// Builds the per-file scan driver from the negotiated parameters.
fn scan_generator(config: &DeltaGeneratorConfig<'_>, source_len: u64) -> DeltaGenerator {
    DeltaGenerator::new()
        .with_consecutive_match_needed(consecutive_match_needed(config))
        .with_updating_basis_file(config.updating_basis_file)
        .with_source_len(source_len)
}

/// Refills `index` from `blocks`, building it on first use.
///
/// `blocks` lends its allocation to the file signature and takes it back, so
/// the next [`SenderDeltaContext::receive_sums`] refills the same buffer.
fn rebuild_index<'i>(
    index: &'i mut Option<DeltaSignatureIndex>,
    blocks: &mut Vec<SignatureBlock>,
    config: &DeltaGeneratorConfig<'_>,
) -> io::Result<&'i DeltaSignatureIndex> {
    let signature = file_signature(std::mem::take(blocks), config)?;
    let algorithm = ChecksumFactory::from_negotiation(
        config.negotiated_algorithms,
        config.protocol,
        config.checksum_seed,
        config.compat_flags,
    )
    .signature_algorithm();
    let built = match index.as_mut() {
        Some(existing) => existing.rebuild(&signature, algorithm),
        None => {
            *index = DeltaSignatureIndex::from_signature(&signature, algorithm);
            index.is_some()
        }
    };
    *blocks = signature.into_blocks();
    match index.as_ref() {
        Some(index) if built => {
            // upstream: match.c:436-437 - `match_sums()` announces the table it
            // just built, immediately before entering `hash_search()`.
            matching::trace_deltasum::trace_built_hash_table();
            Ok(index)
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "failed to create signature index {}{}",
                error_location!(),
                crate::role_trailer::sender()
            ),
        )),
    }
}

/// Wraps the received `blocks` in a [`FileSignature`] with the `sum_head`
/// layout carried by `config`.
///
/// # Upstream Reference
///
/// - `io.c:2284-2288` - `read_sum_head()` reads and range-checks `remainder`
/// - `generator.c:760-761` - `remainder = len % blength`,
///   `count = len/blength + (remainder != 0)`
fn file_signature(
    blocks: Vec<SignatureBlock>,
    config: &DeltaGeneratorConfig<'_>,
) -> io::Result<FileSignature> {
    let block_length = NonZeroU32::new(config.block_length).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "block length must be non-zero {}{}",
                error_location!(),
                crate::role_trailer::sender()
            ),
        )
    })?;
    let strong_sum_length = NonZeroU8::new(config.strong_sum_length).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "strong sum length must be non-zero {}{}",
                error_location!(),
                crate::role_trailer::sender()
            ),
        )
    })?;
    let block_count = blocks.len() as u64;
    let remainder = config.remainder;
    let layout =
        SignatureLayout::from_raw_parts(block_length, remainder, block_count, strong_sum_length);
    // Inverts generator.c:760-761 to recover the basis length the sum_head
    // describes.
    let total_bytes = if remainder == 0 {
        block_count * u64::from(config.block_length)
    } else {
        block_count.saturating_sub(1) * u64::from(config.block_length) + u64::from(remainder)
    };
    Ok(FileSignature::from_raw_parts(layout, blocks, total_bytes))
}

fn delta_failed(e: io::Error) -> io::Error {
    io::Error::other(format!(
        "delta generation failed: {e} {}{}",
        error_location!(),
        crate::role_trailer::sender()
    ))
}

/// Derives the zsync consecutive-match gating threshold (`seq_matches`) from the
/// mutually negotiated compat flags.
///
/// Returns `2` only when the private `CAP_CONSECUTIVE_MATCH` bit is present -
/// the exact same condition under which the receiver halved the per-block
/// strong-sum length carried in `config.strong_sum_length`. The gating
/// compensates for the shorter, weaker checksum; the two are bound to one
/// negotiated bit so they can never diverge. Absent the bit (any upstream peer,
/// or no opt-in) the threshold stays at `1` and the scan is upstream-identical.
fn consecutive_match_needed(config: &DeltaGeneratorConfig<'_>) -> u8 {
    if config
        .compat_flags
        .is_some_and(|f| f.contains(protocol::CompatibilityFlags::CONSECUTIVE_MATCH))
    {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::num::{NonZeroU8, NonZeroU32};

    use protocol::{ChecksumAlgorithm, ProtocolVersion};
    use signature::{SignatureLayoutParams, calculate_signature_layout, generate_file_signature};
    use tempfile::NamedTempFile;

    use super::*;
    use crate::generator::delta::{script_to_wire_delta, write_delta_with_inline_checksum};
    use crate::generator::open_source::SourceOpen;

    const STRONG_LEN: u8 = 16;

    /// One file as the sender sees it: the receiver's sum_head and block sums,
    /// plus the source to delta-encode against them.
    struct SignedFile {
        sum_head: SumHead,
        sums: Vec<u8>,
        config: DeltaGeneratorConfig<'static>,
        source: NamedTempFile,
        source_len: u64,
    }

    fn lcg_bytes(seed: u64, len: usize) -> Vec<u8> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                (state >> 33) as u8
            })
            .collect()
    }

    fn signed_file(basis: &[u8], source: &[u8], block_len: u32) -> SignedFile {
        let algorithm = ChecksumFactory::from_negotiation(None, ProtocolVersion::NEWEST, 0, None)
            .signature_algorithm();
        let layout = calculate_signature_layout(SignatureLayoutParams::new(
            basis.len() as u64,
            NonZeroU32::new(block_len),
            ProtocolVersion::NEWEST,
            NonZeroU8::new(STRONG_LEN).expect("strong length"),
        ))
        .expect("layout");
        let signature =
            generate_file_signature(io::Cursor::new(basis), layout, algorithm).expect("signature");
        let mut sums = Vec::new();
        for block in signature.blocks() {
            sums.extend_from_slice(&block.rolling().value().to_le_bytes());
            sums.extend_from_slice(block.strong());
        }
        let block_length = layout.block_length().get();
        let strong_len = layout.strong_sum_length().get();
        let remainder = (basis.len() % block_length as usize) as u32;
        let count = signature.blocks().len() as u32;
        let mut file = NamedTempFile::new().expect("temp source");
        file.write_all(source).expect("write source");
        SignedFile {
            sum_head: SumHead::new(count, block_length, u32::from(strong_len), remainder),
            sums,
            config: DeltaGeneratorConfig::new(block_length, strong_len, ProtocolVersion::NEWEST)
                .with_remainder(remainder),
            source: file,
            source_len: source.len() as u64,
        }
    }

    /// Sends one file through `ctx` exactly as the sender loop does and
    /// returns everything that reaches the wire, plus the scan counters.
    fn send(ctx: &mut SenderDeltaContext, file: &SignedFile) -> (Vec<u8>, ScanCounters) {
        ctx.receive_sums(&mut io::Cursor::new(&file.sums), &file.sum_head, 0, || {
            Ok(())
        })
        .expect("receive sums");
        let source = std::fs::File::open(file.source.path()).expect("open source");
        let (script, counters) = ctx
            .generate(source, file.config, file.source_len)
            .expect("delta");
        let mut wire = Vec::new();
        let result = write_delta_with_inline_checksum(
            &mut wire,
            script_to_wire_delta(&script, file.config.block_length),
            ctx.copy_buf(),
            None,
            false,
            file.source.path(),
            &SourceOpen::new(None, None, false, false),
            ChecksumAlgorithm::MD5,
            0,
            ProtocolVersion::NEWEST,
        )
        .expect("wire delta");
        wire.extend_from_slice(&result.checksum_buf[..result.checksum_len]);
        (wire, counters)
    }

    // WHY: the sender keeps one context for the whole session, so any state a
    // file leaves behind - block sums, the index, the scan window, the copy
    // buffer - would corrupt every later file's delta. Running a large file,
    // then a small one with a different block length, then a large one again
    // through ONE context must put on the wire exactly the bytes a fresh
    // context produces for each file on its own.
    #[test]
    fn a_reused_context_sends_the_same_bytes_as_a_fresh_one() {
        let big_basis = lcg_bytes(1, 64 * 1024);
        let mut big_source = big_basis.clone();
        big_source[20_000..20_100].copy_from_slice(&lcg_bytes(2, 100));

        let small_basis = lcg_bytes(3, 4 * 700 + 200);
        let mut small_source = small_basis.clone();
        small_source[900..910].copy_from_slice(&lcg_bytes(4, 10));

        let mut mid_basis = lcg_bytes(5, 16 * 1024);
        mid_basis[..1024].copy_from_slice(&big_basis[..1024]);
        let mut mid_source = mid_basis.clone();
        mid_source.extend_from_slice(&small_basis[..700]);

        let files = [
            signed_file(&big_basis, &big_source, 1024),
            signed_file(&small_basis, &small_source, 700),
            signed_file(&mid_basis, &mid_source, 1024),
        ];

        let mut reused = SenderDeltaContext::default();
        for (n, file) in files.iter().enumerate() {
            let fresh = send(&mut SenderDeltaContext::default(), file);
            let (wire, counters) = send(&mut reused, file);
            assert!(
                counters.matches > 0,
                "file {n} must exercise the match path"
            );
            assert_eq!(
                (wire, counters),
                fresh,
                "file {n}: a reused context must send what a fresh one sends"
            );
        }
    }
}
