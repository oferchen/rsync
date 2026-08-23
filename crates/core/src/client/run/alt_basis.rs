//! Operator-supplied alternate-basis directory validation.
//!
//! upstream: `main.c:867` `check_alt_basis_dirs()`, called from both receiver
//! entry points - `main.c:1241` (server receiver) and `main.c:1424` (client
//! receiver) - once the destination directory is the current directory.
//!
//! The check is advisory: upstream warns and continues, leaving the exit code
//! untouched. Every `basis_link_stat()` caller in `generator.c` already treats
//! any stat failure as "no candidate in this basis dir", so a bad basis costs
//! the hard-link optimisation and nothing else. The warning exists so the
//! operator learns their `--link-dest` is stale instead of silently getting a
//! full copy.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf, is_separator};

use engine::local_copy::{ReferenceDirectory, ReferenceDirectoryKind};
use logging::warn_log;

/// Names the alt-dest option a basis directory came from.
///
/// upstream: `options.c:1444` `alt_dest_opt()`. Upstream reads the single
/// `alt_dest_type` in effect rather than a per-entry kind, which it can do
/// because combining two alt-dest options is rejected outright
/// (`main.c:1886`, exit 1) - so at most one kind is ever present and the
/// per-entry kind carries exactly the same information.
fn alt_dest_opt(kind: ReferenceDirectoryKind) -> &'static str {
    match kind {
        ReferenceDirectoryKind::Compare => "--compare-dest",
        ReferenceDirectoryKind::Copy => "--copy-dest",
        ReferenceDirectoryKind::Link => "--link-dest",
    }
}

/// Removes at most ONE trailing separator, and never from a bare root.
///
/// upstream: `main.c:876-877`
/// `if (bd_len > 1 && bdir[bd_len-1] == '/') bdir[--bd_len] = '\0';`
///
/// Exactly one, because the count decides *which* diagnostic fires. Measured
/// against real 3.5.0 with a plain file at an absolute path:
///
/// | arg | upstream |
/// |---|---|
/// | `<abs>/f` | `is not a dir: <abs>/f` |
/// | `<abs>/f/` | `is not a dir: <abs>/f` - stripped, so the stat lands on the file |
/// | `<abs>/f//` | `does not exist: <abs>/f/` - one survives, `stat("f/")` is ENOTDIR |
/// | `/` | silent - length 1, never stripped, and it is a directory |
///
/// A general path normalisation would collapse both separators and flip the
/// third row to `is not a dir`.
///
/// oc accepts either platform separator where upstream hardcodes `'/'`: a
/// Windows operator writes `--link-dest=..\prev`, and `is_separator` is what
/// the rest of this module tree already uses to decide the question.
fn strip_one_trailing_separator(path: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let bytes = path.as_os_str().as_bytes();
        match bytes.split_last() {
            Some((last, rest)) if !rest.is_empty() && is_separator(char::from(*last)) => {
                PathBuf::from(OsString::from_vec(rest.to_vec()))
            }
            _ => path.to_path_buf(),
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};

        let units: Vec<u16> = path.as_os_str().encode_wide().collect();
        match units.split_last() {
            Some((last, rest))
                if !rest.is_empty()
                    && char::from_u32(u32::from(*last)).is_some_and(is_separator) =>
            {
                PathBuf::from(OsString::from_wide(rest))
            }
            _ => path.to_path_buf(),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        path.to_path_buf()
    }
}

/// Resolves a basis directory to the absolute path upstream reports.
///
/// upstream: `main.c:885-898` joins a relative basis onto `curr_dir`, which is
/// the *destination* directory - upstream has already chdir'd there by the
/// time `check_alt_basis_dirs()` runs, so `--link-dest=../prev` means
/// "`../prev` relative to the destination", not to the invocation directory.
/// That resolution is already correct in oc's candidate lookup; reproducing it
/// here is what makes the reported path match.
///
/// `curr_dir` is absolute for upstream because it is a real working directory,
/// so a relative destination is absolutised against the process cwd to print
/// the same text. Deliberately not canonicalised: upstream string-joins and
/// does not resolve symlinks or `..` for this message.
fn resolve_against_destination(basis: &Path, destination: &Path) -> PathBuf {
    if basis.is_absolute() {
        return basis.to_path_buf();
    }

    let anchor = if destination.is_absolute() {
        destination.to_path_buf()
    } else {
        match env::current_dir() {
            Ok(cwd) => cwd.join(destination),
            // Without a readable cwd there is no absolute form to report, so
            // fall back to the operator's own spelling rather than dropping
            // the warning entirely.
            Err(_) => destination.to_path_buf(),
        }
    };

    anchor.join(basis)
}

/// Warns about each `--compare-dest` / `--copy-dest` / `--link-dest` argument
/// that is missing or is not a directory.
///
/// upstream: `main.c:900-903` - `%s arg does not exist: %s` when the stat
/// fails, `%s arg is not a dir: %s` when it succeeds on a non-directory. Both
/// go to `FWARNING` and neither changes the exit code.
///
/// The stat follows symlinks (upstream uses `do_stat`, not `do_lstat`), so a
/// basis directory reached through a symlink is accepted silently.
pub(crate) fn check_alt_basis_dirs(references: &[ReferenceDirectory], destination: &Path) {
    for reference in references {
        let basis = strip_one_trailing_separator(&reference.path);
        let resolved = resolve_against_destination(&basis, destination);
        let option = alt_dest_opt(reference.kind);

        // `warn_log!` expands to a statement (it ends in `;`), so each arm keeps
        // it in statement position. Calling it directly as an arm value puts a
        // trailing-semicolon macro in expression position, which newer compilers
        // reject outright - and every other caller in the tree does it this way.
        match fs::metadata(&resolved) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                warn_log!("{option} arg is not a dir: {}", resolved.display());
            }
            Err(_) => {
                warn_log!("{option} arg does not exist: {}", resolved.display());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use logging::{DiagnosticEvent, VerbosityConfig, drain_events, init};
    use tempfile::tempdir;

    /// Collects the warning texts `check_alt_basis_dirs` emitted.
    fn warnings_for(references: &[ReferenceDirectory], destination: &Path) -> Vec<String> {
        init(VerbosityConfig::default());
        drain_events();
        check_alt_basis_dirs(references, destination);
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Info { message, .. } => Some(message),
                _ => None,
            })
            .collect()
    }

    fn link(path: impl Into<PathBuf>) -> ReferenceDirectory {
        ReferenceDirectory::new(ReferenceDirectoryKind::Link, path.into())
    }

    /// A real basis directory is accepted in silence - the non-vacuity
    /// companion for every warning row below. Without it, a helper that never
    /// warns at all would satisfy none of them and still look correct.
    #[test]
    fn an_existing_basis_directory_is_silent() {
        let temp = tempdir().expect("tempdir");
        let basis = temp.path().join("prev");
        fs::create_dir(&basis).expect("create basis");

        assert!(warnings_for(&[link(basis)], temp.path()).is_empty());
    }

    /// upstream: `main.c:901` - a missing arg reports `does not exist`.
    #[test]
    fn a_missing_basis_reports_does_not_exist() {
        let temp = tempdir().expect("tempdir");
        let basis = temp.path().join("absent");

        assert_eq!(
            warnings_for(&[link(&basis)], temp.path()),
            vec![format!(
                "--link-dest arg does not exist: {}",
                basis.display()
            )]
        );
    }

    /// upstream: `main.c:903` - an arg that exists but is not a directory
    /// reports `is not a dir`.
    #[test]
    fn a_plain_file_basis_reports_is_not_a_dir() {
        let temp = tempdir().expect("tempdir");
        let basis = temp.path().join("file");
        fs::write(&basis, b"not a directory").expect("write basis");

        assert_eq!(
            warnings_for(&[link(&basis)], temp.path()),
            vec![format!("--link-dest arg is not a dir: {}", basis.display())]
        );
    }

    /// ONE trailing separator is stripped, so the stat lands on the file and
    /// the message is `is not a dir` - not `does not exist`.
    ///
    /// upstream: `main.c:876-877`; measured against real 3.5.0.
    #[test]
    fn one_trailing_separator_is_stripped_before_the_stat() {
        let temp = tempdir().expect("tempdir");
        let basis = temp.path().join("file");
        fs::write(&basis, b"not a directory").expect("write basis");

        let mut with_slash = basis.clone().into_os_string();
        with_slash.push(std::path::MAIN_SEPARATOR_STR);

        assert_eq!(
            warnings_for(&[link(PathBuf::from(with_slash))], temp.path()),
            vec![format!("--link-dest arg is not a dir: {}", basis.display())],
            "a single trailing separator must be removed, exactly as upstream does"
        );
    }

    /// Only ONE separator is stripped: the second survives, `stat` fails with
    /// ENOTDIR, and the message flips to `does not exist` with the surviving
    /// separator still in the reported path.
    ///
    /// This is the row that a general path normalisation would get wrong.
    #[test]
    fn only_one_trailing_separator_is_stripped() {
        let temp = tempdir().expect("tempdir");
        let basis = temp.path().join("file");
        fs::write(&basis, b"not a directory").expect("write basis");

        let mut doubled = basis.clone().into_os_string();
        doubled.push(std::path::MAIN_SEPARATOR_STR);
        doubled.push(std::path::MAIN_SEPARATOR_STR);

        let mut reported = basis.into_os_string();
        reported.push(std::path::MAIN_SEPARATOR_STR);

        assert_eq!(
            warnings_for(&[link(PathBuf::from(doubled))], temp.path()),
            vec![format!(
                "--link-dest arg does not exist: {}",
                Path::new(&reported).display()
            )],
            "the surviving separator makes the stat fail, which selects the \
             other message - collapsing both would report `is not a dir`"
        );
    }

    /// A relative basis resolves against the DESTINATION, not the invocation
    /// directory. upstream: `main.c:885-898` joins onto `curr_dir`.
    #[test]
    fn a_relative_basis_resolves_against_the_destination() {
        let temp = tempdir().expect("tempdir");
        let destination = temp.path().join("dest");
        fs::create_dir(&destination).expect("create dest");

        assert_eq!(
            warnings_for(&[link("absent")], &destination),
            vec![format!(
                "--link-dest arg does not exist: {}",
                destination.join("absent").display()
            )],
            "the reported path must be anchored at the destination"
        );
    }

    /// Each alt-dest option names itself. upstream: `options.c:1444`.
    #[test]
    fn each_alt_dest_kind_names_its_own_option() {
        let temp = tempdir().expect("tempdir");
        let basis = temp.path().join("absent");

        for (kind, expected) in [
            (ReferenceDirectoryKind::Compare, "--compare-dest"),
            (ReferenceDirectoryKind::Copy, "--copy-dest"),
            (ReferenceDirectoryKind::Link, "--link-dest"),
        ] {
            let reference = ReferenceDirectory::new(kind, basis.clone());
            assert_eq!(
                warnings_for(&[reference], temp.path()),
                vec![format!(
                    "{expected} arg does not exist: {}",
                    basis.display()
                )]
            );
        }
    }
}
