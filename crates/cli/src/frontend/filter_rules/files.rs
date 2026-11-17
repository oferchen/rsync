//! The files a CLI filter option names, read the way the client reads them.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use core::client::FilterRuleSpec;
use core::client::filter_args::{FilterFileKind, FilterFileSource};
use core::message::Role;

/// Opens the client's (and a remote-shell server's) filter files: relative to
/// the cwd, through the ownership walk, and standard input for `-`.
///
/// upstream: `exclude.c:1680-1684` - `parse_filter_file()` opens with
/// `operator_path_resolve` set, and reads `stdin` for a name of `-`.
pub(crate) struct ProcessFilterFiles {
    /// The process parsing the rules: the client, or a remote-shell server.
    pub(crate) who: Role,
}

impl FilterFileSource for ProcessFilterFiles {
    /// Empty, so a relative merge name stays relative: opened from the cwd and
    /// reported as the operator spelled it, cleaned.
    ///
    /// upstream: exclude.c:737-747 parse_merge_name() prepends `dirbuf`, which
    /// set_filter_dir() has not filled while the options are parsed, so
    /// `-f '. sub/../nope'` fails as `failed to open exclude file nope`.
    fn base_dir(&self) -> PathBuf {
        PathBuf::new()
    }

    fn who(&self) -> Role {
        self.who
    }

    fn open(
        &self,
        path: &Path,
        _kind: FilterFileKind,
        _list: &[FilterRuleSpec],
    ) -> io::Result<Box<dyn Read>> {
        if path == Path::new("-") {
            #[cfg(test)]
            if let Some(data) = TEST_STDIN.with(|slot| slot.borrow_mut().take()) {
                return Ok(Box::new(io::Cursor::new(data)));
            }
            return Ok(Box::new(io::stdin()));
        }
        // `.`, `..` and `/` name a directory, which upstream opens and then reads
        // as an empty file (exclude.c:1764-1769): the open still has to succeed.
        if path.file_name().is_none() {
            crate::frontend::operator_file::open_dir(path)?;
            return Ok(Box::new(io::empty()));
        }
        Ok(Box::new(
            crate::frontend::operator_file::open_read_confined(path)?,
        ))
    }
}

#[cfg(test)]
thread_local! {
    static TEST_STDIN: std::cell::RefCell<Option<Vec<u8>>> = const {
        std::cell::RefCell::new(None)
    };
}

/// Sets what [`ProcessFilterFiles`] reads for `-` in this test thread.
#[cfg(test)]
pub(crate) fn set_filter_stdin_input(data: Vec<u8>) {
    TEST_STDIN.with(|slot| *slot.borrow_mut() = Some(data));
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENT_FILES: ProcessFilterFiles = ProcessFilterFiles { who: Role::Client };

    /// upstream: `-f '. ..'` opens the directory and reads it as an empty file,
    /// so 3.5.2 exits 0 where oc refused it with EINVAL (exit 11).
    #[test]
    fn a_merge_name_without_a_final_component_reads_as_empty() {
        for name in [".", ".."] {
            let mut reader = CLIENT_FILES
                .open(Path::new(name), FilterFileKind::MergeFile, &[])
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            let mut contents = Vec::new();
            reader.read_to_end(&mut contents).expect("read");
            assert!(contents.is_empty(), "{name}");
        }
    }

    /// A directory that does not exist is still an open failure. The name
    /// is missing both where `..` is walked (Unix) and where it is resolved
    /// lexically (Windows collapses `nosuch\sub\..` to `nosuch`).
    #[test]
    fn a_missing_directory_name_still_fails_to_open() {
        let temp = test_support::create_tempdir();
        let missing = temp.path().join("nosuch").join("sub").join("..");
        assert!(
            CLIENT_FILES
                .open(&missing, FilterFileKind::MergeFile, &[])
                .is_err()
        );
    }
}
