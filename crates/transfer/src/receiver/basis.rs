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
    /// [`protocol::FnameCmpType::Fname`] for the ordinary destination basis (no wire byte)
    /// and [`protocol::FnameCmpType::PartialDir`] (`0x81`) when the basis was recovered
    /// from `--partial-dir` on a resume. upstream: generator.c:1759-1765,1853.
    pub fnamecmp_type: protocol::FnameCmpType,
}

impl BasisFileResult {
    /// Empty result when no basis file is found.
    pub(super) const EMPTY: Self = Self {
        signature: None,
        basis_path: None,
        fnamecmp_type: protocol::FnameCmpType::Fname,
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
/// path exists in each one. Returns the first match found. The basis open
/// goes through [`fast_io::open_basis_nofollow`], which follows directory
/// symlinks (so `--copy-dirlinks` continues to work) but refuses to follow
/// a symlinked leaf component, mirroring upstream `syscall.c:705`
/// (`do_open_at`).
///
/// # Upstream Reference
///
/// - `generator.c:1400` - Reference directory basis file lookup
/// - `syscall.c:705` (`do_open_at`) - dirname/basename split with
///   `O_NOFOLLOW` on the basename.
pub(super) fn try_reference_directories(
    relative_path: &std::path::Path,
    reference_directories: &[ReferenceDirectory],
) -> Option<(fs::File, u64, PathBuf)> {
    for ref_dir in reference_directories {
        let candidate = ref_dir.path.join(relative_path);
        if let Ok(file) = fast_io::open_basis_nofollow(&candidate) {
            if let Ok(meta) = file.metadata() {
                if meta.is_file() {
                    return Some((file, meta.len(), candidate));
                }
            }
        }
    }
    None
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

/// Attempts fuzzy matching to find a similar basis file.
///
/// For level 1, searches only the destination directory. For level 2,
/// also searches reference directories (`--compare-dest`, `--copy-dest`,
/// `--link-dest`) by passing them as fuzzy basis dirs to the matcher.
///
/// # Upstream Reference
///
/// - `generator.c:1580` - Fuzzy matching via `find_fuzzy_basis()`
/// - `options.c:2120` - `fuzzy_basis = basis_dir_cnt + 1` for level 2
fn try_fuzzy_match(
    relative_path: &std::path::Path,
    dest_dir: &std::path::Path,
    target_size: u64,
    target_mtime: i64,
    fuzzy_level: u8,
    reference_directories: &[ReferenceDirectory],
) -> Option<(fs::File, u64, PathBuf)> {
    let target_name = relative_path.file_name()?;

    // Build the search directory for reference dirs: join each reference
    // dir base with the target file's parent directory, mirroring upstream
    // generator.c where fuzzy_dirlist[i] = get_dirlist(basis_dir[i-1]/dn).
    let parent_dir = relative_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new(""));
    let basis_dirs: Vec<PathBuf> = if fuzzy_level >= 2 {
        reference_directories
            .iter()
            .map(|rd| rd.path.join(parent_dir))
            .filter(|p| p.is_dir())
            .collect()
    } else {
        Vec::new()
    };

    let fuzzy_matcher = FuzzyMatcher::with_level(fuzzy_level).with_fuzzy_basis_dirs(basis_dirs);
    let fuzzy_match =
        fuzzy_matcher.find_fuzzy_basis(target_name, dest_dir, target_size, Some(target_mtime))?;
    // upstream: generator.c:1775 - announce the selected fuzzy basis at
    // FUZZY,1 the moment the matcher returns a candidate, before we attempt
    // to open it.
    trace_fuzzy_basis_selected(
        &relative_path.display().to_string(),
        &fuzzy_match.path.display().to_string(),
    );
    try_open_file(&fuzzy_match.path)
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
    config: SignatureGenerationConfig,
) -> BasisFileResult {
    let params =
        SignatureLayoutParams::new(basis_size, None, config.protocol, config.checksum_length);

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
    // Upstream `generator.c:1949`: when `whole_file` is set, no basis file
    // is used - the entire file is sent as literals.
    if config.whole_file {
        return BasisFileResult::EMPTY;
    }

    // Try sources in priority order: exact match -> reference dirs -> fuzzy.
    // The ordinary destination / reference-dir / fuzzy basis is reported to the
    // sender as FNAMECMP_FNAME (no basis-type byte), matching the pre-existing
    // wire encoding for those cases.
    let basis = try_open_file(config.file_path)
        .or_else(|| try_reference_directories(config.relative_path, config.reference_directories))
        .or_else(|| {
            if config.fuzzy_level > 0 {
                try_fuzzy_match(
                    config.relative_path,
                    config.dest_dir,
                    config.target_size,
                    config.target_mtime,
                    config.fuzzy_level,
                    config.reference_directories,
                )
            } else {
                None
            }
        });

    let (fnamecmp_type, basis) = match basis {
        Some(found) => (protocol::FnameCmpType::Fname, Some(found)),
        // upstream: generator.c:1759-1765 - when the destination stat fails and
        // --partial-dir is set, fall back to the same-named regular file inside
        // the partial directory as the delta basis and tag it
        // FNAMECMP_PARTIAL_DIR (prepare_to_open at generator.c:1850-1855).
        None => match config.partial_dir {
            Some(dir) => match crate::temp_guard::partial_dir_fname(config.file_path, dir) {
                Some(partial) => (protocol::FnameCmpType::PartialDir, try_open_file(&partial)),
                None => (protocol::FnameCmpType::Fname, None),
            },
            None => (protocol::FnameCmpType::Fname, None),
        },
    };

    let Some((file, size, path)) = basis else {
        return BasisFileResult::EMPTY;
    };

    let sig_config = SignatureGenerationConfig::from_basis_config(config);
    generate_basis_signature(file, size, path, fnamecmp_type, sig_config)
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
}
