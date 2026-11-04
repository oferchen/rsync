//! Operator-supplied alternate-basis directory validation.
//!
//! upstream: `main.c:880` `check_alt_basis_dirs()`, called from both receiver
//! entry points - `main.c:1259` (server receiver) and `main.c:1442` (client
//! receiver) - once the destination directory is the current directory.
//!
//! The check is advisory: upstream warns and continues, leaving the exit code
//! untouched. Every `basis_link_stat()` caller in `generator.c` already treats
//! any stat failure as "no candidate in this basis dir", so a bad basis costs
//! the hard-link optimisation and nothing else. The warning exists so the
//! operator learns their `--link-dest` is stale instead of silently getting a
//! full copy.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use engine::local_copy::{ReferenceDirectory, ReferenceDirectoryKind};
use logging::warn_log;

/// Names the alt-dest option a basis directory came from.
///
/// upstream: `options.c:1450` `alt_dest_opt()`. Upstream reads the single
/// `alt_dest_type` in effect rather than a per-entry kind, which it can do
/// because combining two alt-dest options is rejected outright
/// (`options.c:1854-1858`, exit 1) - so at most one kind is ever present and the
/// per-entry kind carries exactly the same information.
fn alt_dest_opt(kind: ReferenceDirectoryKind) -> &'static str {
    match kind {
        ReferenceDirectoryKind::Compare => "--compare-dest",
        ReferenceDirectoryKind::Copy => "--copy-dest",
        ReferenceDirectoryKind::Link => "--link-dest",
    }
}

/// Returns the destination as upstream's absolute `curr_dir`.
///
/// upstream: `main.c:898-911` joins a relative basis onto `curr_dir`, which is
/// the *destination* directory - upstream has already chdir'd there by the
/// time `check_alt_basis_dirs()` runs, so `--link-dest=../prev` means
/// "`../prev` relative to the destination", not to the invocation directory.
///
/// `curr_dir` is absolute for upstream because it is a real working directory,
/// so a relative destination is absolutised against the process cwd to print
/// the same text. Deliberately not canonicalised: upstream string-joins and
/// does not resolve symlinks or `..` for this message.
fn absolute_destination(destination: &Path) -> PathBuf {
    if destination.is_absolute() {
        destination.to_path_buf()
    } else {
        match env::current_dir() {
            Ok(cwd) => cwd.join(destination),
            // Without a readable cwd there is no absolute form to report, so
            // fall back to the operator's own spelling rather than dropping
            // the warning entirely.
            Err(_) => destination.to_path_buf(),
        }
    }
}

/// Whether the CLIENT should run the basis-dir check for a remote transfer.
///
/// upstream: `main.c:1442` runs `check_alt_basis_dirs()` on the client only
/// when the client is the receiver, i.e. on a pull. On a push the remote server
/// is the receiver and checks its own forwarded argv (`main.c:1259`), because
/// `options.c:3114-3123` emits the basis-dir args only in that direction - so
/// warning here too would double-report.
///
/// The remote-operand conjunct keeps a purely local copy out: that path has its
/// own call once `LocalCopyPlan` resolves the destination, and without this the
/// same run would warn twice.
pub(crate) fn client_checks_basis_dirs(is_pull: bool, has_remote_operand: bool) -> bool {
    is_pull && has_remote_operand
}

/// Warns about each `--compare-dest` / `--copy-dest` / `--link-dest` argument
/// that is missing or is not a directory.
///
/// upstream: `main.c:913-916` - `%s arg does not exist: %s` when the stat
/// fails, `%s arg is not a dir: %s` when it succeeds on a non-directory. Both
/// go to `FWARNING` and neither changes the exit code.
///
/// The stat follows symlinks (upstream uses `do_stat`, not `do_lstat`), so a
/// basis directory reached through a symlink is accepted silently.
pub fn check_alt_basis_dirs(references: &[ReferenceDirectory], destination: &Path) {
    let curr_dir = absolute_destination(destination);
    for reference in references {
        let anchored = reference.anchored_at(&curr_dir, false);
        check_alt_basis_dir_at(reference.kind, &anchored.path, &anchored.path);
    }
}

/// Warns about ONE basis directory, given the path to stat and the spelling to
/// print.
///
/// Split out because the two are not always the same string. A daemon receiver
/// has `sanitize_paths` set (`clientserver.c:1068`), so `main.c:898`'s
/// `if (*bdir != '/' && (dry_run > 1 || !sanitize_paths))` skips the join onto
/// `curr_dir` and upstream stats and prints the *sanitized* value: relative for
/// a relative arg (`--link-dest arg does not exist: sibling`), absolute for an
/// absolute one, because `util1.c:1242-1249` re-roots an absolute arg at
/// `module_dir` inside the sanitize itself. Upstream can stat that relative
/// name because it chdir'd into the destination; oc does not, so the caller
/// supplies the absolute equivalent to stat and keeps upstream's spelling to
/// print.
///
/// Path resolution therefore belongs to the caller, which knows its own
/// anchoring rules; what is shared - and what must not be duplicated - is the
/// choice between the two diagnostics and their exact wording.
pub fn check_alt_basis_dir_at(kind: ReferenceDirectoryKind, stat_path: &Path, reported: &Path) {
    let option = alt_dest_opt(kind);

    // `warn_log!` expands to a statement (it ends in `;`), so each arm keeps
    // it in statement position. Calling it directly as an arm value puts a
    // trailing-semicolon macro in expression position, which newer compilers
    // reject outright - and every other caller in the tree does it this way.
    match fs::metadata(stat_path) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            warn_log!("{option} arg is not a dir: {}", reported.display());
        }
        Err(_) => {
            warn_log!("{option} arg does not exist: {}", reported.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use logging::{DiagnosticEvent, VerbosityConfig, drain_events, init};

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
        let temp = test_support::create_tempdir();
        let basis = temp.path().join("prev");
        fs::create_dir(&basis).expect("create basis");

        assert!(warnings_for(&[link(basis)], temp.path()).is_empty());
    }

    /// upstream: `main.c:914` - a missing arg reports `does not exist`.
    #[test]
    fn a_missing_basis_reports_does_not_exist() {
        let temp = test_support::create_tempdir();
        let basis = temp.path().join("absent");

        assert_eq!(
            warnings_for(&[link(&basis)], temp.path()),
            vec![format!(
                "--link-dest arg does not exist: {}",
                basis.display()
            )]
        );
    }

    /// upstream: `main.c:916` - an arg that exists but is not a directory
    /// reports `is not a dir`.
    #[test]
    fn a_plain_file_basis_reports_is_not_a_dir() {
        let temp = test_support::create_tempdir();
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
    /// upstream: `main.c:889-891`; measured against real 3.5.0.
    #[test]
    fn one_trailing_separator_is_stripped_before_the_stat() {
        let temp = test_support::create_tempdir();
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
        let temp = test_support::create_tempdir();
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
    /// directory. upstream: `main.c:898-911` joins onto `curr_dir`.
    #[test]
    fn a_relative_basis_resolves_against_the_destination() {
        let temp = test_support::create_tempdir();
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

    /// The client checks basis dirs only when it is the RECEIVER of a REMOTE
    /// transfer. Exhaustive over both inputs: a push must stay silent (the
    /// server reports it) and a local copy must stay silent here (the
    /// local-copy path reports it), or the same run warns twice.
    #[test]
    fn client_only_checks_basis_dirs_on_a_remote_pull() {
        assert!(
            client_checks_basis_dirs(true, true),
            "remote pull must check"
        );
        assert!(
            !client_checks_basis_dirs(false, true),
            "remote push: the server receiver checks its own forwarded argv"
        );
        assert!(
            !client_checks_basis_dirs(true, false),
            "local copy: checked once the LocalCopyPlan destination resolves"
        );
        assert!(
            !client_checks_basis_dirs(false, false),
            "local push is local"
        );
    }

    /// Each alt-dest option names itself. upstream: `options.c:1450`.
    #[test]
    fn each_alt_dest_kind_names_its_own_option() {
        let temp = test_support::create_tempdir();
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
