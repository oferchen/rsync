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
}

impl BasisFileResult {
    /// Empty result when no basis file is found.
    pub(super) const EMPTY: Self = Self {
        signature: None,
        basis_path: None,
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
    /// Fuzzy matching level (0=off, 1=dest dir, 2=dest+reference dirs).
    ///
    /// Upstream: `options.c:764` - `fuzzy_basis` controls how many directory
    /// sources are searched. Level 1 searches only the destination directory.
    /// Level 2 also searches reference directories (`--compare-dest`,
    /// `--copy-dest`, `--link-dest`).
    pub fuzzy_level: u8,
    /// List of reference directories to check.
    pub reference_directories: &'a [ReferenceDirectory],
    /// Protocol version for signature generation.
    pub protocol: ProtocolVersion,
    /// Checksum truncation length.
    pub checksum_length: NonZeroU8,
    /// Algorithm for strong checksums.
    pub checksum_algorithm: engine::signature::SignatureAlgorithm,
    /// When true, skip basis file search entirely (upstream `--whole-file`).
    pub whole_file: bool,
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
}

impl SignatureGenerationConfig {
    /// Extracts signature generation config from a BasisFileConfig.
    fn from_basis_config(config: &BasisFileConfig<'_>) -> Self {
        Self {
            protocol: config.protocol,
            checksum_length: config.checksum_length,
            checksum_algorithm: config.checksum_algorithm,
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
    let fuzzy_match = fuzzy_matcher.find_fuzzy_basis(target_name, dest_dir, target_size)?;
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
    config: SignatureGenerationConfig,
) -> BasisFileResult {
    let params =
        SignatureLayoutParams::new(basis_size, None, config.protocol, config.checksum_length);

    let layout = match calculate_signature_layout(params) {
        Ok(layout) => layout,
        Err(_) => return BasisFileResult::EMPTY,
    };

    let parallel = parallel_checksum_enabled();
    match compute_basis_signature(
        basis_file,
        basis_size,
        layout,
        config.checksum_algorithm,
        parallel,
    ) {
        Ok(sig) => BasisFileResult {
            signature: Some(sig),
            basis_path: Some(basis_path),
        },
        Err(_) => BasisFileResult::EMPTY,
    }
}

/// Process-wide opt-in gate for parallel basis-signature generation.
///
/// Read once from `OC_RSYNC_PARALLEL_CHECKSUM` (truthy = `1` / `true` / `yes`,
/// case-insensitive) via a [`OnceLock`], matching the other `OC_RSYNC_*`
/// runtime probes. Default off: signature generation stays sequential and the
/// wire output is byte-identical either way. This is the interim opt-in until a
/// `--checksum-threads` flag plus adaptive backpressure land; gating keeps the
/// parallel path out of default builds until it is bench-validated.
fn parallel_checksum_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("OC_RSYNC_PARALLEL_CHECKSUM")
            .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false)
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

    // Try sources in priority order: exact match -> reference dirs -> fuzzy
    let basis = try_open_file(config.file_path)
        .or_else(|| try_reference_directories(config.relative_path, config.reference_directories))
        .or_else(|| {
            if config.fuzzy_level > 0 {
                try_fuzzy_match(
                    config.relative_path,
                    config.dest_dir,
                    config.target_size,
                    config.fuzzy_level,
                    config.reference_directories,
                )
            } else {
                None
            }
        });

    let Some((file, size, path)) = basis else {
        return BasisFileResult::EMPTY;
    };

    let sig_config = SignatureGenerationConfig::from_basis_config(config);
    generate_basis_signature(file, size, path, sig_config)
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
}
