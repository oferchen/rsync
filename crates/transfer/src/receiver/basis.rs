//! Basis file search and signature generation.
//!
//! Implements the search strategy for finding a suitable basis file for delta
//! transfer (exact match, reference directories, fuzzy matching) and generates
//! the file signature used by the sender to compute deltas.

use std::fs;
use std::num::NonZeroU8;
use std::path::PathBuf;

use engine::delta::{SignatureLayoutParams, calculate_signature_layout};
use engine::fuzzy::FuzzyMatcher;
use engine::signature::{FileSignature, generate_file_signature};
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
/// path exists in each one. Returns the first match found.
///
/// # Upstream Reference
///
/// - `generator.c:1400` - Reference directory basis file lookup
pub(super) fn try_reference_directories(
    relative_path: &std::path::Path,
    reference_directories: &[ReferenceDirectory],
) -> Option<(fs::File, u64, PathBuf)> {
    for ref_dir in reference_directories {
        let candidate = ref_dir.path.join(relative_path);
        if let Ok(file) = fs::File::open(&candidate) {
            if let Ok(meta) = file.metadata() {
                if meta.is_file() {
                    return Some((file, meta.len(), candidate));
                }
            }
        }
    }
    None
}

/// Opens a file and returns it with metadata.
///
/// Returns the file handle, size, and path if successful.
fn try_open_file(path: &std::path::Path) -> Option<(fs::File, u64, PathBuf)> {
    let file = fs::File::open(path).ok()?;
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

    match generate_file_signature(basis_file, layout, config.checksum_algorithm) {
        Ok(sig) => BasisFileResult {
            signature: Some(sig),
            basis_path: Some(basis_path),
        },
        Err(_) => BasisFileResult::EMPTY,
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
#[must_use]
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
