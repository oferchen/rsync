//! Event-driven waits on a child process's log stream.
//!
//! A test that needs a daemon to reach some state (a listener serving, a
//! session line logged) blocks on the daemon's own log output instead of
//! polling a file against a deadline. Point the daemon's log at a pipe the test
//! owns (for example `log file = /dev/stderr` with `Stdio::piped()`), hand that
//! pipe to [`LogLines::spawn`], and call [`LogLines::wait_for`]. The wait ends
//! when the line arrives, or with an error when the stream ends first, so
//! a slow host only makes it slower, never makes it fail.

use std::io::{self, BufRead, BufReader, Read};
use std::sync::mpsc::{self, Receiver};
use std::thread;

/// The lines a child process writes to one stream, read on a background
/// thread so the child never blocks on a full pipe.
#[derive(Debug)]
pub struct LogLines {
    rx: Receiver<String>,
    seen: Vec<String>,
}

impl LogLines {
    /// Starts reading `source` line by line until it reaches end of file.
    pub fn spawn(source: impl Read + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(source).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            rx,
            seen: Vec::new(),
        }
    }

    /// Returns the first line containing `needle`, blocking until it arrives.
    ///
    /// Lines already read are searched first, so the order of waits does not
    /// matter.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::UnexpectedEof`], naming `needle` and the lines
    /// seen, when the stream ends without such a line (the child exited).
    pub fn wait_for(&mut self, needle: &str) -> io::Result<String> {
        if let Some(line) = self.seen.iter().find(|line| line.contains(needle)) {
            return Ok(line.clone());
        }
        while let Ok(line) = self.rx.recv() {
            self.seen.push(line);
            let line = self.seen.last().expect("just pushed");
            if line.contains(needle) {
                return Ok(line.clone());
            }
        }
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!(
                "log ended before a line containing {needle:?}; lines seen:\n{}",
                self.transcript()
            ),
        ))
    }

    /// Every line read so far, in order.
    #[must_use]
    pub fn seen(&self) -> &[String] {
        &self.seen
    }

    /// The lines read so far, joined for a failure message.
    #[must_use]
    pub fn transcript(&self) -> String {
        self.seen.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wait returns the matching line and keeps every line before it, so a
    /// later assertion can check what was (or was not) logged earlier.
    #[test]
    fn wait_for_returns_the_matching_line_and_keeps_history() {
        let mut log = LogLines::spawn(&b"one\nready on 1\nthree\n"[..]);
        assert_eq!(log.wait_for("ready").expect("ready line"), "ready on 1");
        assert_eq!(log.seen(), ["one", "ready on 1"]);
        assert_eq!(log.wait_for("one").expect("earlier line"), "one");
        assert_eq!(log.wait_for("three").expect("later line"), "three");
    }

    /// A stream that ends without the line is an error, never a hang or a pass.
    #[test]
    fn wait_for_fails_at_end_of_stream() {
        let mut log = LogLines::spawn(&b"one\ntwo\n"[..]);
        let error = log.wait_for("ready").expect_err("no ready line");
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        assert!(error.to_string().contains("one\ntwo"), "{error}");
    }
}
