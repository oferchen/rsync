//! The files a CLI filter option names, read the way the client reads them.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use core::client::filter_args::{FilterFileKind, FilterFileSource};

/// Opens the client's (and a remote-shell server's) filter files: relative to
/// the cwd, through the ownership walk, and standard input for `-`.
///
/// upstream: `exclude.c:1680-1684` - `parse_filter_file()` opens with
/// `operator_path_resolve` set, and reads `stdin` for a name of `-`.
pub(crate) struct ProcessFilterFiles;

impl FilterFileSource for ProcessFilterFiles {
    fn base_dir(&self) -> PathBuf {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }

    fn open(&self, path: &Path, _kind: FilterFileKind) -> io::Result<Box<dyn Read>> {
        if path == Path::new("-") {
            #[cfg(test)]
            if let Some(data) = TEST_STDIN.with(|slot| slot.borrow_mut().take()) {
                return Ok(Box::new(io::Cursor::new(data)));
            }
            return Ok(Box::new(io::stdin()));
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
