//! Recursive directory-tree comparison for upstream-compat ports.
//!
//! `DirDiff` is the harness primitive that backs the runtests.py edge-case
//! ports specified in `docs/design/uts-nextest-edge-b-test-harness.md`. It
//! mirrors upstream's `rsync_ls_lR` + `diff -r` pair: after a transfer, a
//! port asserts the destination tree equals a known-good tree under a chosen
//! set of attributes (content, mode, owner, mtime, symlink target).
//!
//! The comparison is standard-library only and deterministic (entries are
//! visited in sorted order), so a mismatch prints the same diff regardless
//! of filesystem enumeration order. xattr and ACL comparison are declared in
//! [`DirDiffOptions`] but not yet wired to a backend; requesting them returns
//! an error rather than silently passing, so a test can never believe it
//! checked an attribute the harness ignored.

use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Streaming byte-compare threshold. Files at or below this size are read
/// fully and compared with a slice equality; larger files stream in chunks
/// so a multi-gigabyte basis file never forces a full in-memory copy.
const STREAM_THRESHOLD: u64 = 1 << 20;

/// Attributes to compare between two trees.
///
/// Defaults compare structure only (presence of every path). Enable
/// individual checks or use the [`DirDiffOptions::structural`] /
/// [`DirDiffOptions::archive`] presets that mirror upstream `checkit()` and
/// `-a` semantics.
#[derive(Clone, Copy, Debug, Default)]
pub struct DirDiffOptions {
    /// Compare regular-file byte content.
    pub check_content: bool,
    /// Compare the low 12 permission bits (`0o7777`).
    pub check_mode: bool,
    /// Compare modification time (whole seconds).
    pub check_mtime: bool,
    /// Compare `(uid, gid)` ownership. Unix only.
    pub check_owner: bool,
    /// Compare access time. Not yet implemented; requesting it errors.
    pub check_atime: bool,
    /// Compare extended attributes. Not yet implemented; requesting it errors.
    pub check_xattr: bool,
    /// Compare POSIX ACLs. Not yet implemented; requesting it errors.
    pub check_acl: bool,
}

impl DirDiffOptions {
    /// Upstream `checkit()` default: structure plus file content and mode.
    ///
    /// This is the "did every file arrive with the right bytes and bits"
    /// check most non-archive ports need.
    #[must_use]
    pub fn structural() -> Self {
        Self {
            check_content: true,
            check_mode: true,
            ..Self::default()
        }
    }

    /// Archive mode (`-a`): content, mode, owner, and mtime.
    ///
    /// Symlink targets are always compared literally (never dereferenced),
    /// matching `rsync -a` which recreates the link rather than following it.
    #[must_use]
    pub fn archive() -> Self {
        Self {
            check_content: true,
            check_mode: true,
            check_mtime: true,
            check_owner: true,
            ..Self::default()
        }
    }

    fn unsupported(self) -> Option<&'static str> {
        if self.check_atime {
            Some("check_atime")
        } else if self.check_xattr {
            Some("check_xattr")
        } else if self.check_acl {
            Some("check_acl")
        } else {
            None
        }
    }
}

/// Recursive tree comparator. See the module docs for semantics.
pub struct DirDiff;

impl DirDiff {
    /// Compare `expected` against `actual` under `opts`.
    ///
    /// Returns `Ok(())` when the trees are equivalent. Returns a
    /// [`DirDiffMismatch`] enumerating every difference otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`DirDiffError::Unsupported`] if `opts` requests an attribute
    /// the harness does not yet compare (atime, xattr, acl), and
    /// [`DirDiffError::Io`] if either tree cannot be traversed. Both are
    /// distinct from a content mismatch so a test never mistakes an
    /// infrastructure failure for a clean tree.
    pub fn compare(
        expected: &Path,
        actual: &Path,
        opts: DirDiffOptions,
    ) -> Result<Result<(), DirDiffMismatch>, DirDiffError> {
        if let Some(attr) = opts.unsupported() {
            return Err(DirDiffError::Unsupported(attr));
        }

        let mut differences = Vec::new();
        compare_dir(expected, actual, Path::new(""), opts, &mut differences)?;

        if differences.is_empty() {
            Ok(Ok(()))
        } else {
            Ok(Err(DirDiffMismatch { differences }))
        }
    }
}

/// Infrastructure failure while comparing, distinct from a tree mismatch.
#[derive(Debug)]
pub enum DirDiffError {
    /// An option requested an attribute the harness cannot yet compare.
    Unsupported(&'static str),
    /// A filesystem error occurred during traversal.
    Io(io::Error),
}

impl std::fmt::Display for DirDiffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DirDiffError::Unsupported(attr) => {
                write!(f, "DirDiff option {attr} is not yet implemented")
            }
            DirDiffError::Io(e) => write!(f, "DirDiff traversal failed: {e}"),
        }
    }
}

impl std::error::Error for DirDiffError {}

impl From<io::Error> for DirDiffError {
    fn from(e: io::Error) -> Self {
        DirDiffError::Io(e)
    }
}

/// A non-empty set of differences between two trees.
#[derive(Debug)]
pub struct DirDiffMismatch {
    /// Every difference found, in sorted-path order.
    pub differences: Vec<DirDiffEntry>,
}

impl DirDiffMismatch {
    /// Render an upstream-`diff -r`-style message for a test panic.
    #[must_use]
    pub fn into_panic_message(self) -> String {
        let mut out = format!(
            "directory trees differ ({} differences):\n",
            self.differences.len()
        );
        for entry in &self.differences {
            let _ = writeln!(out, "  {entry}");
        }
        out
    }
}

impl std::fmt::Display for DirDiffMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} differences", self.differences.len())
    }
}

/// A single tree difference.
#[derive(Debug, PartialEq, Eq)]
pub enum DirDiffEntry {
    /// Present in `expected`, absent from `actual`.
    OnlyInExpected(PathBuf),
    /// Present in `actual`, absent from `expected`.
    OnlyInActual(PathBuf),
    /// Same path, different file type (e.g. file vs directory).
    TypeMismatch {
        /// Relative path.
        path: PathBuf,
        /// Type in the expected tree.
        expected: &'static str,
        /// Type in the actual tree.
        actual: &'static str,
    },
    /// Regular-file byte content differs.
    ContentMismatch {
        /// Relative path.
        path: PathBuf,
        /// Byte length in the expected tree.
        expected_len: u64,
        /// Byte length in the actual tree.
        actual_len: u64,
    },
    /// Permission bits differ.
    ModeMismatch {
        /// Relative path.
        path: PathBuf,
        /// Expected `0o7777` bits.
        expected: u32,
        /// Actual `0o7777` bits.
        actual: u32,
    },
    /// `(uid, gid)` differ.
    OwnerMismatch {
        /// Relative path.
        path: PathBuf,
        /// Expected `(uid, gid)`.
        expected: (u32, u32),
        /// Actual `(uid, gid)`.
        actual: (u32, u32),
    },
    /// Modification time (whole seconds) differs.
    MtimeMismatch {
        /// Relative path.
        path: PathBuf,
        /// Expected seconds since the Unix epoch.
        expected: i64,
        /// Actual seconds since the Unix epoch.
        actual: i64,
    },
    /// Symlink target differs.
    SymlinkMismatch {
        /// Relative path.
        path: PathBuf,
        /// Expected link target.
        expected: PathBuf,
        /// Actual link target.
        actual: PathBuf,
    },
}

impl std::fmt::Display for DirDiffEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DirDiffEntry::OnlyInExpected(p) => write!(f, "only in expected: {}", p.display()),
            DirDiffEntry::OnlyInActual(p) => write!(f, "only in actual: {}", p.display()),
            DirDiffEntry::TypeMismatch {
                path,
                expected,
                actual,
            } => write!(
                f,
                "type mismatch at {}: expected {expected}, actual {actual}",
                path.display()
            ),
            DirDiffEntry::ContentMismatch {
                path,
                expected_len,
                actual_len,
            } => write!(
                f,
                "content mismatch at {}: expected {expected_len} bytes, actual {actual_len} bytes",
                path.display()
            ),
            DirDiffEntry::ModeMismatch {
                path,
                expected,
                actual,
            } => write!(
                f,
                "mode mismatch at {}: expected {expected:04o}, actual {actual:04o}",
                path.display()
            ),
            DirDiffEntry::OwnerMismatch {
                path,
                expected,
                actual,
            } => write!(
                f,
                "owner mismatch at {}: expected {}:{}, actual {}:{}",
                path.display(),
                expected.0,
                expected.1,
                actual.0,
                actual.1
            ),
            DirDiffEntry::MtimeMismatch {
                path,
                expected,
                actual,
            } => write!(
                f,
                "mtime mismatch at {}: expected {expected}, actual {actual}",
                path.display()
            ),
            DirDiffEntry::SymlinkMismatch {
                path,
                expected,
                actual,
            } => write!(
                f,
                "symlink target mismatch at {}: expected {}, actual {}",
                path.display(),
                expected.display(),
                actual.display()
            ),
        }
    }
}

fn compare_dir(
    expected_root: &Path,
    actual_root: &Path,
    rel: &Path,
    opts: DirDiffOptions,
    out: &mut Vec<DirDiffEntry>,
) -> Result<(), DirDiffError> {
    let expected_names = sorted_entries(&expected_root.join(rel))?;
    let actual_names = sorted_entries(&actual_root.join(rel))?;

    let mut ei = 0;
    let mut ai = 0;
    while ei < expected_names.len() || ai < actual_names.len() {
        match (expected_names.get(ei), actual_names.get(ai)) {
            (Some(e), Some(a)) if e == a => {
                let child = rel.join(e);
                compare_entry(expected_root, actual_root, &child, opts, out)?;
                ei += 1;
                ai += 1;
            }
            (Some(e), Some(a)) if e < a => {
                out.push(DirDiffEntry::OnlyInExpected(rel.join(e)));
                ei += 1;
            }
            (Some(_), Some(a)) => {
                out.push(DirDiffEntry::OnlyInActual(rel.join(a)));
                ai += 1;
            }
            (Some(e), None) => {
                out.push(DirDiffEntry::OnlyInExpected(rel.join(e)));
                ei += 1;
            }
            (None, Some(a)) => {
                out.push(DirDiffEntry::OnlyInActual(rel.join(a)));
                ai += 1;
            }
            (None, None) => break,
        }
    }
    Ok(())
}

fn sorted_entries(dir: &Path) -> Result<Vec<PathBuf>, DirDiffError> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir)? {
        names.push(PathBuf::from(entry?.file_name()));
    }
    names.sort();
    Ok(names)
}

fn compare_entry(
    expected_root: &Path,
    actual_root: &Path,
    rel: &Path,
    opts: DirDiffOptions,
    out: &mut Vec<DirDiffEntry>,
) -> Result<(), DirDiffError> {
    let ep = expected_root.join(rel);
    let ap = actual_root.join(rel);
    let em = fs::symlink_metadata(&ep)?;
    let am = fs::symlink_metadata(&ap)?;

    let et = file_type_name(&em);
    let at = file_type_name(&am);
    if et != at {
        out.push(DirDiffEntry::TypeMismatch {
            path: rel.to_path_buf(),
            expected: et,
            actual: at,
        });
        return Ok(());
    }

    if em.file_type().is_symlink() {
        let etarget = fs::read_link(&ep)?;
        let atarget = fs::read_link(&ap)?;
        if etarget != atarget {
            out.push(DirDiffEntry::SymlinkMismatch {
                path: rel.to_path_buf(),
                expected: etarget,
                actual: atarget,
            });
        }
        // Symlink attributes (mode/owner) are not meaningfully portable to
        // compare here; upstream checks the target string, which we did.
        return Ok(());
    }

    if em.is_dir() {
        compare_metadata(rel, &em, &am, opts, out);
        return compare_dir(expected_root, actual_root, rel, opts, out);
    }

    // Regular file (or other non-dir, non-symlink node). A length mismatch
    // short-circuits the byte compare; both cases report the same entry.
    if opts.check_content && em.is_file() && am.is_file() {
        let differs = em.len() != am.len() || !files_equal(&ep, &ap, em.len())?;
        if differs {
            out.push(DirDiffEntry::ContentMismatch {
                path: rel.to_path_buf(),
                expected_len: em.len(),
                actual_len: am.len(),
            });
        }
    }
    compare_metadata(rel, &em, &am, opts, out);
    Ok(())
}

fn compare_metadata(
    rel: &Path,
    em: &fs::Metadata,
    am: &fs::Metadata,
    opts: DirDiffOptions,
    out: &mut Vec<DirDiffEntry>,
) {
    if opts.check_mode {
        let (emode, amode) = (mode_bits(em), mode_bits(am));
        if emode != amode {
            out.push(DirDiffEntry::ModeMismatch {
                path: rel.to_path_buf(),
                expected: emode,
                actual: amode,
            });
        }
    }
    if opts.check_owner {
        if let (Some(eo), Some(ao)) = (owner(em), owner(am)) {
            if eo != ao {
                out.push(DirDiffEntry::OwnerMismatch {
                    path: rel.to_path_buf(),
                    expected: eo,
                    actual: ao,
                });
            }
        }
    }
    if opts.check_mtime {
        if let (Some(et), Some(at)) = (mtime_secs(em), mtime_secs(am)) {
            if et != at {
                out.push(DirDiffEntry::MtimeMismatch {
                    path: rel.to_path_buf(),
                    expected: et,
                    actual: at,
                });
            }
        }
    }
}

fn files_equal(a: &Path, b: &Path, len: u64) -> Result<bool, DirDiffError> {
    if len <= STREAM_THRESHOLD {
        return Ok(fs::read(a)? == fs::read(b)?);
    }
    let mut fa = fs::File::open(a)?;
    let mut fb = fs::File::open(b)?;
    let mut ba = [0u8; 64 * 1024];
    let mut bb = [0u8; 64 * 1024];
    loop {
        let na = read_full(&mut fa, &mut ba)?;
        let nb = read_full(&mut fb, &mut bb)?;
        if na != nb || ba[..na] != bb[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
}

fn read_full(f: &mut fs::File, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match f.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

fn file_type_name(m: &fs::Metadata) -> &'static str {
    let t = m.file_type();
    if t.is_dir() {
        "dir"
    } else if t.is_symlink() {
        "symlink"
    } else if t.is_file() {
        "file"
    } else {
        "special"
    }
}

#[cfg(unix)]
fn mode_bits(m: &fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    m.mode() & 0o7777
}

#[cfg(not(unix))]
fn mode_bits(m: &fs::Metadata) -> u32 {
    u32::from(m.permissions().readonly())
}

#[cfg(unix)]
fn owner(m: &fs::Metadata) -> Option<(u32, u32)> {
    use std::os::unix::fs::MetadataExt;
    Some((m.uid(), m.gid()))
}

#[cfg(not(unix))]
fn owner(_m: &fs::Metadata) -> Option<(u32, u32)> {
    None
}

fn mtime_secs(m: &fs::Metadata) -> Option<i64> {
    let modified = m.modified().ok()?;
    match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => Some(d.as_secs() as i64),
        Err(e) => Some(-(e.duration().as_secs() as i64)),
    }
}
