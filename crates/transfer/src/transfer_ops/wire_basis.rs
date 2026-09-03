//! Resolution of the peer-supplied alternate-basis selector.
//!
//! Upstream's generator and receiver are separate processes, so the generator's
//! basis choice reaches the receiver by crossing the wire through the sender:
//! `write_ndx_and_attrs()` carries `fnamecmp_type` plus, for the basis types, an
//! `ITEM_XNAME_FOLLOWS` leaf name. `recv_files()` then rebuilds the basis path
//! from that pair (`receiver.c:1009-1046`).
//!
//! oc's generator and receiver share a process, so the locally selected
//! `basis_path` is already available on the pending transfer. This module exists
//! for the cases where upstream's receiver *overrides* that choice from the
//! wire: the fuzzy and alt-dest basis types, where `fnamecmp = xname` and the
//! basedir comes from the entry's own directory or from `basis_dir[]`. Honouring
//! them is what makes oc's receiver behave like upstream's against any peer -
//! and the reason the xname is sanitized before it ever reaches here
//! (`receiver::wire::sanitize_basis_xname`, `rsync.c:407-427`).

use std::io;
use std::path::{Path, PathBuf};

use crate::config::ReferenceDirectory;

/// The operator-owned inputs a wire basis selector is resolved against.
///
/// Held by [`super::ResponseContext`] so the per-file resolution has everything
/// upstream's `recv_files()` reads from its globals: the entry's own relative
/// path (upstream `file->dirname`), the `basis_dir[]` array, and the `--fuzzy`
/// level that authorises the `FNAMECMP_FUZZY` arm at all.
#[derive(Clone, Copy)]
pub struct WireBasis<'a> {
    /// The entry's path relative to the destination root (upstream `file->dirname`
    /// is its parent). Used to place the basedir for both fuzzy arms.
    pub entry_relative_path: &'a Path,
    /// The operator's `--compare-dest` / `--copy-dest` / `--link-dest` list, in
    /// declaration order - upstream `basis_dir[]`.
    pub basis_dirs: &'a [ReferenceDirectory],
    /// `--fuzzy` level. Upstream refuses a `FNAMECMP_FUZZY` selector outright
    /// when `fuzzy_basis == 0` (`receiver.c:1009-1012`).
    pub fuzzy_level: u8,
}

impl WireBasis<'_> {
    /// Resolves the basis path the peer named, or `None` to keep the locally
    /// selected one.
    ///
    /// `Some(path)` is returned only for the arms where upstream sets
    /// `fnamecmp = xname`: `FNAMECMP_FUZZY` (basedir = the entry's own
    /// directory) and `FNAMECMP_FUZZY + i` for `1 <= i <= basis_dir_cnt`
    /// (basedir = `basis_dir[i-1]` joined with the entry's directory). Every
    /// other basis type names a path the receiver already knows - the
    /// destination, the partial-dir file, the backup - so the locally selected
    /// `basis_path` stands and `None` is returned.
    ///
    /// # Errors
    ///
    /// Returns `InvalidData` for a selector upstream treats as a protocol
    /// violation: a fuzzy basis with `--fuzzy` off, and a basis-dir index past
    /// the end of `basis_dir[]`.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:1009-1018` - `FNAMECMP_FUZZY`: refuse when `fuzzy_basis == 0`,
    ///   else `basedir = file->dirname` and `fnamecmp = xname`
    /// - `receiver.c:1019-1029` - `fnamecmp_type - FNAMECMP_FUZZY <= basis_dir_cnt`:
    ///   `pathjoin(basis_dir[i], file->dirname)` and `fnamecmp = xname`
    /// - `receiver.c:1030-1034` - out-of-range index is `RERR_PROTOCOL`
    pub fn resolve(
        &self,
        fnamecmp_type: Option<protocol::FnameCmpType>,
        xname: Option<&[u8]>,
        dest_path: &Path,
    ) -> io::Result<Option<PathBuf>> {
        let Some(protocol::FnameCmpType::Fuzzy(offset)) = fnamecmp_type else {
            return Ok(None);
        };
        // upstream: rsync.c:410-412 - the basis arms read `xname` as the leaf.
        // A basis type without one leaves nothing to resolve.
        let Some(leaf) = xname.filter(|name| !name.is_empty()) else {
            return Ok(None);
        };

        let dirname = self
            .entry_relative_path
            .parent()
            .unwrap_or_else(|| Path::new(""));

        let basedir = if offset == 0 {
            // upstream: receiver.c:1009-1012 - a fuzzy selector with --fuzzy off
            // is a malicious peer, not a stale one.
            if self.fuzzy_level == 0 {
                return Err(refusing_malicious_fuzzy(leaf));
            }
            // upstream: receiver.c:1013-1015 - basedir = file->dirname, which
            // upstream resolves against the destination root it has chdir'd to.
            // oc carries the entry's already-resolved destination path, so the
            // same directory is that path's parent.
            dest_path.parent().unwrap_or(Path::new("")).to_path_buf()
        } else {
            let index = usize::from(offset - 1);
            let Some(basis_dir) = self.basis_dirs.get(index) else {
                // upstream: receiver.c:1030-1034 "invalid basis_dir index".
                return Err(invalid_basis_dir_index(offset));
            };
            basis_dir.path.join(dirname)
        };

        Ok(Some(basedir.join(leaf_as_path(leaf))))
    }
}

/// Interprets a sanitized xname as a relative path component.
///
/// The bytes have already been through
/// [`filters::sanitize_path::sanitize_path_bytes_default`] with a depth budget
/// of 0, so no `..` and no leading `/` survive.
#[cfg(unix)]
fn leaf_as_path(leaf: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(leaf))
}

/// Windows has no byte-oriented path constructor; the xname is decoded lossily,
/// matching how every other peer-supplied name reaches the filesystem there.
#[cfg(not(unix))]
fn leaf_as_path(leaf: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(leaf).into_owned())
}

/// upstream: `receiver.c:1010-1011` - "refusing malicious fuzzy operation".
fn refusing_malicious_fuzzy(leaf: &[u8]) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "refusing malicious fuzzy operation for {}{}{}",
            String::from_utf8_lossy(leaf),
            crate::role_trailer::error_location!(),
            crate::role_trailer::receiver()
        ),
    )
}

/// upstream: `receiver.c:1031-1033` - "invalid basis_dir index: %d.".
fn invalid_basis_dir_index(offset: u8) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "invalid basis_dir index: {}.{}{}",
            offset - 1,
            crate::role_trailer::error_location!(),
            crate::role_trailer::receiver()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ReferenceDirectoryKind;

    fn ref_dir(path: &str) -> ReferenceDirectory {
        ReferenceDirectory {
            kind: ReferenceDirectoryKind::Link,
            path: PathBuf::from(path),
            requested: PathBuf::from(path),
        }
    }

    fn basis<'a>(dirs: &'a [ReferenceDirectory], rel: &'a Path, fuzzy: u8) -> WireBasis<'a> {
        WireBasis {
            entry_relative_path: rel,
            basis_dirs: dirs,
            fuzzy_level: fuzzy,
        }
    }

    #[test]
    fn non_basis_types_keep_the_local_choice() {
        let dirs = [ref_dir("/linkdest")];
        let rel = PathBuf::from("sub/file");
        let wb = basis(&dirs, &rel, 0);
        for kind in [
            protocol::FnameCmpType::Fname,
            protocol::FnameCmpType::PartialDir,
            protocol::FnameCmpType::Backup,
            protocol::FnameCmpType::BasisDir(0),
        ] {
            assert_eq!(
                wb.resolve(Some(kind), Some(b"secret"), Path::new("/dest/sub/file"))
                    .unwrap(),
                None
            );
        }
    }

    #[test]
    fn alt_dest_index_joins_the_basis_dir_and_the_entry_dir() {
        let dirs = [ref_dir("/linkdest")];
        let rel = PathBuf::from("sub/file");
        let wb = basis(&dirs, &rel, 0);
        assert_eq!(
            wb.resolve(
                Some(protocol::FnameCmpType::Fuzzy(1)),
                Some(b"other"),
                Path::new("/dest/sub/file"),
            )
            .unwrap(),
            Some(PathBuf::from("/linkdest/sub/other"))
        );
    }

    #[test]
    fn a_root_level_entry_resolves_directly_under_the_basis_dir() {
        let dirs = [ref_dir("/linkdest")];
        let rel = PathBuf::from("file");
        let wb = basis(&dirs, &rel, 0);
        assert_eq!(
            wb.resolve(
                Some(protocol::FnameCmpType::Fuzzy(1)),
                Some(b"secret"),
                Path::new("/dest/file"),
            )
            .unwrap(),
            Some(PathBuf::from("/linkdest/secret"))
        );
    }

    #[test]
    fn a_fuzzy_dest_basis_resolves_beside_the_entry() {
        let rel = PathBuf::from("sub/file");
        let wb = basis(&[], &rel, 1);
        assert_eq!(
            wb.resolve(
                Some(protocol::FnameCmpType::Fuzzy(0)),
                Some(b"near"),
                Path::new("/dest/sub/file"),
            )
            .unwrap(),
            Some(PathBuf::from("/dest/sub/near"))
        );
    }

    #[test]
    fn a_fuzzy_selector_without_fuzzy_enabled_is_refused() {
        let rel = PathBuf::from("file");
        let wb = basis(&[], &rel, 0);
        let err = wb
            .resolve(
                Some(protocol::FnameCmpType::Fuzzy(0)),
                Some(b"near"),
                Path::new("/dest/file"),
            )
            .expect_err("upstream exits RERR_PROTOCOL here");
        assert!(err.to_string().contains("refusing malicious fuzzy"));
    }

    #[test]
    fn an_index_past_the_basis_dir_list_is_refused() {
        let dirs = [ref_dir("/linkdest")];
        let rel = PathBuf::from("file");
        let wb = basis(&dirs, &rel, 1);
        let err = wb
            .resolve(
                Some(protocol::FnameCmpType::Fuzzy(2)),
                Some(b"x"),
                Path::new("/dest/file"),
            )
            .expect_err("upstream exits RERR_PROTOCOL here");
        assert!(err.to_string().contains("invalid basis_dir index: 1."));
    }

    #[test]
    fn a_basis_type_without_an_xname_keeps_the_local_choice() {
        let dirs = [ref_dir("/linkdest")];
        let rel = PathBuf::from("file");
        let wb = basis(&dirs, &rel, 1);
        assert_eq!(
            wb.resolve(
                Some(protocol::FnameCmpType::Fuzzy(1)),
                None,
                Path::new("/dest/file"),
            )
            .unwrap(),
            None
        );
    }
}
