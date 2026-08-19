//! Single resolver for workspace binaries under test.
//!
//! Every integration test that shells out to `oc-rsync` must run the binary
//! Cargo just built, never one left over from an earlier revision. The
//! package that declares the `oc-rsync` `[[bin]]` can say
//! `env!("CARGO_BIN_EXE_oc-rsync")`; every other package cannot - that is a
//! hard compile error. Those crates used to hand-roll a scan of
//! `target/{debug,release,dist}`, or walk up from `CARGO_MANIFEST_DIR`, which
//! resolves whatever happens to be on disk.
//!
//! This module replaces the scan with one Cargo-provided anchor: `build.rs`
//! derives the active profile directory from `OUT_DIR` and bakes it in as
//! `OC_RSYNC_TARGET_PROFILE_DIR`. Exactly one candidate path is ever
//! considered, and [`workspace_bin`] panics naming that path when it is
//! absent or not executable. There is no second candidate to fall back to and
//! no `Option` for a caller to turn into a silent skip.
//!
//! Resolving the right *path* is only half the guarantee. Cargo builds the
//! `oc-rsync` `[[bin]]` from the workspace root package, so a per-crate run
//! such as `cargo nextest run -p cli` compiles that crate's test binaries and
//! leaves `oc-rsync` untouched: the tests then exercise whatever revision was
//! built last. That failure reports as a behavioural regression - the tests
//! fail, the diff looks innocent - and has been misdiagnosed as a code defect
//! more than once. [`workspace_bin`] therefore also asserts the binary is
//! newer than every source Cargo says it was compiled from, reading Cargo's
//! own `<binary>.d` dependency file rather than guessing at a source set.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The profile directory of the current Cargo invocation.
///
/// This is `<target-dir>/[<triple>/]<profile>` - the directory Cargo links
/// workspace binaries into - captured at compile time by this crate's
/// `build.rs`. It tracks custom profiles, cross-compilation triples and
/// relocated target directories, so no runtime probing is needed.
#[must_use]
pub fn target_profile_dir() -> &'static Path {
    Path::new(env!("OC_RSYNC_TARGET_PROFILE_DIR"))
}

/// The single path a workspace binary named `name` can occupy.
///
/// Returns the candidate whether or not it exists, for callers that need to
/// report or probe it (see [`workspace_bin`] and
/// [`crate::locate_workspace_binary`]).
#[must_use]
pub fn workspace_bin_path(name: &str) -> PathBuf {
    target_profile_dir().join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

/// Resolve a workspace binary, panicking loudly if it is not usable.
///
/// # Panics
///
/// Panics naming the exact path when the binary is missing, is not a regular
/// file, (on Unix) is not executable, or is older than one of the sources it
/// was built from. Failing here is deliberate: a helper that returned `None`
/// would let callers print `skip:` and return, and a skip that can never be
/// satisfied is indistinguishable from a pass.
#[must_use]
pub fn workspace_bin(name: &str) -> PathBuf {
    let candidate = workspace_bin_path(name);
    assert!(
        candidate.is_file(),
        "{name} binary missing at {}; refusing to fall back to a stale build \
         (run `cargo build --bin {name}` for this profile first)",
        candidate.display()
    );
    assert_executable(&candidate);
    assert_fresh(name, &candidate);
    candidate
}

/// Assert `binary` is newer than every source Cargo compiled it from.
///
/// Existence proves only that *some* build happened. Cargo records which
/// files that build consumed in a Makefile-style dependency file beside the
/// binary (`--emit=dep-info`), so the source set is Cargo's own answer rather
/// than a heuristic scan of the tree.
///
/// Deliberately not memoized: the check costs one `read` plus a `stat` per
/// dependency, and a test process that resolves the binary a handful of times
/// pays it a handful of times. Caching would trade that for a hidden
/// assumption that the tree cannot change mid-process.
///
/// # Panics
///
/// Panics naming the newest offending source when the binary predates it, and
/// when the dependency file is absent - a missing `.d` would otherwise turn
/// this guarantee off silently, which is the failure mode it exists to stop.
fn assert_fresh(name: &str, binary: &Path) {
    let depfile = binary.with_extension("d");
    let listing = std::fs::read_to_string(&depfile).unwrap_or_else(|e| {
        panic!(
            "cannot read Cargo's dependency file {} for {name}: {e}; \
             without it the binary cannot be proven current",
            depfile.display()
        )
    });

    let built = mtime(binary);
    if let Some(source) = dependencies(&listing).find(|dep| mtime(dep) > built) {
        panic!(
            "{name} at {} is older than {} - it was built from an earlier revision.\n\
             Run `cargo build --bin {name}` for this profile before these tests; \
             a stale binary fails as though the code under test had regressed.",
            binary.display(),
            source.display()
        );
    }
}

/// The source paths listed in a Cargo dependency file.
///
/// The format is `<target>: <dep> <dep> ...` with spaces inside a path
/// escaped as `\ `. Splitting on the target is done at the first colon
/// followed by whitespace so a Windows drive letter (`C:\...`) is not mistaken
/// for the separator.
///
/// Paths under a `.git` directory are skipped. Build scripts that embed
/// revision metadata record them, and they change on every commit or fetch
/// without altering a byte of program input - treating them as sources would
/// demand a rebuild after `git commit`.
fn dependencies(listing: &str) -> impl Iterator<Item = PathBuf> + '_ {
    let deps = listing
        .char_indices()
        .find(|&(i, c)| c == ':' && listing[i + 1..].starts_with([' ', '\t', '\n', '\r']))
        .map_or("", |(i, _)| &listing[i + 1..]);

    split_escaped(deps)
        .map(PathBuf::from)
        .filter(|p| !p.components().any(|c| c.as_os_str() == ".git"))
}

/// Split on unescaped whitespace, turning `\ ` back into a literal space.
///
/// A backslash escapes only a space or a line-continuation newline. Anything
/// else after it is data: Windows dependency paths are written with their
/// `\` separators unescaped, so consuming the next character unconditionally
/// would turn `C:\src\lib.rs` into `C:srclib.rs` - a path that never exists
/// and therefore always looks fresh.
fn split_escaped(deps: &str) -> impl Iterator<Item = String> + '_ {
    let mut entries = Vec::new();
    let mut current = String::new();
    let mut chars = deps.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.peek() {
                Some(' ') => {
                    current.push(' ');
                    chars.next();
                }
                Some('\n') | Some('\r') => {
                    chars.next();
                }
                _ => current.push('\\'),
            },
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    entries.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        entries.push(current);
    }
    entries.into_iter()
}

/// Modification time of `path`, or the epoch when it cannot be read.
///
/// A dependency Cargo recorded but that no longer exists (a deleted source)
/// must not be reported as newer than the binary: the rebuild it implies is
/// Cargo's business, not this guard's.
fn mtime(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Resolve the `oc-rsync` binary built by the current Cargo invocation.
///
/// # Panics
///
/// See [`workspace_bin`].
#[must_use]
pub fn oc_rsync_bin() -> PathBuf {
    workspace_bin("oc-rsync")
}

/// Assert the resolved path carries an execute bit.
///
/// A present-but-unexecutable file would otherwise surface as an opaque
/// `Permission denied` from `Command::spawn` deep inside a test body.
#[cfg(unix)]
fn assert_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
        .permissions()
        .mode();
    assert!(
        mode & 0o111 != 0,
        "{} exists but is not executable (mode {mode:o})",
        path.display()
    );
}

/// No-op on platforms without POSIX permission bits.
#[cfg(not(unix))]
fn assert_executable(_path: &Path) {}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn profile_dir_is_the_directory_holding_this_test_executable() {
        // Why: the whole mechanism rests on build.rs deriving the same
        // profile directory that the test harness is linked into. If those
        // ever diverge, every consumer would resolve a binary from a
        // different profile - exactly the stale-build hazard this replaces.
        let exe = std::env::current_exe().expect("current_exe");
        // `.../<profile>/deps/<test-bin>` -> `.../<profile>`.
        let from_exe = exe
            .parent()
            .and_then(Path::parent)
            .expect("test executable lives under <profile>/deps");
        assert_eq!(
            std::fs::canonicalize(from_exe).expect("canonicalize exe profile dir"),
            std::fs::canonicalize(target_profile_dir()).expect("canonicalize baked profile dir"),
        );
    }

    #[test]
    fn candidate_path_is_a_direct_child_of_the_profile_dir() {
        // Why: pins the layout contract consumers rely on. A binary nested
        // any deeper would mean build.rs picked the wrong ancestor.
        let p = workspace_bin_path("oc-rsync");
        assert_eq!(p.parent(), Some(target_profile_dir()));
        assert!(
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("oc-rsync"))
        );
    }

    /// A binary plus its Cargo dependency file, with `binary` stamped
    /// `binary_age` seconds after the epoch and every source one second after.
    ///
    /// Timestamps are set explicitly because the property under test is an
    /// mtime ordering: creating files in sequence and hoping the filesystem
    /// resolves them apart makes the assertion depend on timestamp
    /// granularity, which is how a guard like this silently stops guarding.
    fn fixture(dir: &Path, binary_age: u64, sources: &[&str]) -> PathBuf {
        use filetime::{FileTime, set_file_mtime};

        let binary = dir.join("fakebin");
        fs::write(&binary, b"").expect("binary");

        let mut listing = format!("{}:", binary.display());
        for source in sources {
            let path = dir.join(source);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("source parent");
            }
            fs::write(&path, b"").expect("source");
            set_file_mtime(&path, FileTime::from_unix_time(1, 0)).expect("stamp source");
            listing.push(' ');
            listing.push_str(&path.display().to_string());
        }
        fs::write(binary.with_extension("d"), listing).expect("depfile");
        set_file_mtime(&binary, FileTime::from_unix_time(binary_age as i64, 0))
            .expect("stamp binary");
        binary
    }

    const NEWER_THAN_SOURCES: u64 = 2;
    const OLDER_THAN_SOURCES: u64 = 0;

    #[test]
    #[should_panic(expected = "built from an earlier revision")]
    fn a_binary_older_than_its_sources_panics() {
        // Why: this is the whole point of the guard. A stale binary makes
        // every test that shells out to it fail as though the code under test
        // had regressed, and the diff under review looks innocent.
        let tmp = TempDir::new().expect("tempdir");
        let binary = fixture(tmp.path(), OLDER_THAN_SOURCES, &["src/lib.rs"]);
        assert_fresh("fakebin", &binary);
    }

    #[test]
    fn a_binary_newer_than_its_sources_is_accepted() {
        // Why: the non-vacuity companion. Without it the panic above would
        // also be satisfied by a guard that rejects every binary, which would
        // make the whole suite unrunnable rather than correct.
        let tmp = TempDir::new().expect("tempdir");
        let binary = fixture(tmp.path(), NEWER_THAN_SOURCES, &["src/lib.rs"]);
        assert_fresh("fakebin", &binary);
    }

    #[test]
    fn git_metadata_never_demands_a_rebuild() {
        // Why: build scripts embedding revision info record `.git/HEAD` as a
        // dependency. It changes on every commit, checkout and fetch without
        // altering a byte of program input, so honouring it would demand a
        // relink after each commit and train everyone to distrust the guard.
        let tmp = TempDir::new().expect("tempdir");
        let binary = fixture(tmp.path(), OLDER_THAN_SOURCES, &[".git/HEAD"]);
        assert_fresh("fakebin", &binary);
    }

    #[test]
    fn a_dependency_cargo_recorded_but_since_deleted_is_not_stale() {
        // Why: a removed source is a rebuild Cargo owns, not a fault to
        // report here. Treating an unreadable path as infinitely new would
        // wedge the suite until someone rebuilt for no stated reason.
        let tmp = TempDir::new().expect("tempdir");
        let binary = fixture(tmp.path(), OLDER_THAN_SOURCES, &["src/lib.rs"]);
        fs::remove_file(tmp.path().join("src/lib.rs")).expect("remove source");
        assert_fresh("fakebin", &binary);
    }

    #[test]
    #[should_panic(expected = "cannot read Cargo's dependency file")]
    fn a_missing_dependency_file_panics_rather_than_skipping_the_check() {
        // Why: silently passing when the evidence is absent is exactly the
        // vacuous-skip pattern this module exists to refuse.
        let tmp = TempDir::new().expect("tempdir");
        let binary = tmp.path().join("fakebin");
        fs::write(&binary, b"").expect("binary");
        assert_fresh("fakebin", &binary);
    }

    #[test]
    fn a_windows_drive_letter_is_not_mistaken_for_the_target_separator() {
        // Why: `C:\out\oc-rsync.exe` contains a colon before the real
        // separator. Splitting on the first colon would treat the drive
        // letter as the target and the rest of the path as a dependency,
        // silently checking a file that does not exist.
        let deps: Vec<PathBuf> =
            dependencies(r"C:\out\oc-rsync.exe: C:\src\lib.rs C:\src\main.rs").collect();
        assert_eq!(
            deps,
            vec![
                PathBuf::from(r"C:\src\lib.rs"),
                PathBuf::from(r"C:\src\main.rs")
            ]
        );
    }

    #[test]
    fn an_escaped_space_stays_inside_one_dependency_path() {
        // Why: the dependency-file format escapes spaces, so a checkout under
        // a path containing one would otherwise split into two nonexistent
        // files - and two nonexistent files always look fresh.
        let deps: Vec<PathBuf> = dependencies(r"/out/bin: /my\ src/lib.rs /other.rs").collect();
        assert_eq!(
            deps,
            vec![PathBuf::from("/my src/lib.rs"), PathBuf::from("/other.rs")]
        );
    }

    #[test]
    fn the_real_dependency_file_lists_the_sources_it_is_read_for() {
        // Why: every case above runs on a synthetic listing. This one pins
        // that the parser's shape matches what Cargo actually writes next to
        // the binary under test - the input the guard sees in production.
        //
        // Returning early when the binary was never built is not a vacuous
        // skip: in that state `workspace_bin` already panics for every
        // consumer, so there is no path this test could still protect.
        let binary = workspace_bin_path("oc-rsync");
        if !binary.is_file() {
            return;
        }
        let depfile = binary.with_extension("d");
        let listing = fs::read_to_string(&depfile)
            .unwrap_or_else(|e| panic!("read {}: {e}", depfile.display()));
        let deps: Vec<PathBuf> = dependencies(&listing).collect();
        assert!(
            deps.iter()
                .filter(|p| p.extension() == Some("rs".as_ref()))
                .count()
                > 100,
            "expected Cargo to record the workspace sources, parsed {} deps",
            deps.len()
        );
    }

    #[test]
    #[should_panic(expected = "refusing to fall back to a stale build")]
    fn missing_binary_panics_naming_the_path() {
        // Why: this is the guarantee the resolver exists for. A missing
        // binary must abort the test loudly, never yield None for a caller
        // to convert into a vacuous skip.
        let _ = workspace_bin("definitely-not-built-xyzzy");
    }
}
