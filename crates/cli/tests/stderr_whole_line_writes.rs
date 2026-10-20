//! Every diagnostic `cli::run` prints reaches stderr in one write call.
//!
//! upstream: log.c:269-427 `rwrite()` formats a whole message, newline
//! included, and writes it in one `write(2)`. The remote shell inherits the
//! same descriptor (pipe.c:48-97 `piped_child()`), so a message issued as
//! several writes could be torn apart by the remote side's own output.
use std::io::{self, Write};

/// Records each `write` call `cli::run` makes on its stderr handle.
#[derive(Default)]
struct WriteLog {
    calls: Vec<Vec<u8>>,
}

impl Write for WriteLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.calls.push(buf.to_vec());
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn stderr_writes(args: &[&str]) -> Vec<Vec<u8>> {
    let mut stdout = Vec::new();
    let mut stderr = WriteLog::default();
    let code = cli::run(args.iter().copied(), &mut stdout, &mut stderr);
    assert_ne!(code, 0, "{args:?} must fail so that it prints a diagnostic");
    stderr.calls
}

fn assert_each_write_is_whole_lines(calls: &[Vec<u8>]) {
    assert!(!calls.is_empty(), "the run printed no diagnostic");
    for call in calls {
        assert!(
            call.ends_with(b"\n") || call.ends_with(b"\r"),
            "a write ended mid-line: {:?} (all writes: {:?})",
            String::from_utf8_lossy(call),
            calls
                .iter()
                .map(|c| String::from_utf8_lossy(c).into_owned())
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn a_usage_error_is_written_one_whole_line_per_write() {
    let calls = stderr_writes(&["oc-rsync", "--definitely-not-an-option"]);
    assert_each_write_is_whole_lines(&calls);
}

#[test]
fn a_transfer_error_is_written_one_whole_line_per_write() {
    let dest =
        std::env::temp_dir().join(format!("oc-rsync-whole-line-writes-{}", std::process::id()));
    let calls = stderr_writes(&[
        "oc-rsync",
        "/definitely/missing/oc-rsync-source",
        dest.to_str().expect("utf-8 temp path"),
    ]);
    let _ = std::fs::remove_dir_all(&dest);
    assert_each_write_is_whole_lines(&calls);
}
