//! Basis file search and signature generation.
//!
//! Implements the search strategy for finding a suitable basis file for delta
//! transfer (exact match, reference directories, fuzzy matching) and generates
//! the file signature used by the sender to compute deltas.

use std::fs;
use std::num::NonZeroU8;
use std::path::PathBuf;
use std::sync::OnceLock;

use engine::delta::{SignatureLayout, SignatureLayoutParams, calculate_signature_layout};
use engine::fuzzy::{FuzzyMatcher, trace_fuzzy_basis_selected};
use engine::signature::{
    FileSignature, PARALLEL_THRESHOLD_BYTES, SignatureAlgorithm, SignatureError,
    generate_file_signature, generate_file_signature_windowed,
};
use protocol::ProtocolVersion;

use crate::config::ReferenceDirectory;

/// Result of searching for a basis file via [`find_basis_file_with_config`].
///
/// Contains both the generated signature and the path to the basis file
/// that was used. When no basis is found, both fields are `None`; use
/// [`is_empty`](Self::is_empty) to check.
#[derive(Debug)]
pub struct BasisFileResult {
    /// The generated signature (None if no basis found).
    pub signature: Option<FileSignature>,
    /// Path to the basis file used (None if no basis found).
    pub basis_path: Option<PathBuf>,
    /// Which basis the generator selected, sent to the sender as the
    /// `fnamecmp_type` byte behind `ITEM_BASIS_TYPE_FOLLOWS`.
    ///
    /// [`protocol::FnameCmpType::Fname`] for the ordinary destination basis (no wire byte),
    /// `FnameCmpType::BasisDir(j)` (`FNAMECMP_BASIS_DIR_LOW + j`) when the basis
    /// came from reference dir `j` (`--compare-dest`/`--copy-dest`/`--link-dest`)
    /// while the destination was absent (upstream: generator.c:1054),
    /// [`protocol::FnameCmpType::PartialDir`] (`0x81`) when the basis was recovered
    /// from `--partial-dir` on a resume (upstream: generator.c:1759-1765,1853), and
    /// `FnameCmpType::Fuzzy(i)` (`FNAMECMP_FUZZY + i`) for a `--fuzzy` match, where
    /// `i` is 0 for the destination directory and `k + 1` for reference dir `k`
    /// (upstream: generator.c:861,903,1945).
    pub fnamecmp_type: protocol::FnameCmpType,
    /// The alternate-basis name sent as an `ITEM_XNAME_FOLLOWS` vstring, present
    /// only for a fuzzy match. Upstream sends `fuzzy_file->basename` - the bare
    /// basename, resolved by the receiver relative to the target's directory
    /// (upstream: generator.c:1948, receiver.c:838-841).
    pub xname: Option<Vec<u8>>,
}

impl BasisFileResult {
    /// Empty result when no basis file is found.
    pub(super) const EMPTY: Self = Self {
        signature: None,
        basis_path: None,
        fnamecmp_type: protocol::FnameCmpType::Fname,
        xname: None,
    };

    /// Returns true if no basis file was found.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.signature.is_none()
    }
}

/// Configuration for basis file search and signature generation.
///
/// Passed to [`find_basis_file_with_config`] to control where to look for
/// a basis file (exact match, reference directories, fuzzy match) and how
/// to generate its signature (protocol version, checksum algorithm, length).
#[derive(Debug)]
pub struct BasisFileConfig<'a> {
    /// Target file path in destination.
    pub file_path: &'a std::path::Path,
    /// Destination directory base.
    pub dest_dir: &'a std::path::Path,
    /// Relative path from destination root.
    pub relative_path: &'a std::path::Path,
    /// Expected size of the target file.
    pub target_size: u64,
    /// Target file modification time in whole seconds since the Unix epoch,
    /// used for the fuzzy size/modtime fast-path. upstream: generator.c:858.
    pub target_mtime: i64,
    /// Fuzzy matching level (0=off, 1=dest dir, 2=dest+reference dirs).
    ///
    /// Upstream: `options.c:764` - `fuzzy_basis` controls how many directory
    /// sources are searched. Level 1 searches only the destination directory.
    /// Level 2 also searches reference directories (`--compare-dest`,
    /// `--copy-dest`, `--link-dest`).
    pub fuzzy_level: u8,
    /// List of reference directories to check.
    pub reference_directories: &'a [ReferenceDirectory],
    /// `--partial-dir` directory, when set. On a resume where the destination
    /// is absent, the generator falls back to a same-named regular file inside
    /// this directory as the delta basis (`FNAMECMP_PARTIAL_DIR`).
    ///
    /// Upstream: `generator.c:1759-1765` - `partialptr = partial_dir_fname(fname)`.
    pub partial_dir: Option<&'a std::path::Path>,
    /// Protocol version for signature generation.
    pub protocol: ProtocolVersion,
    /// Checksum truncation length.
    pub checksum_length: NonZeroU8,
    /// Algorithm for strong checksums.
    pub checksum_algorithm: engine::signature::SignatureAlgorithm,
    /// When true, skip basis file search entirely (upstream `--whole-file`).
    pub whole_file: bool,
    /// Mutually negotiated compatibility flags. Only the private
    /// [`protocol::CompatibilityFlags::CONSECUTIVE_MATCH`] bit is consulted
    /// here, to decide whether the per-block strong-sum length is halved (see
    /// [`protocol::effective_s2length`]). `None` (local copy / no negotiation)
    /// leaves the strong sum at full length, byte-identical to upstream.
    pub compat_flags: Option<protocol::CompatibilityFlags>,
}

/// Configuration for generating a signature from a basis file.
///
/// This parameter object encapsulates the signature-related configuration
/// needed to generate a file signature, reducing parameter count and improving
/// maintainability.
#[derive(Debug, Clone, Copy)]
struct SignatureGenerationConfig {
    /// Protocol version for signature layout calculation.
    protocol: ProtocolVersion,
    /// Checksum truncation length.
    checksum_length: NonZeroU8,
    /// Algorithm for strong checksums.
    checksum_algorithm: engine::signature::SignatureAlgorithm,
    /// Mutually negotiated compatibility flags (see [`BasisFileConfig::compat_flags`]).
    compat_flags: Option<protocol::CompatibilityFlags>,
}

impl SignatureGenerationConfig {
    /// Extracts signature generation config from a BasisFileConfig.
    fn from_basis_config(config: &BasisFileConfig<'_>) -> Self {
        Self {
            protocol: config.protocol,
            checksum_length: config.checksum_length,
            checksum_algorithm: config.checksum_algorithm,
            compat_flags: config.compat_flags,
        }
    }
}

/// Tries to find a basis file in the reference directories.
///
/// Iterates through reference directories in order, checking if the relative
/// path exists in each one. Returns the first match found along with its index
/// `j` in `reference_directories` - the upstream `basis_dir[j]` slot the match
/// came from, which becomes the `FNAMECMP_BASIS_DIR_LOW + j` wire tag. The basis
/// open goes through [`fast_io::open_basis_nofollow`], which follows directory
/// symlinks (so `--copy-dirlinks` continues to work) but refuses to follow
/// a symlinked leaf component, mirroring upstream `syscall.c:705`
/// (`do_open_at`).
///
/// # Upstream Reference
///
/// - `generator.c:975-995` - `try_dests_reg()` scans `basis_dir[j]` in order;
///   the first existing regular file sets `best_match = j` (match_level 1).
/// - `generator.c:1054` - `return FNAMECMP_BASIS_DIR_LOW + j` when the file
///   exists in the reference dir but its content differs (match_level 1).
/// - `syscall.c:705` (`do_open_at`) - dirname/basename split with
///   `O_NOFOLLOW` on the basename.
pub(super) fn try_reference_directories(
    relative_path: &std::path::Path,
    reference_directories: &[ReferenceDirectory],
) -> Option<(fs::File, u64, PathBuf, usize)> {
    for (index, ref_dir) in reference_directories.iter().enumerate() {
        let candidate = ref_dir.path.join(relative_path);
        if let Ok(file) = fast_io::open_basis_nofollow(&candidate) {
            if let Ok(meta) = file.metadata() {
                if meta.is_file() {
                    return Some((file, meta.len(), candidate, index));
                }
            }
        }
    }
    None
}

/// Maps a reference-directory index `j` to its `FNAMECMP_BASIS_DIR_LOW + j` wire
/// tag. Upstream stores at most `MAX_BASIS_DIRS` (20) alt-dest directories and
/// the basis-type byte spans `0x00..=0x7F` (`FNAMECMP_BASIS_DIR_HIGH`), so any
/// realistic index fits. A pathological index beyond the range degrades to
/// `FNAMECMP_FNAME` (no wire byte): the basis is still reconstructed in-process
/// from `basis_path`, so only the wire advertisement is affected.
///
/// upstream: rsync.h `FNAMECMP_BASIS_DIR_LOW`/`FNAMECMP_BASIS_DIR_HIGH`,
/// generator.c:1054.
fn basis_dir_fnamecmp_type(index: usize) -> protocol::FnameCmpType {
    match u8::try_from(index) {
        Ok(byte) if byte <= protocol::FnameCmpType::BASIS_DIR_HIGH => {
            protocol::FnameCmpType::BasisDir(byte)
        }
        _ => protocol::FnameCmpType::Fname,
    }
}

/// Opens a basis file and returns it with metadata.
///
/// Returns the file handle, size, and path if successful. The open is
/// routed through [`fast_io::open_basis_nofollow`] so a symlinked
/// basename is refused (`ELOOP`), matching upstream's `do_open_at()`
/// receiver-side defence against pre-planted leaf symlinks while still
/// honouring `--copy-dirlinks` directory symlinks at every intermediate
/// component.
fn try_open_file(path: &std::path::Path) -> Option<(fs::File, u64, PathBuf)> {
    let file = fast_io::open_basis_nofollow(path).ok()?;
    let size = file.metadata().ok()?.len();
    Some((file, size, path.to_path_buf()))
}

/// A fuzzy basis the generator selected, ready to be signed and advertised.
struct FuzzyBasis {
    /// The opened basis file, its size, and path (for signature generation).
    file: fs::File,
    size: u64,
    path: PathBuf,
    /// The `fnamecmp_type` to advertise on the wire; `Fuzzy(i)` for a fuzzy
    /// basis, where `i` names the dest dir (0) or reference dir `k` (`k + 1`).
    /// upstream: generator.c:861,903.
    fnamecmp_type: protocol::FnameCmpType,
    /// The `ITEM_XNAME_FOLLOWS` basename, always present for a fuzzy basis
    /// (upstream: generator.c:1948 `fuzzy_file->basename`).
    xname: Option<Vec<u8>>,
}

/// Attempts fuzzy matching to find a similar basis file.
///
/// For level 1, searches only the destination directory. For level 2,
/// also searches reference directories (`--compare-dest`, `--copy-dest`,
/// `--link-dest`) by passing them as fuzzy basis dirs to the matcher.
///
/// A match found in the destination directory is advertised as
/// `FnameCmpType::Fuzzy(0)` (`0x83`); a match sourced from a level-2 reference
/// directory `k` is advertised as `FnameCmpType::Fuzzy(k + 1)`
/// (`FNAMECMP_FUZZY + i`, where the dest dir is fuzzy-index 0 and reference dir
/// `k` is fuzzy-index `k + 1`). Either carries the basis basename as the
/// `ITEM_XNAME_FOLLOWS` vstring, byte-for-byte with upstream. The reference-dir
/// index is recovered by mapping the matched file's parent directory back to the
/// original `reference_directories` slot, so it names the peer's `basis_dir[]`
/// entry even when earlier reference dirs did not exist on disk. The basis is
/// reconstructed in-process from `basis_path`, so the tag only affects the wire
/// advertisement, never correctness.
///
/// # Upstream Reference
///
/// - `generator.c:861,903` - `find_fuzzy()`; `*fnamecmp_type_ptr = FNAMECMP_FUZZY + i`
/// - `generator.c:1945-1948` - `ITEM_XNAME_FOLLOWS` + `fuzzy_file->basename`
/// - `options.c:2120` - `fuzzy_basis = basis_dir_cnt + 1` for level 2
fn try_fuzzy_match(
    relative_path: &std::path::Path,
    dest_dir: &std::path::Path,
    target_size: u64,
    target_mtime: i64,
    fuzzy_level: u8,
    reference_directories: &[ReferenceDirectory],
) -> Option<FuzzyBasis> {
    let target_name = relative_path.file_name()?;

    // Build the search directory for reference dirs: join each reference
    // dir base with the target file's parent directory, mirroring upstream
    // generator.c where fuzzy_dirlist[i] = get_dirlist(basis_dir[i-1]/dn).
    let parent_dir = relative_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new(""));
    // Keep the joined path for each reference dir so a match can be mapped back
    // to its original index (upstream fuzzy-index `i` = basis_dir slot + 1),
    // even though non-existent dirs are skipped from the actual search.
    let ref_search_dirs: Vec<PathBuf> = if fuzzy_level >= 2 {
        reference_directories
            .iter()
            .map(|rd| rd.path.join(parent_dir))
            .collect()
    } else {
        Vec::new()
    };
    let basis_dirs: Vec<PathBuf> = ref_search_dirs
        .iter()
        .filter(|p| p.is_dir())
        .cloned()
        .collect();

    let fuzzy_matcher = FuzzyMatcher::with_level(fuzzy_level).with_fuzzy_basis_dirs(basis_dirs);
    let fuzzy_match =
        fuzzy_matcher.find_fuzzy_basis(target_name, dest_dir, target_size, Some(target_mtime))?;
    // upstream: generator.c:1788 - announce the selected fuzzy basis at
    // FUZZY,1 the moment the matcher returns a candidate, before we attempt
    // to open it.
    trace_fuzzy_basis_selected(
        &relative_path.display().to_string(),
        &fuzzy_match.path.display().to_string(),
    );
    let (file, size, path) = try_open_file(&fuzzy_match.path)?;

    // Map the matched candidate back to its fuzzy-index and advertise the
    // matching FNAMECMP_FUZZY + i tag with the basis basename as the xname.
    // upstream: generator.c:843/868 iterate dirlist_array[0] (dest dir, index 0)
    // then basis_dir[i-1] (reference dir k -> index k + 1).
    let fnamecmp_type = fuzzy_index(&path, dest_dir, &ref_search_dirs)?;
    let basename = path
        .file_name()
        .map(basename_wire_bytes)
        .unwrap_or_default();

    Some(FuzzyBasis {
        file,
        size,
        path,
        fnamecmp_type,
        xname: Some(basename),
    })
}

/// Resolves the `FNAMECMP_FUZZY + i` tag for a fuzzy match at `path`.
///
/// Upstream indexes the fuzzy search over `dirlist_array[0..fuzzy_basis]`: index
/// 0 is the destination directory, index `k + 1` is reference dir `k`
/// (`basis_dir[k]`). The candidate's parent directory is compared against the
/// destination directory and then each reference dir's joined search path (in
/// original order, so a skipped non-existent earlier dir does not shift the
/// index). Returns `None` if the parent matches no known search directory - the
/// caller then falls back to a whole-file transfer rather than emit a wrong tag.
///
/// upstream: generator.c:843,868 (`dirlist_array[i]`), generator.c:861,903
/// (`FNAMECMP_FUZZY + i`).
fn fuzzy_index(
    path: &std::path::Path,
    dest_dir: &std::path::Path,
    ref_search_dirs: &[PathBuf],
) -> Option<protocol::FnameCmpType> {
    let parent = path.parent()?;
    if parent == dest_dir {
        return Some(protocol::FnameCmpType::Fuzzy(0));
    }
    let k = ref_search_dirs.iter().position(|dir| dir == parent)?;
    let offset = u8::try_from(k + 1).ok()?;
    Some(protocol::FnameCmpType::Fuzzy(offset))
}

/// Encodes a basis basename as the raw bytes upstream puts in the xname vstring.
///
/// On Unix the on-disk bytes are used verbatim (`fuzzy_file->basename` is a raw
/// byte string). On other platforms the name is UTF-8 encoded, matching how oc
/// serialises file names elsewhere; the receiver reconstructs from the
/// in-process basis path regardless, so this only affects the wire byte form.
fn basename_wire_bytes(name: &std::ffi::OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        name.as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        name.to_string_lossy().into_owned().into_bytes()
    }
}

/// Generates a signature for the given basis file.
///
/// # Arguments
///
/// * `basis_file` - The file to generate a signature for
/// * `basis_size` - The size of the basis file in bytes
/// * `basis_path` - The path to the basis file
/// * `config` - Signature generation configuration
///
/// # Returns
///
/// A `BasisFileResult` containing the signature and path, or empty if generation fails.
fn generate_basis_signature(
    basis_file: fs::File,
    basis_size: u64,
    basis_path: PathBuf,
    fnamecmp_type: protocol::FnameCmpType,
    xname: Option<Vec<u8>>,
    config: SignatureGenerationConfig,
) -> BasisFileResult {
    // Cap the per-file strong-sum length by the negotiated transfer checksum's
    // digest width. `sum_sizes_sqroot()` clamps s2length to
    // `max_s2length = MIN(SUM_LENGTH, xfer_sum_len)`, so it never exceeds the
    // negotiated digest length. `calculate_signature_layout` applies both halves
    // once the digest width is threaded in: the `SUM_LENGTH` (16) cap plus the
    // `xfer_sum_len` cap from the negotiated algorithm's digest width. For the
    // default 16-byte digests (MD5, MD4, XXH3-128) this is a no-op and the
    // SumHead stays byte-identical to upstream; a short digest (XXH64 / XXH3-64 =
    // 8 bytes) bounds s2length so we never write a zero-padded strong sum wider
    // than the checksum the sender expects. This runs before the private
    // CAP_CONSECUTIVE_MATCH halving below, mirroring upstream's order (cap in
    // sum_sizes_sqroot, then any extension).
    // upstream: generator.c:705 sum_sizes_sqroot() `max_s2length`,
    // checksum.c:214 csum_len_for_type().
    let digest_len =
        NonZeroU8::new(config.checksum_algorithm.digest_len().min(u8::MAX as usize) as u8)
            .expect("negotiated digest length is at least one byte");
    let params =
        SignatureLayoutParams::new(basis_size, None, config.protocol, config.checksum_length)
            .with_transfer_digest_length(digest_len);

    let layout = match calculate_signature_layout(params) {
        Ok(layout) => layout,
        Err(_) => return BasisFileResult::EMPTY,
    };

    // Iron invariant choke-point: the per-block strong-sum length is shrunk
    // here, and ONLY here, and ONLY when the mutually negotiated compat flags
    // carry the private CAP_CONSECUTIVE_MATCH bit. That bit can only survive the
    // negotiation AND when both peers are oc and both opted in, in which case
    // the sender applies seq_matches=2 gating to compensate for the shorter
    // checksum. Against any upstream peer, or without the opt-in, the mutual bit
    // is absent and `effective_s2length` returns the full length verbatim,
    // yielding a SumHead byte-identical to upstream.
    let layout = {
        let base_len = layout.strong_sum_length().get();
        let negotiated = config
            .compat_flags
            .unwrap_or(protocol::CompatibilityFlags::EMPTY);
        let eff_len = protocol::effective_s2length(negotiated, base_len);
        if eff_len == base_len {
            layout
        } else {
            let eff_nz = NonZeroU8::new(eff_len).expect("effective_s2length floors at 1");
            SignatureLayout::from_raw_parts(
                layout.block_length(),
                layout.remainder(),
                layout.block_count(),
                eff_nz,
            )
        }
    };

    let parallel = parallel_checksum_enabled();

    // Large regular baseses read the whole file block-by-block to hash it. A
    // raw `File` costs one read syscall per block (default block ~700 bytes),
    // so a multi-GB basis issues millions of syscalls. Memory-mapping the basis
    // replaces those syscalls with demand-paged access while reading the exact
    // same bytes in the same order - the per-block rolling and strong sums, and
    // therefore the wire signature and the resulting delta, are byte-identical.
    // Small baseses stay on the buffered path (mmap setup is not worth it) and
    // any mmap failure (NFS/FUSE/procfs, ENOMEM, non-regular file) falls back
    // to the raw `File` reader.
    #[cfg(unix)]
    let signature = if basis_size >= MMAP_BASIS_THRESHOLD_BYTES {
        match fast_io::MmapReader::from_file(basis_file) {
            Ok(mapped) => {
                let _ = mapped.advise_sequential();
                compute_basis_signature(
                    mapped,
                    basis_size,
                    layout,
                    config.checksum_algorithm,
                    parallel,
                )
            }
            Err(_) => match reopen_basis(&basis_path) {
                Some(file) => compute_basis_signature(
                    file,
                    basis_size,
                    layout,
                    config.checksum_algorithm,
                    parallel,
                ),
                None => return BasisFileResult::EMPTY,
            },
        }
    } else {
        compute_basis_signature(
            basis_file,
            basis_size,
            layout,
            config.checksum_algorithm,
            parallel,
        )
    };

    #[cfg(not(unix))]
    let signature = compute_basis_signature(
        basis_file,
        basis_size,
        layout,
        config.checksum_algorithm,
        parallel,
    );

    match signature {
        Ok(sig) => BasisFileResult {
            signature: Some(sig),
            basis_path: Some(basis_path),
            fnamecmp_type,
            xname,
        },
        Err(_) => BasisFileResult::EMPTY,
    }
}

/// Minimum basis-file size at which the receiver memory-maps the basis for
/// signature hashing instead of reading it through a raw `File`.
///
/// Matches `fast_io::MmapReader`'s own `MMAP_THRESHOLD` (64 KiB). Below this the
/// buffered read wins because mmap setup and page-fault overhead outweigh the
/// saved syscalls; above it the syscall-per-block cost dominates. The choice is
/// a pure local performance knob: the computed signature is byte-identical
/// either way, so it is never negotiated or sent over the wire.
#[cfg(unix)]
const MMAP_BASIS_THRESHOLD_BYTES: u64 = 64 * 1024;

/// Re-opens a basis file through the same hardened, symlink-refusing open used
/// for the original lookup, so the mmap fallback path never re-introduces a
/// symlinked-basename window. Returns `None` if the file can no longer be
/// opened as a regular file, in which case the caller treats the basis as
/// absent and falls back to whole-file transfer.
#[cfg(unix)]
fn reopen_basis(path: &std::path::Path) -> Option<fs::File> {
    let file = fast_io::open_basis_nofollow(path).ok()?;
    if file.metadata().ok()?.is_file() {
        Some(file)
    } else {
        None
    }
}

/// Process-wide policy set by the `--checksum-threads` CLI flag, overriding the
/// bench-validated default (parallel-on above the threshold).
///
/// Installed once at CLI startup via [`set_checksum_threads_policy`]. Because
/// the basis signature is wire-visible and byte-identical regardless of thread
/// count, this is a pure local performance knob - it is never forwarded to the
/// upstream server as an argument, exactly like `--rayon-threads`.
static CHECKSUM_THREADS_POLICY: OnceLock<ChecksumThreadsPolicy> = OnceLock::new();

/// Resolved `--checksum-threads` behaviour for basis-signature hashing.
///
/// `auto`/`0` fans per-block checksums across the rayon pool for baseses at or
/// above [`PARALLEL_THRESHOLD_BYTES`]; `1` forces the sequential path; `N > 1`
/// is parallel with the rayon pool capped to `N` (the cap is applied by the CLI
/// via the shared `--rayon-threads` mechanism, so this variant only records the
/// parallel intent here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumThreadsPolicy {
    /// Parallel above the threshold using the available rayon pool.
    Auto,
    /// Force sequential hashing regardless of basis size.
    Sequential,
}

impl ChecksumThreadsPolicy {
    /// Whether this policy selects the parallel windowed generator.
    #[must_use]
    const fn is_parallel(self) -> bool {
        matches!(self, Self::Auto)
    }
}

/// Installs the process-wide `--checksum-threads` policy.
///
/// Called once from CLI startup before any transfer begins. Later calls are
/// ignored (the first policy wins), matching the write-once semantics of the
/// other startup thread tunables.
pub fn set_checksum_threads_policy(policy: ChecksumThreadsPolicy) {
    let _ = CHECKSUM_THREADS_POLICY.set(policy);
}

/// Resolves whether the parallel windowed generator should be used.
///
/// Precedence: an explicit `--checksum-threads` policy wins; otherwise the
/// `OC_RSYNC_PARALLEL_CHECKSUM` env var is consulted as a documented override
/// (`0`/`false`/`no` forces sequential); absent both, the bench-validated
/// default is parallel-on. The wire output is byte-identical either way.
fn parallel_checksum_enabled() -> bool {
    if let Some(policy) = CHECKSUM_THREADS_POLICY.get() {
        return policy.is_parallel();
    }
    static ENV_DEFAULT: OnceLock<bool> = OnceLock::new();
    *ENV_DEFAULT.get_or_init(|| {
        std::env::var("OC_RSYNC_PARALLEL_CHECKSUM")
            .map(|v| {
                !matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "0" | "false" | "no" | "off"
                )
            })
            .unwrap_or(true)
    })
}

/// Computes the basis signature, choosing the bounded-memory parallel windowed
/// generator when `parallel` is set and the basis is large enough to amortise
/// the rayon scheduling overhead (>= [`PARALLEL_THRESHOLD_BYTES`]). Output is
/// byte-identical to the sequential path regardless of which branch is taken -
/// per-block sums depend only on the block bytes, and both paths emit blocks in
/// strict index order.
fn compute_basis_signature<R: std::io::Read>(
    reader: R,
    basis_size: u64,
    layout: SignatureLayout,
    algorithm: SignatureAlgorithm,
    parallel: bool,
) -> Result<FileSignature, SignatureError> {
    if parallel && basis_size >= PARALLEL_THRESHOLD_BYTES {
        generate_file_signature_windowed(reader, layout, algorithm)
    } else {
        generate_file_signature(reader, layout, algorithm)
    }
}

/// Finds a basis file for delta transfer using the provided configuration.
///
/// Search order:
/// 1. Exact file at destination path
/// 2. Reference directories (in order provided)
/// 3. Fuzzy matching in destination directory (if enabled)
///
/// # Upstream Reference
///
/// - `generator.c:1450` - Basis file selection in `recv_generator()`
/// - `generator.c:1580` - Fuzzy matching via `find_fuzzy_basis()`
/// - `generator.c:1400` - Reference directory checking
pub fn find_basis_file_with_config(config: &BasisFileConfig<'_>) -> BasisFileResult {
    // Upstream `generator.c:1962`: when `whole_file` is set, no basis file
    // is used - the entire file is sent as literals.
    if config.whole_file {
        return BasisFileResult::EMPTY;
    }

    let sig_config = SignatureGenerationConfig::from_basis_config(config);

    // Try sources in priority order: exact match -> reference dirs -> fuzzy ->
    // partial-dir. The exact destination basis is reported to the sender as
    // FNAMECMP_FNAME (no basis-type byte).
    if let Some((file, size, path)) = try_open_file(config.file_path) {
        return generate_basis_signature(
            file,
            size,
            path,
            protocol::FnameCmpType::Fname,
            None,
            sig_config,
        );
    }

    // upstream: generator.c:1054 - when the destination is absent, a basis found
    // in reference dir j (`--compare-dest`/`--copy-dest`/`--link-dest`) whose
    // content differs is tagged FNAMECMP_BASIS_DIR_LOW + j. That sets
    // ITEM_BASIS_TYPE_FOLLOWS (generator.c:1943) so the peer reads the trailing
    // basis-dir index byte; no xname follows (a basis-dir tag is below
    // FNAMECMP_FUZZY, generator.c:1945).
    if let Some((file, size, path, index)) =
        try_reference_directories(config.relative_path, config.reference_directories)
    {
        return generate_basis_signature(
            file,
            size,
            path,
            basis_dir_fnamecmp_type(index),
            None,
            sig_config,
        );
    }

    // upstream: generator.c:861,903,1945-1948 - a fuzzy match is tagged
    // FNAMECMP_FUZZY + i (0x83 for the dest dir, 0x83 + k + 1 for reference dir
    // k) and its basename is sent as the ITEM_XNAME_FOLLOWS vstring so the peer's
    // receiver can open the same basis.
    if config.fuzzy_level > 0
        && let Some(fuzzy) = try_fuzzy_match(
            config.relative_path,
            config.dest_dir,
            config.target_size,
            config.target_mtime,
            config.fuzzy_level,
            config.reference_directories,
        )
    {
        return generate_basis_signature(
            fuzzy.file,
            fuzzy.size,
            fuzzy.path,
            fuzzy.fnamecmp_type,
            fuzzy.xname,
            sig_config,
        );
    }

    // upstream: generator.c:1759-1765 - when the destination stat fails and
    // --partial-dir is set, fall back to the same-named regular file inside
    // the partial directory as the delta basis and tag it FNAMECMP_PARTIAL_DIR
    // (prepare_to_open at generator.c:1850-1855).
    if let Some(dir) = config.partial_dir
        && let Some(partial) = crate::temp_guard::partial_dir_fname(config.file_path, dir)
        && let Some((file, size, path)) = try_open_file(&partial)
    {
        return generate_basis_signature(
            file,
            size,
            path,
            protocol::FnameCmpType::PartialDir,
            None,
            sig_config,
        );
    }

    BasisFileResult::EMPTY
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn parallel_basis_signature_matches_sequential() {
        // The opt-in parallel path's signature goes over the wire to the
        // sender; any divergence from the sequential path would corrupt delta
        // reconstruction. Size exceeds PARALLEL_THRESHOLD_BYTES so the parallel
        // branch is actually exercised.
        use std::io::Cursor;
        let size = (PARALLEL_THRESHOLD_BYTES + 4096) as usize;
        let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let layout = calculate_signature_layout(SignatureLayoutParams::new(
            size as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        ))
        .expect("layout");
        let sequential = compute_basis_signature(
            Cursor::new(data.clone()),
            size as u64,
            layout,
            SignatureAlgorithm::Md4,
            false,
        )
        .expect("sequential signature");
        let parallel = compute_basis_signature(
            Cursor::new(data),
            size as u64,
            layout,
            SignatureAlgorithm::Md4,
            true,
        )
        .expect("parallel signature");
        assert_eq!(
            sequential, parallel,
            "parallel basis signature diverged from sequential",
        );
    }

    #[test]
    fn checksum_threads_policy_maps_to_parallel_flag() {
        // The `--checksum-threads` policy must map to exactly the parallel
        // gate: auto -> parallel, sequential -> sequential. A regression here
        // would silently (dis)engage the parallel windowed generator.
        assert!(ChecksumThreadsPolicy::Auto.is_parallel());
        assert!(!ChecksumThreadsPolicy::Sequential.is_parallel());
    }

    #[test]
    fn checksum_threads_policy_paths_are_byte_identical() {
        // The `--checksum-threads` flag selects between the parallel windowed
        // generator (Auto) and the sequential generator (Sequential). Because
        // the basis signature is wire-visible, both policies MUST yield a
        // byte-identical FileSignature - otherwise the flag would corrupt
        // delta reconstruction depending on thread count. Size exceeds the
        // threshold so Auto actually engages the parallel branch.
        use std::io::Cursor;
        let size = (PARALLEL_THRESHOLD_BYTES * 4 + 4096) as usize;
        let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let layout = calculate_signature_layout(SignatureLayoutParams::new(
            size as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        ))
        .expect("layout");

        let auto = compute_basis_signature(
            Cursor::new(data.clone()),
            size as u64,
            layout,
            SignatureAlgorithm::Md4,
            ChecksumThreadsPolicy::Auto.is_parallel(),
        )
        .expect("auto signature");
        let sequential = compute_basis_signature(
            Cursor::new(data),
            size as u64,
            layout,
            SignatureAlgorithm::Md4,
            ChecksumThreadsPolicy::Sequential.is_parallel(),
        )
        .expect("sequential signature");

        assert_eq!(
            auto, sequential,
            "--checksum-threads parallel and sequential paths must be byte-identical",
        );
    }

    /// Issue #715 regression (`symlink-dirlink-basis.test` test 1): when
    /// the destination directory is a symlink to a real directory, the
    /// receiver must open the basis file through the directory symlink.
    /// Upstream sets this expectation in `syscall.c:705 do_open_at()` -
    /// the dirname is resolved with normal symlink-following semantics.
    #[cfg(unix)]
    #[test]
    fn try_open_file_follows_directory_symlink() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tempdir");
        let real_dir = tmp.path().join("real-dir");
        std::fs::create_dir(&real_dir).expect("mkdir");
        std::fs::write(real_dir.join("basis"), b"basis-bytes").expect("write basis");

        symlink("real-dir", tmp.path().join("dir")).expect("symlink dir -> real-dir");

        let through_link = tmp.path().join("dir").join("basis");
        let (mut file, size, path) =
            try_open_file(&through_link).expect("basis open must succeed through dir symlink");
        assert_eq!(size, b"basis-bytes".len() as u64);
        assert_eq!(path, through_link);
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).expect("read");
        assert_eq!(buf, b"basis-bytes");
    }

    /// Security regression: the receiver must NOT follow a symlinked
    /// basename pointing outside the destination tree (matches
    /// upstream's `O_NOFOLLOW` on the basename in `do_open_at()` and
    /// `secure_relative_open()` - the CVE-class defence the upstream
    /// test header references). A symlinked basis basename is treated
    /// as "no basis", forcing whole-file transfer.
    #[cfg(unix)]
    #[test]
    fn try_open_file_rejects_symlinked_basename() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tempdir");
        let secret = tmp.path().join("secret");
        std::fs::write(&secret, b"do-not-leak").expect("write secret");

        let dir = tmp.path().join("dir");
        std::fs::create_dir(&dir).expect("mkdir dir");
        let basis = dir.join("basis");
        symlink(&secret, &basis).expect("symlink basis -> secret");

        assert!(
            try_open_file(&basis).is_none(),
            "symlinked basename must be refused so the receiver falls back to whole-file"
        );
    }

    /// Top-level basis path (`symlink-dirlink-basis.test` test 6): no
    /// dirname split needed. Equivalent to upstream's `if (!slash)
    /// return do_open(...)` short-circuit at `syscall.c:727`.
    #[test]
    fn try_open_file_handles_top_level_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let basis = tmp.path().join("topfile");
        std::fs::write(&basis, b"top-bytes").expect("write");

        let (mut file, size, path) = try_open_file(&basis).expect("top-level open");
        assert_eq!(size, b"top-bytes".len() as u64);
        assert_eq!(path, basis);
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).expect("read");
        assert_eq!(buf, b"top-bytes");
    }

    /// Reference directory lookup (`--copy-dest` / `--link-dest`) goes
    /// through the same hardened open path. A symlinked basename inside
    /// a reference directory must also be rejected.
    #[cfg(unix)]
    #[test]
    fn try_reference_directories_rejects_symlinked_basename() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tempdir");
        let ref_root = tmp.path().join("ref");
        std::fs::create_dir(&ref_root).expect("mkdir ref");
        let secret = tmp.path().join("secret");
        std::fs::write(&secret, b"leak").expect("write secret");

        let sub = ref_root.join("sub");
        std::fs::create_dir(&sub).expect("mkdir sub");
        symlink(&secret, sub.join("basis")).expect("symlink basis -> secret");

        let ref_dirs = vec![ReferenceDirectory {
            kind: crate::config::ReferenceDirectoryKind::Compare,
            path: ref_root.clone(),
        }];
        let rel = std::path::Path::new("sub").join("basis");

        assert!(
            try_reference_directories(&rel, &ref_dirs).is_none(),
            "reference-dir lookup must refuse symlinked basename"
        );
    }

    /// Byte-transparency gate for the default-mmap basis read: the signature
    /// computed by memory-mapping a large basis (`MmapReader::from_file`) MUST
    /// be byte-identical to the signature computed by reading the same basis
    /// through a raw `File`. The signature is what the sender matches against,
    /// so any divergence between the two read paths would change the delta and
    /// break interop. Size exceeds `MMAP_BASIS_THRESHOLD_BYTES` so the mmap
    /// branch is exercised.
    #[cfg(unix)]
    #[test]
    fn mmap_basis_signature_equals_buffered_file_signature() {
        use std::io::Write;

        let size = (MMAP_BASIS_THRESHOLD_BYTES + 4096) as usize;
        // Non-trivial, non-repeating bytes so blocks hash distinctly and a
        // mis-read (wrong offset/length) would surface as a signature diff.
        let data: Vec<u8> = (0..size).map(|i| ((i * 31 + 7) % 251) as u8).collect();

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("basis.bin");
        {
            let mut f = fs::File::create(&path).expect("create basis");
            f.write_all(&data).expect("write basis");
            f.flush().expect("flush");
        }

        let layout = calculate_signature_layout(SignatureLayoutParams::new(
            size as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        ))
        .expect("layout");

        let mapped = fast_io::MmapReader::from_file(
            fast_io::open_basis_nofollow(&path).expect("open basis"),
        )
        .expect("mmap basis");
        let via_mmap =
            compute_basis_signature(mapped, size as u64, layout, SignatureAlgorithm::Md4, false)
                .expect("mmap signature");

        let file = fast_io::open_basis_nofollow(&path).expect("open basis");
        let via_file =
            compute_basis_signature(file, size as u64, layout, SignatureAlgorithm::Md4, false)
                .expect("file signature");

        assert_eq!(
            via_mmap, via_file,
            "mmap basis signature diverged from buffered-file signature",
        );

        // Also assert against the parallel path so the mmap read composes with
        // both signature generators used in production.
        let mapped_par = fast_io::MmapReader::from_file(
            fast_io::open_basis_nofollow(&path).expect("open basis"),
        )
        .expect("mmap basis");
        let via_mmap_parallel = compute_basis_signature(
            mapped_par,
            size as u64,
            layout,
            SignatureAlgorithm::Md4,
            true,
        )
        .expect("mmap parallel signature");
        assert_eq!(
            via_mmap_parallel, via_file,
            "mmap parallel basis signature diverged from buffered-file signature",
        );
    }

    /// End-to-end check that `generate_basis_signature` (the production entry
    /// that now defaults to mmap for large baseses) yields the same signature
    /// the raw-`File` path produces. Guards against the dispatch wrapper picking
    /// a divergent branch.
    #[cfg(unix)]
    #[test]
    fn generate_basis_signature_mmap_default_matches_raw_file() {
        use std::io::Write;

        let size = (MMAP_BASIS_THRESHOLD_BYTES + 1234) as usize;
        let data: Vec<u8> = (0..size).map(|i| ((i * 17 + 3) % 251) as u8).collect();

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("basis.bin");
        {
            let mut f = fs::File::create(&path).expect("create basis");
            f.write_all(&data).expect("write basis");
            f.flush().expect("flush");
        }

        let cfg = SignatureGenerationConfig {
            protocol: ProtocolVersion::NEWEST,
            checksum_length: NonZeroU8::new(16).unwrap(),
            checksum_algorithm: SignatureAlgorithm::Md4,
            compat_flags: None,
        };

        // Production path (mmap default engaged because size >= threshold).
        let via_default = generate_basis_signature(
            fast_io::open_basis_nofollow(&path).expect("open basis"),
            size as u64,
            path.clone(),
            protocol::FnameCmpType::Fname,
            None,
            cfg,
        );

        // Reference: hash the same bytes straight through a raw `File`.
        let layout = calculate_signature_layout(SignatureLayoutParams::new(
            size as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        ))
        .expect("layout");
        let via_raw = compute_basis_signature(
            fast_io::open_basis_nofollow(&path).expect("open basis"),
            size as u64,
            layout,
            SignatureAlgorithm::Md4,
            parallel_checksum_enabled(),
        )
        .expect("raw signature");

        assert_eq!(
            via_default.signature.as_ref(),
            Some(&via_raw),
            "generate_basis_signature mmap-default diverged from raw-file signature",
        );
        assert_eq!(via_default.basis_path.as_deref(), Some(path.as_path()));
    }

    /// Issue #264 regression: when the destination is absent but an
    /// interrupted transfer left a same-named file under `--partial-dir`, the
    /// generator must select that partial file as the delta basis and tag it
    /// `FNAMECMP_PARTIAL_DIR` (0x81) so a delta - not a whole file - is sent.
    /// upstream: generator.c:1759-1765 + prepare_to_open (generator.c:1850-1855).
    #[test]
    fn partial_dir_basis_selected_when_dest_absent() {
        use std::io::Write;

        let tmp = tempfile::tempdir().expect("tempdir");
        let dest_dir = tmp.path();
        // Destination file does NOT exist; a partial file with prior content
        // lives under the relative partial-dir next to it.
        let partial_dir = dest_dir.join(".rsync-partial");
        std::fs::create_dir(&partial_dir).expect("mkdir partial-dir");
        let data: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
        {
            let mut f = fs::File::create(partial_dir.join("file.bin")).expect("create partial");
            f.write_all(&data).expect("write partial");
            f.flush().expect("flush");
        }

        let dest_file = dest_dir.join("file.bin");
        let rel = std::path::Path::new("file.bin");
        let partial_rel = std::path::Path::new(".rsync-partial");
        let config = BasisFileConfig {
            file_path: &dest_file,
            dest_dir,
            relative_path: rel,
            target_size: data.len() as u64,
            target_mtime: 0,
            fuzzy_level: 0,
            reference_directories: &[],
            partial_dir: Some(partial_rel),
            protocol: ProtocolVersion::NEWEST,
            checksum_length: NonZeroU8::new(16).unwrap(),
            checksum_algorithm: SignatureAlgorithm::Md4,
            whole_file: false,
            compat_flags: None,
        };

        let result = find_basis_file_with_config(&config);
        assert!(
            result.signature.is_some(),
            "a partial-dir basis must yield a signature (delta transfer), not EMPTY"
        );
        assert_eq!(
            result.basis_path.as_deref(),
            Some(partial_dir.join("file.bin").as_path()),
            "the basis path must point at the partial-dir file"
        );
        assert_eq!(
            result.fnamecmp_type,
            protocol::FnameCmpType::PartialDir,
            "the basis must be tagged FNAMECMP_PARTIAL_DIR so 0x81 is sent on the wire"
        );
    }

    /// The partial-dir fallback must not fire when the destination file itself
    /// exists: the ordinary destination basis wins and is tagged FNAMECMP_FNAME
    /// (no basis-type byte on the wire), preserving the pre-existing encoding.
    #[test]
    fn dest_basis_preferred_over_partial_dir() {
        use std::io::Write;

        let tmp = tempfile::tempdir().expect("tempdir");
        let dest_dir = tmp.path();
        let data: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        {
            let mut f = fs::File::create(dest_dir.join("file.bin")).expect("create dest");
            f.write_all(&data).expect("write dest");
            f.flush().expect("flush");
        }
        let partial_dir = dest_dir.join(".rsync-partial");
        std::fs::create_dir(&partial_dir).expect("mkdir partial-dir");
        std::fs::write(partial_dir.join("file.bin"), b"stale").expect("write partial");

        let dest_file = dest_dir.join("file.bin");
        let config = BasisFileConfig {
            file_path: &dest_file,
            dest_dir,
            relative_path: std::path::Path::new("file.bin"),
            target_size: data.len() as u64,
            target_mtime: 0,
            fuzzy_level: 0,
            reference_directories: &[],
            partial_dir: Some(std::path::Path::new(".rsync-partial")),
            protocol: ProtocolVersion::NEWEST,
            checksum_length: NonZeroU8::new(16).unwrap(),
            checksum_algorithm: SignatureAlgorithm::Md4,
            whole_file: false,
            compat_flags: None,
        };

        let result = find_basis_file_with_config(&config);
        assert_eq!(result.basis_path.as_deref(), Some(dest_file.as_path()));
        assert_eq!(result.fnamecmp_type, protocol::FnameCmpType::Fname);
    }

    /// A `--fuzzy` match found in the destination directory must be selected as
    /// a delta basis (not a whole-file send) AND advertised as FNAMECMP_FUZZY
    /// (0x83) with the basis basename as the xname, so a remote peer opens the
    /// same basis. upstream: generator.c:1785,1945-1948.
    #[test]
    fn fuzzy_match_tagged_fuzzy_with_basename_xname() {
        use std::io::Write;

        let tmp = tempfile::tempdir().expect("tempdir");
        let dest_dir = tmp.path();
        // Target "data.txt" is absent; a same-suffix candidate "data2.txt" sits
        // beside it. The shared ".txt" suffix keeps the fuzzy distance under the
        // cap (upstream weights suffix mismatches x10, generator.c:894).
        let data: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
        {
            let mut f = fs::File::create(dest_dir.join("data2.txt")).expect("create candidate");
            f.write_all(&data).expect("write candidate");
            f.flush().expect("flush");
        }

        let dest_file = dest_dir.join("data.txt");
        let config = BasisFileConfig {
            file_path: &dest_file,
            dest_dir,
            relative_path: std::path::Path::new("data.txt"),
            target_size: data.len() as u64,
            target_mtime: 0,
            fuzzy_level: 1,
            reference_directories: &[],
            partial_dir: None,
            protocol: ProtocolVersion::NEWEST,
            checksum_length: NonZeroU8::new(16).unwrap(),
            checksum_algorithm: SignatureAlgorithm::Md4,
            whole_file: false,
            compat_flags: None,
        };

        let result = find_basis_file_with_config(&config);
        assert!(
            result.signature.is_some(),
            "a fuzzy basis must yield a signature (delta transfer), not EMPTY"
        );
        assert_eq!(
            result.basis_path.as_deref(),
            Some(dest_dir.join("data2.txt").as_path()),
            "the basis path must point at the fuzzy candidate"
        );
        assert_eq!(
            result.fnamecmp_type,
            protocol::FnameCmpType::Fuzzy(0),
            "a dest-dir fuzzy match is FNAMECMP_FUZZY + 0 (0x83)"
        );
        assert_eq!(
            result.xname.as_deref(),
            Some(b"data2.txt".as_slice()),
            "the xname is the basis basename only, not a path"
        );
    }

    /// #204: with the destination absent, a basis found in reference dir `j`
    /// (`--compare-dest`/`--copy-dest`/`--link-dest`) whose content differs is
    /// selected as a delta basis and tagged FNAMECMP_BASIS_DIR_LOW + j
    /// (`BasisDir(j)`), so ITEM_BASIS_TYPE_FOLLOWS + the index byte go on the
    /// wire - matching a real upstream generator - and NO xname follows.
    /// upstream: generator.c:1054, generator.c:1943-1945.
    #[test]
    fn reference_dir_basis_tagged_basis_dir_index() {
        use std::io::Write;

        let tmp = tempfile::tempdir().expect("tempdir");
        let dest_dir = tmp.path().join("dest");
        std::fs::create_dir(&dest_dir).expect("mkdir dest");
        // Two reference dirs; the target lives only in the SECOND one, so the
        // emitted tag must be BasisDir(1), proving the index is threaded through
        // rather than hard-coded to 0.
        let ref0 = tmp.path().join("ref0");
        let ref1 = tmp.path().join("ref1");
        std::fs::create_dir(&ref0).expect("mkdir ref0");
        std::fs::create_dir(&ref1).expect("mkdir ref1");
        let data: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
        {
            let mut f = fs::File::create(ref1.join("file.bin")).expect("create ref basis");
            f.write_all(&data).expect("write ref basis");
            f.flush().expect("flush");
        }

        let dest_file = dest_dir.join("file.bin");
        let reference_directories = vec![
            crate::config::ReferenceDirectory {
                kind: crate::config::ReferenceDirectoryKind::Compare,
                path: ref0.clone(),
            },
            crate::config::ReferenceDirectory {
                kind: crate::config::ReferenceDirectoryKind::Compare,
                path: ref1.clone(),
            },
        ];
        let config = BasisFileConfig {
            file_path: &dest_file,
            dest_dir: &dest_dir,
            relative_path: std::path::Path::new("file.bin"),
            target_size: data.len() as u64,
            target_mtime: 0,
            fuzzy_level: 0,
            reference_directories: &reference_directories,
            partial_dir: None,
            protocol: ProtocolVersion::NEWEST,
            checksum_length: NonZeroU8::new(16).unwrap(),
            checksum_algorithm: SignatureAlgorithm::Md4,
            whole_file: false,
            compat_flags: None,
        };

        let result = find_basis_file_with_config(&config);
        assert!(
            result.signature.is_some(),
            "a reference-dir basis must yield a delta signature, not EMPTY"
        );
        assert_eq!(
            result.basis_path.as_deref(),
            Some(ref1.join("file.bin").as_path()),
            "the basis path must point at the reference-dir file"
        );
        assert_eq!(
            result.fnamecmp_type,
            protocol::FnameCmpType::BasisDir(1),
            "reference dir index 1 must be tagged FNAMECMP_BASIS_DIR_LOW + 1"
        );
        assert_eq!(
            result.fnamecmp_type.to_wire(),
            0x01,
            "BasisDir(1) encodes to the wire byte 0x01"
        );
        assert!(
            result.xname.is_none(),
            "a basis-dir tag is below FNAMECMP_FUZZY, so no xname follows"
        );
    }

    /// #205: a `-yy` fuzzy hit sourced from reference dir `k` must be tagged
    /// FNAMECMP_FUZZY + (k + 1) (`Fuzzy(k + 1)`; the dest dir is fuzzy-index 0)
    /// with the basis basename as the xname, so a real upstream peer reads the
    /// 0x84 tag plus the vstring. upstream: generator.c:861,903,1945-1948.
    #[test]
    fn reference_dir_fuzzy_tagged_fuzzy_index_with_xname() {
        use std::io::Write;

        let tmp = tempfile::tempdir().expect("tempdir");
        let dest_dir = tmp.path().join("dest");
        std::fs::create_dir(&dest_dir).expect("mkdir dest");
        // Target "data.txt" is absent from dest and from the reference dir (so
        // the exact reference-dir basis does not fire); a same-suffix sibling
        // "data2.txt" sits in the single reference dir, making it the fuzzy hit
        // at fuzzy-index 1 (dest dir is 0).
        let ref_dir = tmp.path().join("ref");
        std::fs::create_dir(&ref_dir).expect("mkdir ref");
        let data: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
        {
            let mut f = fs::File::create(ref_dir.join("data2.txt")).expect("create candidate");
            f.write_all(&data).expect("write candidate");
            f.flush().expect("flush");
        }

        let dest_file = dest_dir.join("data.txt");
        let reference_directories = vec![crate::config::ReferenceDirectory {
            kind: crate::config::ReferenceDirectoryKind::Compare,
            path: ref_dir.clone(),
        }];
        let config = BasisFileConfig {
            file_path: &dest_file,
            dest_dir: &dest_dir,
            relative_path: std::path::Path::new("data.txt"),
            target_size: data.len() as u64,
            target_mtime: 0,
            fuzzy_level: 2,
            reference_directories: &reference_directories,
            partial_dir: None,
            protocol: ProtocolVersion::NEWEST,
            checksum_length: NonZeroU8::new(16).unwrap(),
            checksum_algorithm: SignatureAlgorithm::Md4,
            whole_file: false,
            compat_flags: None,
        };

        let result = find_basis_file_with_config(&config);
        assert!(
            result.signature.is_some(),
            "a reference-dir fuzzy basis must yield a delta signature, not EMPTY"
        );
        assert_eq!(
            result.basis_path.as_deref(),
            Some(ref_dir.join("data2.txt").as_path()),
            "the basis path must point at the reference-dir fuzzy candidate"
        );
        assert_eq!(
            result.fnamecmp_type,
            protocol::FnameCmpType::Fuzzy(1),
            "a reference-dir (index 0) fuzzy match is FNAMECMP_FUZZY + 1"
        );
        assert_eq!(
            result.fnamecmp_type.to_wire(),
            0x84,
            "Fuzzy(1) encodes to the wire byte 0x84"
        );
        assert_eq!(
            result.xname.as_deref(),
            Some(b"data2.txt".as_slice()),
            "the xname is the basis basename only, not a path"
        );
    }

    /// With `--fuzzy` off, the same layout finds no basis: the fuzzy candidate is
    /// never consulted, so the result is EMPTY (whole-file send) with no
    /// FNAMECMP_FUZZY advertisement. Guards the strict no-op when fuzzy is off.
    #[test]
    fn fuzzy_off_ignores_candidate() {
        use std::io::Write;

        let tmp = tempfile::tempdir().expect("tempdir");
        let dest_dir = tmp.path();
        let data: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        {
            let mut f = fs::File::create(dest_dir.join("data.bak")).expect("create candidate");
            f.write_all(&data).expect("write candidate");
            f.flush().expect("flush");
        }

        let dest_file = dest_dir.join("data.txt");
        let config = BasisFileConfig {
            file_path: &dest_file,
            dest_dir,
            relative_path: std::path::Path::new("data.txt"),
            target_size: data.len() as u64,
            target_mtime: 0,
            fuzzy_level: 0,
            reference_directories: &[],
            partial_dir: None,
            protocol: ProtocolVersion::NEWEST,
            checksum_length: NonZeroU8::new(16).unwrap(),
            checksum_algorithm: SignatureAlgorithm::Md4,
            whole_file: false,
            compat_flags: None,
        };

        let result = find_basis_file_with_config(&config);
        assert!(
            result.is_empty(),
            "fuzzy-off must not consult the candidate; result is a whole-file send"
        );
        assert_eq!(result.fnamecmp_type, protocol::FnameCmpType::Fname);
        assert!(result.xname.is_none());
    }

    /// Phase-2 redo must still send a checksum-based delta against the basis,
    /// not a whole-file literal resend. When a file fails its whole-file
    /// verification in phase 1, upstream re-queues it and regenerates the block
    /// signature with the FULL strong-checksum length rather than discarding the
    /// basis: `check_for_finished_files()` sets `csum_length = SUM_LENGTH` (16)
    /// around the redo `recv_generator()` call, and `sum_sizes_sqroot()` then
    /// takes the `csum_length == SUM_LENGTH` branch to emit a full-strength
    /// `s2length` instead of the phase-1 sqrt-reduced length.
    /// upstream: generator.c:2178 (`csum_length = SUM_LENGTH`),
    /// generator.c:739-740 (`s2length = max_s2length`), generator.c:2205
    /// (restore `SHORT_SUM_LENGTH`).
    ///
    /// The invariant this locks in: for an existing basis, the redo checksum
    /// length yields a non-EMPTY signature (a delta) whose strong sum is the
    /// full 16 bytes - never an EMPTY (whole-file) result and never the
    /// phase-1 short sum.
    #[test]
    fn phase2_redo_sends_full_checksum_delta_not_whole_file() {
        use std::io::Write;

        let tmp = tempfile::tempdir().expect("tempdir");
        let dest_dir = tmp.path();
        // The basis is the file present at the destination when the redo pass
        // runs (the receiver kept/committed it after phase 1). Size is chosen so
        // the phase-1 sqrt heuristic would pick a strong sum shorter than 16,
        // making the phase-2 upgrade to the full length observable.
        let data: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
        {
            let mut f = fs::File::create(dest_dir.join("file.bin")).expect("create dest");
            f.write_all(&data).expect("write dest");
            f.flush().expect("flush");
        }

        let dest_file = dest_dir.join("file.bin");
        let make_config = |checksum_length: NonZeroU8| BasisFileConfig {
            file_path: &dest_file,
            dest_dir,
            relative_path: std::path::Path::new("file.bin"),
            target_size: data.len() as u64,
            target_mtime: 0,
            fuzzy_level: 0,
            reference_directories: &[],
            partial_dir: None,
            protocol: ProtocolVersion::NEWEST,
            checksum_length,
            checksum_algorithm: SignatureAlgorithm::Md4,
            // Not --whole-file: the redo path must still search for a basis.
            whole_file: false,
            compat_flags: None,
        };

        let strong_len = |result: &BasisFileResult| -> u8 {
            result
                .signature
                .as_ref()
                .expect("redo must produce a delta signature, not a whole-file resend")
                .layout()
                .strong_sum_length()
                .get()
        };

        let phase1 =
            find_basis_file_with_config(&make_config(crate::receiver::PHASE1_CHECKSUM_LENGTH));
        let redo = find_basis_file_with_config(&make_config(crate::receiver::REDO_CHECKSUM_LENGTH));

        // Redo yields a real block signature (delta), never EMPTY (whole file).
        assert!(
            redo.signature.is_some(),
            "phase-2 redo must send a checksum-based delta, not a whole-file literal transfer"
        );
        assert_eq!(
            redo.fnamecmp_type,
            protocol::FnameCmpType::Fname,
            "the redo basis is the ordinary destination file (no wire basis-type byte)"
        );
        // The redo strong sum is the full 16 bytes, and never shorter than the
        // phase-1 sqrt-reduced length: this is the phase-2 collision-resistance
        // upgrade that lets the corrected delta pass verification.
        assert_eq!(
            strong_len(&redo),
            signature::block_size::MAX_SUM_LENGTH,
            "phase-2 redo must use the full strong-checksum length"
        );
        assert!(
            strong_len(&redo) >= strong_len(&phase1),
            "redo strong sum must not be shorter than the phase-1 signature"
        );
    }

    #[test]
    fn consecutive_match_cap_halves_strong_sum_length() {
        use std::io::Write;

        let data: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("basis.bin");
        {
            let mut f = fs::File::create(&path).expect("create");
            f.write_all(&data).expect("write");
            f.flush().expect("flush");
        }

        let strong_len = |cfg: SignatureGenerationConfig| -> u8 {
            generate_basis_signature(
                fast_io::open_basis_nofollow(&path).expect("open"),
                data.len() as u64,
                path.clone(),
                protocol::FnameCmpType::Fname,
                None,
                cfg,
            )
            .signature
            .as_ref()
            .expect("signature")
            .layout()
            .strong_sum_length()
            .get()
        };

        let base = SignatureGenerationConfig {
            protocol: ProtocolVersion::NEWEST,
            checksum_length: NonZeroU8::new(16).unwrap(),
            checksum_algorithm: SignatureAlgorithm::Md4,
            compat_flags: None,
        };

        // No flags -> full length (byte-identical to upstream).
        assert_eq!(strong_len(base), 16);

        // Iron invariant: flags WITHOUT the private CAP bit never shrink it,
        // even a rich set of standard flags.
        let no_cap = protocol::CompatibilityFlags::INC_RECURSE
            | protocol::CompatibilityFlags::SAFE_FILE_LIST
            | protocol::CompatibilityFlags::CHECKSUM_SEED_FIX;
        assert_eq!(
            strong_len(SignatureGenerationConfig {
                compat_flags: Some(no_cap),
                ..base
            }),
            16
        );

        // CAP present in the mutual flags -> halved. This is the ONLY input
        // that shrinks the strong-sum length.
        let with_cap = protocol::CompatibilityFlags::INC_RECURSE
            | protocol::CompatibilityFlags::CONSECUTIVE_MATCH;
        assert_eq!(
            strong_len(SignatureGenerationConfig {
                compat_flags: Some(with_cap),
                ..base
            }),
            8,
            "CAP_CONSECUTIVE_MATCH must halve the strong-sum length"
        );
    }

    #[test]
    fn s2length_capped_by_negotiated_digest_length() {
        use std::io::Write;
        use std::num::NonZeroU32;

        // WHY: `sum_head.s2length` and every per-block strong sum go on the wire.
        // Upstream caps s2length to `max_s2length = MIN(SUM_LENGTH, xfer_sum_len)`
        // (generator.c:705), so a short negotiated digest bounds the strong-sum
        // width. A larger s2length would make oc write a zero-padded sum wider
        // than the sender's checksum, desyncing the block-checksum stream.
        //
        // `phase_len` is the phase indicator: SHORT_SUM_LENGTH (2) for phase 1,
        // MAX_SUM_LENGTH (16) for the phase-2 redo.
        let sig = |file_size: u64,
                   block: Option<NonZeroU32>,
                   phase_len: u8,
                   algo: SignatureAlgorithm|
         -> u8 {
            let data: Vec<u8> = (0..file_size).map(|i| (i % 251) as u8).collect();
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join("basis.bin");
            {
                let mut f = fs::File::create(&path).expect("create");
                f.write_all(&data).expect("write");
                f.flush().expect("flush");
            }
            let cfg = SignatureGenerationConfig {
                protocol: ProtocolVersion::NEWEST,
                checksum_length: NonZeroU8::new(phase_len).unwrap(),
                checksum_algorithm: algo,
                compat_flags: None,
            };
            let params = SignatureLayoutParams::new(
                file_size,
                block,
                ProtocolVersion::NEWEST,
                cfg.checksum_length,
            );
            let uncapped = calculate_signature_layout(params)
                .expect("layout")
                .strong_sum_length()
                .get();
            let capped = generate_basis_signature(
                fast_io::open_basis_nofollow(&path).expect("open"),
                file_size,
                path.clone(),
                protocol::FnameCmpType::Fname,
                None,
                cfg,
            )
            .signature
            .as_ref()
            .expect("signature")
            .layout()
            .strong_sum_length()
            .get();
            let digest_cap = algo.digest_len() as u8;
            assert!(
                capped <= digest_cap,
                "s2length {capped} exceeds negotiated digest {digest_cap} \
                 (file={file_size}, phase_len={phase_len}, uncapped={uncapped})"
            );
            // Wide digests (>= MAX_SUM_LENGTH): the cap must be a strict no-op so
            // the SumHead stays byte-identical to upstream. Any short digest can
            // only shrink, never grow, the length.
            if digest_cap >= 16 {
                assert_eq!(
                    capped, uncapped,
                    "16-byte digest must leave s2length unchanged \
                     (file={file_size}, phase_len={phase_len})"
                );
            }
            capped
        };

        // Default 16-byte digests: byte-identical to upstream. Phase-2 pins 16;
        // phase-1 tracks the heuristic. The helper asserts capped == uncapped.
        assert_eq!(sig(4096, None, 16, SignatureAlgorithm::Md4), 16);
        sig(64 * 1024 * 1024, None, 2, SignatureAlgorithm::Md4);
        assert_eq!(
            sig(4096, None, 16, SignatureAlgorithm::Xxh3_128 { seed: 0 }),
            16
        );

        // Short 8-byte digest (XXH64 / XXH3-64): the cap actually reduces
        // s2length. The phase-2 redo computes MAX_SUM_LENGTH (16) uncapped, so
        // the negotiated digest bounds it to 8 - the case that would otherwise
        // put a zero-padded 16-byte strong sum on the wire.
        let xxh64 = SignatureAlgorithm::Xxh64 { seed: 0 };
        let xxh3_64 = SignatureAlgorithm::Xxh3 { seed: 0 };
        assert_eq!(
            sig(4096, None, 16, xxh64),
            8,
            "phase-2 redo must cap s2length at the 8-byte XXH64 digest"
        );
        assert_eq!(
            sig(256 * 1024 * 1024, None, 16, xxh3_64),
            8,
            "phase-2 redo on a large file must cap at the 8-byte XXH3-64 digest"
        );

        // Sweep file and block sizes: the wire s2length never exceeds the
        // negotiated digest for a short checksum, at any layout (asserted inside
        // the `sig` helper).
        for size in [512u64, 4096, 1 << 20, 1 << 24, 1 << 28] {
            for block in [None, NonZeroU32::new(700), NonZeroU32::new(8192)] {
                sig(size, block, 2, xxh64);
                sig(size, block, 16, xxh3_64);
            }
        }
    }
}
