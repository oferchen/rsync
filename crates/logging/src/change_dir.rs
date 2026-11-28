//! The `change_dir#N <name> failed` line `get_local_name()` prints when the
//! receiver cannot enter its destination.

use std::fmt;
use std::io;

use crate::upstream_errno_text;

/// Which `change_dir()` call in upstream's `get_local_name()` failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeDirStep {
    /// An existing destination directory.
    ///
    /// upstream: main.c:779 `"change_dir#1 %s failed"`
    One,
    /// A destination directory the run has just created.
    ///
    /// upstream: main.c:837 `"change_dir#2 %s failed"`
    Two,
    /// The parent of a single-file destination.
    ///
    /// upstream: main.c:866 and main.c:982 `"change_dir#3 %s failed"`
    Three,
}

impl ChangeDirStep {
    const fn number(self) -> u8 {
        match self {
            Self::One => 1,
            Self::Two => 2,
            Self::Three => 3,
        }
    }
}

/// Renders `change_dir#N <fname> failed: <strerror> (<errno>)`.
///
/// This is the text `rsyserr(FERROR, errno, "change_dir#N %s failed",
/// full_fname(..))` builds after its `rsync: [role] ` prefix: the message,
/// then `": %s (%d)"` with the errno (log.c:575-585). Callers add the prefix.
///
/// `fname` is already rendered the way upstream's `full_fname()` would
/// render it, quotes included; callers own their trailing-slash rules.
#[derive(Clone, Copy, Debug)]
pub struct ChangeDirFailed<'a> {
    step: ChangeDirStep,
    fname: &'a str,
    cause: &'a io::Error,
}

impl<'a> ChangeDirFailed<'a> {
    /// A failed `change_dir()` at `step` on the already-rendered `fname`.
    #[must_use]
    pub const fn new(step: ChangeDirStep, fname: &'a str, cause: &'a io::Error) -> Self {
        Self { step, fname, cause }
    }
}

impl fmt::Display for ChangeDirFailed<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "change_dir#{} {} failed: {}",
            self.step.number(),
            self.fname,
            upstream_errno_text(self.cause)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each step's upstream format string, with `%s` standing for the name.
    /// A wrong number, a missing `failed` or a changed separator breaks the
    /// line rsync users and scripts match against.
    const UPSTREAM: [(ChangeDirStep, &str); 3] = [
        // upstream: main.c:779
        (ChangeDirStep::One, "change_dir#1 %s failed"),
        // upstream: main.c:837
        (ChangeDirStep::Two, "change_dir#2 %s failed"),
        // upstream: main.c:866
        (ChangeDirStep::Three, "change_dir#3 %s failed"),
    ];

    /// An errno the kernel produces, so the tail is the platform's own
    /// strerror text rather than a string this test writes.
    fn kernel_error() -> io::Error {
        let missing = std::env::temp_dir().join("oc-rsync-change-dir-missing-entry");
        std::fs::read_dir(missing).expect_err("the probe path does not exist")
    }

    #[test]
    fn each_step_renders_upstreams_format_string_and_rsyserr_tail() {
        let cause = kernel_error();
        let fname = "\"/var/dest/\"";
        for (step, format) in UPSTREAM {
            let expected = format!(
                "{}: {}",
                format.replacen("%s", fname, 1),
                upstream_errno_text(&cause)
            );
            assert_eq!(
                ChangeDirFailed::new(step, fname, &cause).to_string(),
                expected,
                "{step:?}"
            );
        }
    }

    #[test]
    fn the_name_is_printed_as_given() {
        let cause = kernel_error();
        let line = ChangeDirFailed::new(ChangeDirStep::Two, "\"rel/new\"", &cause).to_string();
        assert!(
            line.starts_with("change_dir#2 \"rel/new\" failed: "),
            "{line}"
        );
    }
}
