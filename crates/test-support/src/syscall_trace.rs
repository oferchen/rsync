//! Counting a command's file opens with `strace`.
//!
//! For tests that pin how many times a file is opened, such as the sender's
//! one open per source file (upstream `sender.c:656-800`). The trace follows
//! every thread and child of the command (`-f`), so the command under trace
//! must be the only process that opens the files being counted - a daemon on
//! the other end of the transfer runs untraced.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The open calls recorded: Rust's std and glibc reach `openat`, the confined
/// resolvers `openat2`.
const OPEN_CALLS: &str = "trace=openat,openat2";

/// The paths a traced command passed to its open calls, one per call.
#[derive(Debug)]
pub struct OpenTrace {
    paths: Vec<PathBuf>,
}

impl OpenTrace {
    /// Runs `program` with `args` under `strace -f`, writing the raw trace to
    /// `log`, and returns the command's output with its recorded opens.
    ///
    /// # Errors
    ///
    /// Returns an error if `strace` cannot be spawned or `log` cannot be read.
    pub fn record<I, S>(
        strace: &Path,
        log: &Path,
        program: &Path,
        args: I,
    ) -> io::Result<(Output, Self)>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = Command::new(strace)
            .args(["-f", "-qq", "-e", OPEN_CALLS, "-o"])
            .arg(log)
            .arg("--")
            .arg(program)
            .args(args)
            .stdin(Stdio::null())
            .output()?;
        let paths = fs::read_to_string(log)?
            .lines()
            .filter_map(opened_path)
            .collect();
        Ok((output, Self { paths }))
    }

    /// How many opens named a file whose final component is `name`, by full
    /// path or relative to a directory descriptor.
    #[must_use]
    pub fn opens_of(&self, name: &OsStr) -> usize {
        self.paths
            .iter()
            .filter(|path| path.file_name() == Some(name))
            .count()
    }
}

/// The path argument of one `openat`/`openat2` trace line: the first quoted
/// string. An interrupted call (`<unfinished ...>`) carries its path on the
/// first line and none on the `<... resumed>` line, so each call counts once.
fn opened_path(line: &str) -> Option<PathBuf> {
    let call = line.find("openat")?;
    let quoted = &line[call..];
    let start = quoted.find('"')? + 1;
    let len = quoted[start..].find('"')?;
    Some(PathBuf::from(OsString::from(&quoted[start..start + len])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absolute_and_a_relative_open_both_name_the_file() {
        let trace = OpenTrace {
            paths: [
                "12 openat(AT_FDCWD, \"/src/a.bin\", O_RDONLY|O_CLOEXEC) = 5",
                "12 openat(4, \"a.bin\", O_RDONLY|O_NOFOLLOW|O_CLOEXEC) = 6",
                "13 openat(AT_FDCWD, \"/src/b.bin\", O_RDONLY <unfinished ...>",
                "13 <... openat resumed>) = 7",
            ]
            .into_iter()
            .filter_map(opened_path)
            .collect(),
        };
        assert_eq!(trace.opens_of(OsStr::new("a.bin")), 2);
        assert_eq!(trace.opens_of(OsStr::new("b.bin")), 1);
        assert_eq!(trace.opens_of(OsStr::new("src")), 0);
    }
}
