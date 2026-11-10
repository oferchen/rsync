//! Filter options from an argv, parsed as upstream's `parse_arguments()`
//! parses them.
//!
//! upstream: options.c:1620-1656 (`--filter`, `--exclude`, `--include` and the
//! `-from` pair) and :1718-1727 (`-F`). The client and a remote-shell server
//! parse their argv with this one code path, relative to the process cwd.

mod apple_double;
mod build;
mod cvs;
mod directive;
mod merge;
mod order;
mod parsing;
mod sources;

#[cfg(test)]
mod tests;

use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub use build::{FilterInputs, build_filter_rules};
pub use order::FilterOrderToken;

/// Where the files that filter options name are read from.
///
/// upstream: `parse_filter_file()` (exclude.c:1601-1719) is one function for
/// every caller, but what its open may reach depends on who supplied the name.
/// The client and a remote-shell server open relative to their cwd through
/// the ownership walk; a daemon resolves beneath its module and gates the open
/// on the module's own rules. That part belongs to the caller.
pub trait FilterFileSource {
    /// The directory a relative merge-file name resolves against.
    fn base_dir(&self) -> PathBuf;

    /// Opens `path` for reading; `-` names standard input.
    ///
    /// # Errors
    ///
    /// The open failed. The parser reports it as upstream's open failure
    /// under `XFLG_FATAL_ERRORS` (exclude.c:1703-1720).
    fn open(&self, path: &Path) -> io::Result<Box<dyn Read>>;
}

/// A source for the parser's own tests: the cwd, with a standard input the
/// test can fill.
#[cfg(test)]
pub(crate) struct TestFiles;

#[cfg(test)]
thread_local! {
    static TEST_STDIN: std::cell::RefCell<Option<Vec<u8>>> = const {
        std::cell::RefCell::new(None)
    };
}

/// Sets what [`TestFiles`] reads for `-`.
#[cfg(test)]
pub(crate) fn set_filter_stdin_input(data: Vec<u8>) {
    TEST_STDIN.with(|slot| *slot.borrow_mut() = Some(data));
}

#[cfg(test)]
impl FilterFileSource for TestFiles {
    fn base_dir(&self) -> PathBuf {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }

    fn open(&self, path: &Path) -> io::Result<Box<dyn Read>> {
        if path == Path::new("-") {
            let data = TEST_STDIN
                .with(|slot| slot.borrow_mut().take())
                .unwrap_or_default();
            return Ok(Box::new(io::Cursor::new(data)));
        }
        Ok(Box::new(std::fs::File::open(path)?))
    }
}
