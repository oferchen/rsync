//! Where a local copy refuses `--checksum` with a `none` file checksum.
//!
//! upstream: flist.c:1815-1816 - the sender's `make_file()` calls
//! `file_checksum()` for every regular file under `--checksum`, and
//! checksum.c:548-551 refuses `none` there. The file must open first: an
//! unreadable one gets a zero sum (checksum.c:426-430). So a source with no
//! readable regular file is copied, and any other source is refused before
//! anything is copied.

use std::ffi::OsString;
use std::fs;
use std::path::Path;

/// Reports whether building the file list of `sources` would checksum a file:
/// whether the walk reaches a regular file that opens.
///
/// The walk descends as upstream's sender does: a whole tree under
/// `recursive`; one level for a trailing-slash directory under `dirs`
/// (`send_directory()` without recursion, flist.c:2434); otherwise only the
/// operand itself. Symlinks are not followed and filters are not applied.
pub(super) fn reaches_summed_file(sources: &[OsString], recursive: bool, dirs: bool) -> bool {
    sources.iter().any(|source| {
        let path = Path::new(source);
        let lists_contents = source.as_encoded_bytes().ends_with(b"/");
        let depth = if recursive {
            usize::MAX
        } else if dirs && lists_contents {
            1
        } else {
            0
        };
        let metadata = if lists_contents {
            fs::metadata(path)
        } else {
            fs::symlink_metadata(path)
        };
        metadata.is_ok_and(|meta| reaches(path, &meta, depth))
    })
}

fn reaches(path: &Path, meta: &fs::Metadata, depth: usize) -> bool {
    if meta.is_file() {
        return fs::File::open(path).is_ok();
    }
    if !meta.is_dir() || depth == 0 {
        return false;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .metadata()
            .is_ok_and(|child| reaches(&entry.path(), &child, depth - 1))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources(paths: &[&Path]) -> Vec<OsString> {
        paths.iter().map(|p| p.as_os_str().to_owned()).collect()
    }

    #[test]
    fn an_empty_tree_reaches_no_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(temp.path().join("src/empty")).expect("dirs");
        let src = format!("{}/", temp.path().join("src").display());
        assert!(!reaches_summed_file(&[OsString::from(src)], true, false));
    }

    #[test]
    fn a_nested_file_is_reached_only_when_the_walk_descends() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(temp.path().join("src/sub")).expect("dirs");
        std::fs::write(temp.path().join("src/sub/f"), b"x").expect("file");
        let src = OsString::from(format!("{}/", temp.path().join("src").display()));
        assert!(reaches_summed_file(std::slice::from_ref(&src), true, false));
        assert!(!reaches_summed_file(&[src], false, true));
    }

    #[test]
    fn a_file_operand_is_reached() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("f");
        std::fs::write(&file, b"x").expect("file");
        assert!(reaches_summed_file(&sources(&[&file]), false, false));
    }
}
