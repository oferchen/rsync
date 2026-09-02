//! Verifies that `upstream: <file>:<line>` citations name a file the pinned
//! upstream rsync source has, at a line that file has.
//!
//! Two rules, both mechanical, both with no false-positive tension:
//!
//! 1. **The cited file must exist.** `tools/ci/run_upstream_testsuite.sh` cited
//!    `runtests.sh:205-215` and `runtests.sh:254`. Neither rsync 3.4.4 nor
//!    3.5.0 ships a `runtests.sh`; both ship `runtests.py`. The citation named
//!    a file that has never existed, and nothing noticed for as long as it took
//!    someone to read the script, because every citation checker in the tree
//!    only ever opened `crates/**/*.rs`.
//! 2. **The cited line must be inside the file.** `crates/metadata/src/apply/
//!    permissions.rs` cited `rsync.c:954-965` for a `dest_mode()` call site;
//!    rsync 3.4.4's `rsync.c` is 831 lines. The claim looked specific enough
//!    that seven call sites copied it.
//!
//! # What is deliberately NOT checked, and must not be "finished"
//!
//! Neither rule says anything about whether the cited line means what the
//! comment claims. That is the open question, not an oversight:
//!
//! - Checking that a comment's quoted text appears in the cited file was
//!   measured and rejected: 27-52% of citations fail it depending on the
//!   matcher, and the failures are dominated by correct documentation idioms -
//!   `foo()` names a function and never appears verbatim in C, a comment
//!   routinely cites a call site while naming the callee, and quoted spans are
//!   deliberately elided with `...` or paraphrased. Gating on that would mean
//!   annotating hundreds of correct comments, and an exemption that common
//!   stops being read.
//! - Line-number *accuracy* is worse than unsettled, it is version-bound. The
//!   same citation set audited at 11% suspected drift against 3.4.4 and 93%
//!   against 3.5.0, which is why moving the pin meant retargeting the corpus
//!   rather than editing one constant. Whether these citations should carry
//!   line numbers at all, or name a function and let the reader grep, remains
//!   open. `tools/ci/citation_drift_audit.py` reports on that question as a
//!   ratchet; it is not, and must not become, a hard gate.
//!
//!   Both pins move together or neither does. This gate and the drift audit
//!   read the same comments against different upstream trees, so a release
//!   flipped in one and not the other makes them contradict: the retarget that
//!   satisfies the audit names lines the older manifest does not have.
//!
//! # Why a manifest instead of the source tree
//!
//! The required `nextest` cell does not fetch the upstream tarball, so a gate
//! that reads `target/interop/upstream-src/` there would skip - and a gate that
//! skips in the one place it must run is not a gate. `tools/ci/
//! upstream-3.5.0-lines.tsv` carries one line count per upstream file. rsync
//! 3.5.0 is a released tarball and is immutable, so the manifest cannot drift
//! from it; `tests::manifest_matches_the_pinned_source` re-derives it wherever
//! the source is present.

use crate::error::TaskResult;
use crate::util::{list_sources_via_git_unfiltered, validation_error};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Committed line counts for every citable source file in the pinned upstream
/// release: `.c`, `.h`, `.py` and `.sh`.
pub const MANIFEST_PATH: &str = "tools/ci/upstream-3.5.0-lines.tsv";

/// The pinned upstream source, present only where the interop tarball was fetched.
pub const PINNED_SOURCE_DIR: &str = "target/interop/upstream-src/rsync-3.5.0";

/// The pinned release, as the `rsync-<VER>/` component of a cited path spells it.
pub const PINNED_VERSION: &str = "3.5.0";

/// Where `tools/ci/run_interop.sh` unpacks the tarball. Citations written while
/// reading the unpacked tree routinely paste this whole prefix, so the resolver
/// has to see past it; see [`split_release_prefix`].
const UNPACK_PREFIX: &str = "target/interop/upstream-src/";

/// Sources permitted to spell a citation at a release the gate does not pin.
///
/// A RULE with exactly two members, not a list that grows: these are the two
/// files that *document this gate*, and both have to be able to write a
/// rejected citation verbatim to show what the rule rejects. `CONTRIBUTING.md`
/// illustrates the out-of-scope form for contributors and
/// `xtask/src/commands/citations.rs` is this file, whose own prose names the
/// historical `rsync-3.4.1/` audit notes. Retargeting either at the pin would
/// destroy the example it exists to give. The exemption covers only
/// [`Violation::UnpinnedRelease`]: a phantom file or an out-of-range line in
/// these two files still fails.
const RULE_DOCUMENTATION_SOURCES: [&str; 2] =
    ["CONTRIBUTING.md", "xtask/src/commands/citations.rs"];

/// Whether `source` may cite a non-pinned release without it being a defect.
///
/// Two clauses. [`RULE_DOCUMENTATION_SOURCES`] carries the first.
///
/// The second is `docs/`, and it is a scope statement rather than debt swept
/// under a rug. Documentation there mixes live reference with historical
/// record, and a note recording what upstream looked like at 3.4.1 - a design
/// review, an audit write-up, a decision log - is accurate *because* it names
/// the old release; retargeting it at the pin would falsify it. Measured, so
/// the size of the exemption is stated rather than discovered later: 253
/// citations across 70 files under `docs/` name a non-pinned release. Deciding
/// which of those are live reference and which are record needs a reader per
/// file, which is a separate change from widening the gate.
fn unpinned_release_is_permitted(source: &str) -> bool {
    source.starts_with("docs/") || RULE_DOCUMENTATION_SOURCES.contains(&source)
}

/// Extensions a citation may name. Upstream carries protocol logic in C and its
/// test and release harness in Python and shell, and citations reach all four -
/// `runtests.py`, `support/lsh.sh` and `packaging/release.py` are cited by the
/// CI tooling exactly as `flist.c` is cited by the crates.
const CITED_EXTENSIONS: [&str; 4] = ["c", "h", "py", "sh"];

/// Header written above the generated counts, explaining why the file exists.
const MANIFEST_HEADER: &str = "\
# Line counts for every .c/.h/.py/.sh in rsync 3.5.0, the pinned upstream source.
# Lets the citation gate run where the source tree is absent (the required
# nextest cell does not fetch it). rsync 3.5.0 is a released tarball and is
# immutable, so this cannot drift; it is regenerated only when the pin moves.
# The key set is also the gate's answer to \"does this upstream file exist\", so
# a name missing here is a citation defect, not a manifest gap.
# Regenerate: cargo xtask citations --write-manifest  (needs the source present)
";

/// One citation that cannot be followed to the pinned upstream source.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Violation {
    /// The cited file exists but does not have the cited line.
    OutOfRange {
        /// Workspace-relative file carrying the citation.
        source: String,
        /// Line in the citing file.
        line: usize,
        /// Upstream file as resolved against the manifest.
        upstream: String,
        /// The out-of-range line number that was cited.
        cited: usize,
        /// Number of lines the upstream file actually has.
        actual: usize,
    },
    /// The pinned upstream release has no file of that name at all.
    MissingFile {
        /// Workspace-relative file carrying the citation.
        source: String,
        /// Line in the citing file.
        line: usize,
        /// The upstream file name as written in the comment.
        upstream: String,
        /// The line number that was cited, echoed so the site is greppable.
        cited: usize,
    },
    /// The citation spells out a release, and it is not the one this gate pins.
    ///
    /// These were invisible before: a path carrying a directory component fell
    /// straight into `ForeignUpstream` and was skipped without even being
    /// counted, so a citation naming a release the tree no longer builds
    /// against passed a gate that never opened it.
    UnpinnedRelease {
        /// Workspace-relative file carrying the citation.
        source: String,
        /// Line in the citing file.
        line: usize,
        /// The upstream path with the release prefix stripped.
        upstream: String,
        /// The line number that was cited, echoed so the site is greppable.
        cited: usize,
        /// The release the citation names.
        version: String,
    },
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfRange {
                source,
                line,
                upstream,
                cited,
                actual,
            } => write!(
                f,
                "{source}:{line}: cites {upstream}:{cited} but {upstream} has {actual} lines"
            ),
            Self::MissingFile {
                source,
                line,
                upstream,
                cited,
            } => write!(
                f,
                "{source}:{line}: cites {upstream}:{cited} but rsync 3.5.0 has no {upstream}"
            ),
            Self::UnpinnedRelease {
                source,
                line,
                upstream,
                cited,
                version,
            } => write!(
                f,
                "{source}:{line}: cites rsync-{version}/{upstream}:{cited}, but the pin is \
                 rsync {PINNED_VERSION}; re-locate the construct in {PINNED_SOURCE_DIR} and \
                 cite it as {upstream}:<line> there"
            ),
        }
    }
}

/// Parses the manifest into `upstream path -> line count`.
pub fn load_manifest(workspace: &Path) -> TaskResult<BTreeMap<String, usize>> {
    let raw = fs::read_to_string(workspace.join(MANIFEST_PATH))?;
    Ok(parse_manifest(&raw))
}

fn parse_manifest(raw: &str) -> BTreeMap<String, usize> {
    raw.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let (path, count) = l.split_once('\t')?;
            Some((path.to_owned(), count.trim().parse().ok()?))
        })
        .collect()
}

/// What a cited path turned out to be.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Resolution {
    /// A file of the pinned rsync release, at this manifest path.
    Pinned(String),
    /// A bare name several manifest paths share, so the file exists but there
    /// is no single one to range-check. rsync ships both `compat.c` and
    /// `lib/compat.c`; a comment writing the bare form has named a real file
    /// and is not a defect, it is merely unresolvable.
    Ambiguous,
    /// A bare name the pinned release does not have. This is the `runtests.sh`
    /// case: no directory component, so it cannot be a reference to some other
    /// project, and no such upstream file exists.
    Missing,
    /// Carries a directory component the pinned tarball does not have.
    ///
    /// RULE, not an allow-list: a path qualified by a directory rsync does not
    /// ship is a citation to a *different* upstream and is out of scope for a
    /// gate that only knows rsync. The zsync references in `crates/matching`
    /// are written `librcksum/rsum.c:262`, and that is the whole of what this
    /// arm now covers.
    ///
    /// It used to cover far more. An explicitly-versioned
    /// `target/interop/upstream-src/rsync-3.4.1/flist.c:713` also landed here,
    /// on the reading that any unshipped directory means a different project -
    /// which swallowed every citation of rsync that happened to name its own
    /// release. Measured when this arm was narrowed: 379 citations were being
    /// skipped, and 113 of them named the PINNED release, so their line numbers
    /// had never been checked by anything. A release directory is now split off
    /// before this arm is ever reached; see [`Resolution::UnpinnedRelease`].
    ForeignUpstream,
    /// The path spells `rsync-<VER>/` for a release other than the pin.
    ///
    /// This used to be folded into [`Resolution::ForeignUpstream`] on the
    /// reasoning that a versioned path names "a different upstream". It does
    /// not: it names *this* upstream at a release the tree no longer builds
    /// against, which is the definition of a stale citation rather than an
    /// out-of-scope one. `librcksum/rsum.c` is still foreign;
    /// `rsync-3.4.1/delete.c:130` is a defect.
    UnpinnedRelease {
        /// The release the path spells.
        version: String,
        /// The path with the release prefix stripped.
        path: String,
    },
}

/// Splits a leading `[target/interop/upstream-src/]rsync-<VER>/` off a cited path.
///
/// Both spellings occur and mean the same thing: a comment written while
/// reading the unpacked tarball pastes the whole workspace-relative path, one
/// written from memory keeps only the release directory. Returns
/// `(version, remainder)`; `None` when there is no release prefix at all.
fn split_release_prefix(cited: &str) -> Option<(&str, &str)> {
    let rest = cited.strip_prefix(UNPACK_PREFIX).unwrap_or(cited);
    let (version, tail) = rest.strip_prefix("rsync-")?.split_once('/')?;
    let numeric = !version.is_empty()
        && version
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'.' || b == b'-');
    numeric.then_some((version, tail))
}

/// Resolves a cited path against the manifest.
///
/// A release prefix is stripped first, so `rsync-3.5.0/flist.c:2477` is checked
/// exactly as `flist.c:2477` is instead of being skipped as foreign; naming a
/// release other than the pin is a violation rather than a skip. Resolution of
/// what is left is by exact path first so `lib/wildmatch.c` beats a bare-name
/// guess, then by unique basename so the common `flist.c:2477` form keeps
/// working.
pub fn resolve_upstream_path(cited: &str, manifest: &BTreeMap<String, usize>) -> Resolution {
    if let Some((version, tail)) = split_release_prefix(cited) {
        if version != PINNED_VERSION {
            return Resolution::UnpinnedRelease {
                version: version.to_owned(),
                path: tail.to_owned(),
            };
        }
        // The prefix names the pinned release outright, so a tail that resolves
        // nowhere is a phantom file - it cannot be a reference to some other
        // project the way a bare `librcksum/rsum.c` can.
        return match resolve_within_release(tail, manifest) {
            Resolution::ForeignUpstream => Resolution::Missing,
            other => other,
        };
    }
    resolve_within_release(cited, manifest)
}

/// Resolves a cited path that carries no release prefix.
fn resolve_within_release(cited: &str, manifest: &BTreeMap<String, usize>) -> Resolution {
    if manifest.contains_key(cited) {
        return Resolution::Pinned(cited.to_owned());
    }
    if cited.contains('/') {
        return Resolution::ForeignUpstream;
    }
    let mut hits = manifest.keys().filter(|k| {
        Path::new(k)
            .file_name()
            .is_some_and(|n| n == std::ffi::OsStr::new(cited))
    });
    let Some(first) = hits.next() else {
        return Resolution::Missing;
    };
    if hits.next().is_some() {
        return Resolution::Ambiguous;
    }
    Resolution::Pinned(first.clone())
}

/// Byte index of the `:` ending a `.<ext>:` at `dot`, for any cited extension.
fn extension_end(bytes: &[u8], dot: usize) -> Option<usize> {
    if bytes[dot] != b'.' {
        return None;
    }
    CITED_EXTENSIONS.iter().find_map(|ext| {
        let colon = dot + 1 + ext.len();
        (bytes.get(dot + 1..colon) == Some(ext.as_bytes()) && bytes.get(colon) == Some(&b':'))
            .then_some(colon)
    })
}

/// Extracts `(cited path, line numbers)` from one comment line.
///
/// Hand-rolled rather than a regex so the path component can absorb a leading
/// directory (`lib/wildmatch.c`) without also swallowing the surrounding prose.
/// Both ends of a `1234-1240` range are returned, since a range that starts
/// inside the file can still end past its last line.
pub fn citations_in_line(line: &str) -> Vec<(String, Vec<usize>)> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let Some(colon) = extension_end(bytes, i) else {
            i += 1;
            continue;
        };
        let mut start = i;
        while start > 0 {
            let c = bytes[start - 1];
            if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'/' || c == b'.' {
                start -= 1;
            } else {
                break;
            }
        }
        let path = &line[start..colon];
        let mut j = colon + 1;
        let mut nums = Vec::new();
        let mut cur = String::new();
        while j < bytes.len() {
            if bytes[j].is_ascii_digit() {
                cur.push(bytes[j] as char);
                j += 1;
            } else if bytes[j] == b'-'
                && !cur.is_empty()
                && j + 1 < bytes.len()
                && bytes[j + 1].is_ascii_digit()
            {
                if let Ok(v) = cur.parse() {
                    nums.push(v);
                }
                cur.clear();
                j += 1;
            } else {
                break;
            }
        }
        if let Ok(v) = cur.parse() {
            nums.push(v);
        }
        if !path.is_empty() && !nums.is_empty() {
            out.push((path.to_owned(), nums));
        }
        i = j.max(colon + 1);
    }
    out
}

/// Collects every unfollowable citation in `contents` and tallies what was
/// examined. The tallies are what let callers tell "clean" apart from
/// "examined nothing".
pub fn scan_contents(
    source: &str,
    contents: &str,
    manifest: &BTreeMap<String, usize>,
    tally: &mut ScanTally,
) -> Vec<Violation> {
    let mut out = Vec::new();
    for (lineno, line) in contents.lines().enumerate() {
        if !line.to_ascii_lowercase().contains("upstream") {
            continue;
        }
        for (cited_path, nums) in citations_in_line(line) {
            let resolution = resolve_upstream_path(&cited_path, manifest);
            // Counted BEFORE the skip, not after. The bug this replaces
            // incremented `names_checked` below the `continue`, so a skipped
            // citation left no trace at all: 379 of them were invisible in a
            // report whose whole purpose is to distinguish "clean" from
            // "examined nothing".
            if matches!(resolution, Resolution::ForeignUpstream) {
                tally.foreign_skipped += 1;
                continue;
            }
            if let Resolution::UnpinnedRelease { version, path } = resolution {
                if unpinned_release_is_permitted(source) {
                    tally.unpinned_permitted += 1;
                    continue;
                }
                tally.names_checked += 1;
                out.push(Violation::UnpinnedRelease {
                    source: source.to_owned(),
                    line: lineno + 1,
                    upstream: path,
                    cited: nums[0],
                    version,
                });
                continue;
            }
            tally.names_checked += 1;
            let resolved = match resolution {
                Resolution::Pinned(path) => path,
                // The file exists under several paths; nothing to range-check.
                Resolution::Ambiguous => continue,
                Resolution::Missing => {
                    out.push(Violation::MissingFile {
                        source: source.to_owned(),
                        line: lineno + 1,
                        upstream: cited_path,
                        cited: nums[0],
                    });
                    continue;
                }
                Resolution::ForeignUpstream | Resolution::UnpinnedRelease { .. } => {
                    unreachable!("filtered above")
                }
            };
            tally.ranges_checked += 1;
            let actual = manifest[&resolved];
            for n in nums {
                if n > actual {
                    out.push(Violation::OutOfRange {
                        source: source.to_owned(),
                        line: lineno + 1,
                        upstream: resolved.clone(),
                        cited: n,
                        actual,
                    });
                    break;
                }
            }
        }
    }
    out
}

/// How much each rule actually examined during a scan.
#[derive(Debug, Default, Clone, Copy)]
pub struct ScanTally {
    /// Citations whose file name was tested against the pinned release.
    pub names_checked: usize,
    /// Citations that resolved to one pinned file and were range-checked.
    pub ranges_checked: usize,
    /// Citations skipped as naming a project that is not rsync.
    ///
    /// Reported, not silent. A skip nobody counts is how 379 citations, 113 of
    /// them naming the pinned release itself, sat unexamined inside a green
    /// gate; publishing the number means the next time the skipped population
    /// grows, it grows in the open.
    pub foreign_skipped: usize,
    /// Non-pinned-release citations allowed by [`unpinned_release_is_permitted`].
    pub unpinned_permitted: usize,
}

/// What one full scan of the workspace saw.
#[derive(Debug, Default)]
pub struct ScanReport {
    /// Citations that cannot be followed to the pinned upstream source.
    pub violations: Vec<Violation>,
    /// Sources opened.
    pub files_read: usize,
    /// What each rule examined.
    pub tally: ScanTally,
}

/// Scans every file in the repository for unfollowable upstream citations.
///
/// Every file, not a curated extension list: the phantom `runtests.sh`
/// citations sat in a shell script for as long as they did because the only
/// checker in the tree globbed `crates/**/*.rs`. Whatever cannot be decoded as
/// UTF-8 is skipped, which is how binary goldens stay out of the way without
/// anyone having to name them.
///
/// Refuses to return a clean report it did not earn. An empty manifest, a
/// source list that resolves to nothing, or a citation extractor that stopped
/// matching would each leave `violations` empty and look identical to success -
/// which is precisely how the Python audit this replaces reported healthy for
/// months while reading zero files.
pub fn collect_violations(workspace: &Path) -> TaskResult<ScanReport> {
    let manifest = load_manifest(workspace)?;
    if manifest.is_empty() {
        return Err(validation_error(format!(
            "{MANIFEST_PATH} parsed to zero entries; every citation would resolve \
             to nothing and the check would pass without examining anything"
        )));
    }
    let mut report = ScanReport::default();
    for relative in list_sources_via_git_unfiltered(workspace)? {
        let contents = match fs::read_to_string(workspace.join(&relative)) {
            Ok(c) => c,
            Err(_) => continue,
        };
        report.files_read += 1;
        report.violations.extend(scan_contents(
            &relative.display().to_string(),
            &contents,
            &manifest,
            &mut report.tally,
        ));
    }
    if report.files_read == 0 {
        return Err(validation_error(
            "read zero sources; `git ls-files` returned nothing, so a clean \
             result would mean nothing",
        ));
    }
    if report.tally.names_checked == 0 {
        return Err(validation_error(format!(
            "read {} source(s) but resolved ZERO upstream citations; the \
             citation extractor or {MANIFEST_PATH} is broken",
            report.files_read
        )));
    }
    Ok(report)
}

/// Re-derives the manifest by walking the pinned source tree.
///
/// Shared by `write_manifest` and the cross-check test so the two cannot drift
/// into disagreeing about which files belong in the manifest.
pub fn derive_manifest(root: &Path) -> TaskResult<BTreeMap<String, usize>> {
    let mut rows = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
    {
        let p = entry.path();
        let citable = p
            .extension()
            .is_some_and(|e| CITED_EXTENSIONS.iter().any(|ext| e == *ext));
        if !citable || !p.is_file() {
            continue;
        }
        let rel = p
            .strip_prefix(root)
            .map_err(|e| validation_error(e.to_string()))?
            .to_string_lossy()
            .replace('\\', "/");
        rows.insert(rel, fs::read_to_string(p)?.lines().count());
    }
    Ok(rows)
}

/// Rewrites the manifest from the pinned source. Needed only when the upstream
/// pin moves; the release it describes is immutable.
pub fn write_manifest(workspace: &Path) -> TaskResult<()> {
    let root = workspace.join(PINNED_SOURCE_DIR);
    if !root.is_dir() {
        return Err(validation_error(format!(
            "pinned source missing at {PINNED_SOURCE_DIR}; run tools/ci/run_interop.sh to fetch it"
        )));
    }
    let rows = derive_manifest(&root)?;
    let mut out = String::from(MANIFEST_HEADER);
    for (rel, n) in &rows {
        out.push_str(&format!("{rel}\t{n}\n"));
    }
    fs::write(workspace.join(MANIFEST_PATH), out)?;
    eprintln!("wrote {MANIFEST_PATH}: {} files", rows.len());
    Ok(())
}

/// Executes the `citations` command.
pub fn execute(workspace: &Path) -> TaskResult<()> {
    let report = collect_violations(workspace)?;
    if report.violations.is_empty() {
        eprintln!(
            "citations: {} name(s) and {} line range(s) checked across {} \
             file(s); {} skipped as not-rsync and {} permitted at a non-pinned \
             release - every checked citation names a file rsync {PINNED_VERSION} has, \
             at a line that file has. This does NOT mean the cited line says \
             what the comment claims; nothing here checks that.",
            report.tally.names_checked,
            report.tally.ranges_checked,
            report.files_read,
            report.tally.foreign_skipped,
            report.tally.unpinned_permitted,
        );
        return Ok(());
    }
    for v in &report.violations {
        eprintln!("{v}");
    }
    Err(validation_error(format!(
        "{} upstream citation(s) cannot be followed to {PINNED_SOURCE_DIR}; \
         re-locate the construct there and correct the file name or the line \
         number, or - if the behaviour has no upstream analogue any more - \
         drop the citation",
        report.violations.len()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `scan_contents` with the tally discarded, for tests that only care about
    /// the violations.
    fn scan(source: &str, contents: &str, m: &BTreeMap<String, usize>) -> Vec<Violation> {
        let mut tally = ScanTally::default();
        scan_contents(source, contents, m, &mut tally)
    }

    fn manifest() -> BTreeMap<String, usize> {
        parse_manifest(
            "# c\nrsync.c\t831\nflist.c\t3500\nlib/wildmatch.c\t100\n\
             runtests.py\t400\nsupport/lsh.sh\t60\ncompat.c\t700\nlib/compat.c\t272\n\
             popt/dup.h\t20\nzlib/dup.h\t30\n",
        )
    }

    /// The cited line number of a violation, whichever kind it is.
    fn cited_line(v: &Violation) -> usize {
        match v {
            Violation::OutOfRange { cited, .. }
            | Violation::MissingFile { cited, .. }
            | Violation::UnpinnedRelease { cited, .. } => *cited,
        }
    }

    /// Builds a citation at run time so this file's own fixtures are not
    /// flagged by the scanner it tests. `no_placeholders` splits its marker
    /// literals for the same reason.
    fn cite(file: &str, span: &str) -> String {
        format!(
            "// upstream: {file}{}{span} (`dest_mode()` invocation)",
            ":"
        )
    }

    #[test]
    fn parses_path_and_both_ends_of_a_range() {
        let got = citations_in_line(&cite("rsync.c", "954-965"));
        assert_eq!(got, vec![("rsync.c".to_owned(), vec![954, 965])]);
    }

    #[test]
    fn parses_a_directory_qualified_path() {
        let got = citations_in_line(&cite("librcksum/rsum.c", "262"));
        assert_eq!(got, vec![("librcksum/rsum.c".to_owned(), vec![262])]);
    }

    #[test]
    fn parses_a_python_and_a_shell_path() {
        // The defect that motivated the file-existence rule was a `.sh`
        // citation, so the extractor has to see past `.c`/`.h`.
        assert_eq!(
            citations_in_line(&cite("runtests.py", "205-215")),
            vec![("runtests.py".to_owned(), vec![205, 215])]
        );
        assert_eq!(
            citations_in_line(&cite("support/lsh.sh", "42")),
            vec![("support/lsh.sh".to_owned(), vec![42])]
        );
    }

    #[test]
    fn resolves_bare_name_and_exact_path() {
        let m = manifest();
        assert_eq!(
            resolve_upstream_path("rsync.c", &m),
            Resolution::Pinned("rsync.c".to_owned())
        );
        assert_eq!(
            resolve_upstream_path("wildmatch.c", &m),
            Resolution::Pinned("lib/wildmatch.c".to_owned())
        );
        assert_eq!(
            resolve_upstream_path("lib/wildmatch.c", &m),
            Resolution::Pinned("lib/wildmatch.c".to_owned())
        );
    }

    #[test]
    fn a_foreign_upstream_is_out_of_scope() {
        // RULE: a directory the rsync tarball does not ship means the citation
        // is to some other project. That is now zsync's `librcksum/` and
        // nothing else - an explicitly-versioned rsync path is a citation of
        // rsync, and is handled by the release-prefix arm below.
        assert_eq!(
            resolve_upstream_path("librcksum/rsum.c", &manifest()),
            Resolution::ForeignUpstream
        );
    }

    #[test]
    fn a_pinned_release_prefix_is_stripped_and_the_range_is_checked() {
        // THE HOLE THIS CLOSES, half one. `rsync-3.5.0/rsync.c:954` names the
        // pinned release, yet the old resolver saw a `/` in a path that is not
        // a manifest key and skipped it as foreign - so the range check, the
        // entire point of the gate, never ran on it.
        let m = manifest();
        for spelling in [
            "rsync-3.5.0/rsync.c",
            "target/interop/upstream-src/rsync-3.5.0/rsync.c",
        ] {
            assert_eq!(
                resolve_upstream_path(spelling, &m),
                Resolution::Pinned("rsync.c".to_owned()),
                "{spelling}"
            );
        }
        let v = scan(
            "a.rs",
            &cite("target/interop/upstream-src/rsync-3.5.0/rsync.c", "954-965"),
            &m,
        );
        assert_eq!(v.len(), 1);
        assert!(
            matches!(v[0], Violation::OutOfRange { actual: 831, .. }),
            "{:?}",
            v[0]
        );
    }

    #[test]
    fn a_non_pinned_release_is_a_violation_not_a_skip() {
        // THE HOLE THIS CLOSES, half two. A 3.4.1 line number is a citation of
        // rsync at a release this tree no longer builds against; the old
        // resolver called that "a different upstream" and let it through.
        let m = manifest();
        assert_eq!(
            resolve_upstream_path("target/interop/upstream-src/rsync-3.4.1/flist.c", &m),
            Resolution::UnpinnedRelease {
                version: "3.4.1".to_owned(),
                path: "flist.c".to_owned(),
            }
        );
        let v = scan(
            "crates/x/src/lib.rs",
            &cite("target/interop/upstream-src/rsync-3.4.1/flist.c", "713"),
            &m,
        );
        assert_eq!(v.len(), 1);
        assert!(
            matches!(v[0], Violation::UnpinnedRelease { .. }),
            "{:?}",
            v[0]
        );
        assert_eq!(cited_line(&v[0]), 713);
        let msg = v[0].to_string();
        // Assembled rather than written out, so this file's own scan does not
        // see a citation in the assertion.
        let stale_coordinate = format!("rsync-3.4.1/flist.c{}713", ":");
        assert!(
            msg.contains(&stale_coordinate) && msg.contains(PINNED_VERSION),
            "the message must name both the stale release and the pin: {msg}"
        );
    }

    #[test]
    fn a_non_pinned_release_is_permitted_only_where_the_rule_is_documented() {
        // The exemption is scoped, and the scope is the point: `docs/` mixes
        // live reference with historical record, and the two files that
        // document this gate have to be able to write the rejected form.
        let m = manifest();
        let stale = cite("target/interop/upstream-src/rsync-3.4.1/flist.c", "713");
        for permitted in [
            "docs/audits/flist-3.4.1.md",
            "CONTRIBUTING.md",
            "xtask/src/commands/citations.rs",
        ] {
            assert!(
                scan(permitted, &stale, &m).is_empty(),
                "{permitted} must be allowed to name a non-pinned release"
            );
        }
        for gated in [
            "crates/engine/src/delete/mod.rs",
            "tools/ci/x.sh",
            "docsite/index.md",
        ] {
            assert_eq!(
                scan(gated, &stale, &m).len(),
                1,
                "{gated} must not inherit the exemption"
            );
        }
    }

    #[test]
    fn a_pinned_prefix_over_a_phantom_file_is_missing_not_foreign() {
        // The prefix asserts the pinned release outright, so an unresolvable
        // tail cannot be "some other project" the way `librcksum/rsum.c` can.
        assert_eq!(
            resolve_upstream_path("rsync-3.5.0/nowhere/phantom.c", &manifest()),
            Resolution::Missing
        );
    }

    #[test]
    fn a_release_shaped_directory_that_is_not_a_release_stays_foreign() {
        // `rsync-` followed by something that is not a version number is not a
        // release directory, and must not be silently stripped.
        assert_eq!(
            resolve_upstream_path("rsync-patches/flist.c", &manifest()),
            Resolution::ForeignUpstream
        );
    }

    #[test]
    fn a_bare_name_the_release_lacks_is_missing_not_foreign() {
        // Without a directory component there is nothing to say the citation
        // means a different project, so `rsum.c` is a defect while
        // `librcksum/rsum.c` is out of scope. This asymmetry is the whole
        // reason the existence rule has no false positives.
        assert_eq!(
            resolve_upstream_path("rsum.c", &manifest()),
            Resolution::Missing
        );
    }

    #[test]
    fn an_ambiguous_bare_name_exists_and_is_not_a_violation() {
        // A bare name several manifest paths share, none of them top-level. The
        // file exists, so this is not a missing-file defect; it just cannot be
        // range-checked. Note the near miss: rsync ships both `compat.c` and
        // `lib/compat.c`, but the exact-path branch resolves the bare form to
        // the top-level file, so the common case is checked, not skipped.
        let m = manifest();
        assert_eq!(resolve_upstream_path("dup.h", &m), Resolution::Ambiguous);
        assert_eq!(
            resolve_upstream_path("compat.c", &m),
            Resolution::Pinned("compat.c".to_owned())
        );
        assert!(scan("a.rs", &cite("dup.h", "999999"), &m).is_empty());
    }

    #[test]
    fn flags_a_file_the_pinned_release_does_not_have() {
        // THE DEFECT THIS RULE EXISTS FOR. `tools/ci/run_upstream_testsuite.sh`
        // cited `runtests.sh:205-215`; rsync ships `runtests.py` and has never
        // shipped a `runtests.sh`.
        let v = scan(
            "tools/ci/x.sh",
            &cite("runtests.sh", "205-215"),
            &manifest(),
        );
        assert_eq!(v.len(), 1);
        assert!(matches!(v[0], Violation::MissingFile { .. }), "{:?}", v[0]);
        assert_eq!(cited_line(&v[0]), 205);
        assert!(
            v[0].to_string().contains("runtests.sh"),
            "the message must name the missing target: {}",
            v[0]
        );
    }

    #[test]
    fn flags_a_line_past_the_end_of_the_file() {
        let v = scan("a.rs", &cite("rsync.c", "954-965"), &manifest());
        assert_eq!(v.len(), 1);
        assert!(
            matches!(v[0], Violation::OutOfRange { actual: 831, .. }),
            "{:?}",
            v[0]
        );
        assert_eq!(cited_line(&v[0]), 954);
    }

    #[test]
    fn flags_a_range_whose_end_overruns() {
        // The start is inside the file, so only checking the first number
        // would miss this.
        let v = scan("a.rs", &cite("rsync.c", "820-900"), &manifest());
        assert_eq!(v.len(), 1);
        assert_eq!(cited_line(&v[0]), 900);
    }

    #[test]
    fn accepts_an_in_range_citation() {
        assert!(scan("a.rs", &cite("rsync.c", "457-465"), &manifest()).is_empty());
    }

    #[test]
    fn ignores_lines_that_are_not_upstream_citations() {
        let not_a_citation = format!("let x = foo.c;\n// see rsync.{}9999\n", "c:");
        assert!(scan("a.rs", &not_a_citation, &manifest()).is_empty());
    }

    /// THE GATE. Every upstream citation in the repository must name a file the
    /// pinned release has, at a line that file has. Runs from the committed
    /// manifest, so it is not silently skipped in the cell that lacks the
    /// upstream tarball, and over every file git knows about, so it is not
    /// silently skipped in whichever directory the next defect lands in.
    ///
    /// WHY EVERY FILE. The file-existence rule was added because
    /// `tools/ci/run_upstream_testsuite.sh` cited a `runtests.sh` that has never
    /// existed in any rsync release. It survived because the only citation
    /// checkers in the tree globbed `crates/**/*.rs`, so every citation under
    /// `tools/`, `.github/` and `xtask/` was unexamined. A gate scoped to where
    /// the last defect was found does not cover where the next one lands.
    ///
    /// RESIDUAL, stated rather than hidden: the scan reaches `docs/`, but
    /// `.github/workflows/ci.yml` does not list `docs/**` among the paths that
    /// start the nextest cell, so a documentation-only pull request introducing
    /// a phantom citation is caught at the next change that does touch code or
    /// tooling, not at the pull request that added it.
    ///
    /// WHAT GREEN HERE DOES NOT MEAN. This proves only that the coordinates are
    /// reachable. It does not check that the cited line says what the comment
    /// claims, and it does not catch an off-by-N or a citation landing in the
    /// wrong region of the right file. Measured, not assumed: a sibling branch
    /// audited 26 of its own citations and found SIX naming the wrong line -
    /// every one of them in range, so every one of them green here. Four of the
    /// six had been copied from existing comments, which is how one wrong number
    /// reaches seven call sites (see the `rsync.c:954` case in the module docs).
    ///
    /// The sharpest instance came out of this gate's own sweep.
    /// `cli/frontend/server/flags.rs` cited `options.c:2750-2762` for
    /// "client forwards --log-format=%i" at BOTH :229 and :783. That range is
    /// the `--no-r` guard, the `--compress-level` asprintf and the `--devices`
    /// note - the --log-format block is 2768-2780. The sweep corrected :229 and
    /// left :783, in a file it had already edited, while specifically hunting
    /// wrong citations. Nothing here caught it; a person reading origin/master
    /// did.
    ///
    /// THE REMEDIATION RULE THAT FOLLOWS, which costs one grep. A finder only
    /// surfaces what its heuristic matched - this audit flagged :229 because an
    /// anchor happened to match there and never saw :783. So working a finding
    /// list is not a sweep. After correcting a flagged instance, grep the tree
    /// for the same defect STRING before closing, or the siblings the finder
    /// could not see survive the pass that was meant to remove them.
    ///
    /// Five broader designs were measured and rejected as hard gates, each
    /// dominated by correct documentation idioms rather than defects: quoted
    /// text anywhere in the file (27-52% fail depending on matcher), the cited
    /// line falling inside the named function's body (14%), requiring a
    /// uniquely-spelled quoted token to sit at the citation (21% - an option
    /// name legitimately cited at its popt table entry is spelled uniquely at
    /// the forwarding site instead), and the reverse direction, requiring the
    /// cited line to share an identifier with the comment (44%, the worst of
    /// the five). The reverse fails hardest because it penalises the
    /// best-written comments: `options.c:2392` is
    /// `if (daemon_bwlimit && (!bwlimit || bwlimit > daemon_bwlimit))` and its
    /// comment reads "min(client, daemon) wins" - a paraphrase that explains
    /// the rule instead of restating the identifiers, which is what a good
    /// comment does. Each design would need 130-250 in-source exemptions, and
    /// an exemption written that often stops being read.
    #[test]
    fn every_upstream_citation_names_a_file_and_line_that_exist() {
        let workspace = crate::workspace::workspace_root().expect("workspace root");
        let report = collect_violations(&workspace).expect("scan succeeds");
        assert!(
            report.violations.is_empty(),
            "upstream citations that cannot be followed to rsync 3.5.0:\n{}",
            report
                .violations
                .iter()
                .map(|v| format!("  {v}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        // A green run must have been earned. `collect_violations` already
        // refuses to return an unearned clean report; these pin the floor so a
        // future change that quietly narrows either rule's reach cannot pass as
        // success. Both are asserted: a scan that resolved names but stopped
        // range-checking would still look clean under one alone.
        assert!(
            report.tally.names_checked > 100 && report.tally.ranges_checked > 100,
            "only {} name(s) and {} range(s) checked across {} file(s) - the \
             scan collapsed, and an empty violation list here would mean nothing",
            report.tally.names_checked,
            report.tally.ranges_checked,
            report.files_read
        );
    }

    /// The gate must see past `crates/`. A floor on the count of non-Rust files
    /// read is what keeps that true: `collect_violations` could be narrowed back
    /// to a Rust glob and every other assertion here would still pass.
    #[test]
    fn the_scan_reaches_beyond_rust_sources() {
        let workspace = crate::workspace::workspace_root().expect("workspace root");
        let files = list_sources_via_git_unfiltered(&workspace).expect("git ls-files");
        let non_rust = files
            .iter()
            .filter(|p| p.extension().is_some_and(|e| e != "rs"))
            .count();
        assert!(
            non_rust > 500,
            "only {non_rust} non-Rust file(s) enumerated; the scan has been \
             narrowed back to a Rust glob, which is the hole this gate closed"
        );
        assert!(
            files
                .iter()
                .any(|p| p.ends_with("run_upstream_testsuite.sh")),
            "the shell script that carried the phantom citations is not in the \
             scan set"
        );
    }

    #[test]
    fn an_empty_manifest_is_an_error_not_a_clean_run() {
        // Every citation would resolve to nothing, so violations would be empty
        // and the gate would pass having checked nothing. That is the failure
        // mode this whole check exists to make impossible.
        let m: BTreeMap<String, usize> = parse_manifest("# only comments\n");
        assert!(m.is_empty());
        let mut tally = ScanTally::default();
        let v = scan_contents("a.rs", &cite("rsync.c", "954-965"), &m, &mut tally);
        // With no manifest the bare name looks missing rather than resolvable,
        // so the violation is real but meaningless. `collect_violations`
        // rejects an empty manifest outright for exactly that reason.
        assert_eq!(v.len(), 1);
        assert_eq!(
            tally.ranges_checked, 0,
            "nothing was range-checked - the tell"
        );
    }

    #[test]
    fn a_resolved_citation_is_counted_under_both_rules() {
        let mut tally = ScanTally::default();
        scan_contents("a.rs", &cite("rsync.c", "457-465"), &manifest(), &mut tally);
        assert_eq!(tally.names_checked, 1);
        assert_eq!(tally.ranges_checked, 1);
    }

    #[test]
    fn a_foreign_citation_is_counted_under_neither_rule_but_is_not_invisible() {
        // ORDERING. The skip used to `continue` above every tally increment, so
        // a skipped citation left no trace anywhere in the report - which is
        // how 379 of them, 113 of those naming the pin itself, hid in a green gate
        // whose stated job is telling "clean" apart from "examined nothing".
        // `names_checked` still excludes it, because nothing was checked; the
        // skip itself is what gets counted.
        let mut tally = ScanTally::default();
        let v = scan_contents(
            "a.rs",
            &cite("librcksum/rsum.c", "262"),
            &manifest(),
            &mut tally,
        );
        assert!(v.is_empty());
        assert_eq!(tally.names_checked, 0);
        assert_eq!(tally.ranges_checked, 0);
        assert_eq!(tally.foreign_skipped, 1, "the skip must be counted");
    }

    #[test]
    fn a_permitted_non_pinned_citation_is_counted_as_permitted() {
        let mut tally = ScanTally::default();
        let v = scan_contents(
            "docs/audits/old.md",
            &cite("target/interop/upstream-src/rsync-3.4.1/flist.c", "713"),
            &manifest(),
            &mut tally,
        );
        assert!(v.is_empty());
        assert_eq!(tally.unpinned_permitted, 1);
        assert_eq!(tally.foreign_skipped, 0);
    }

    /// Proves the committed manifest still describes the real pinned source.
    /// Conditional on the tarball being present - unlike the gate above, which
    /// must never be - because this test exists to catch a forged or stale
    /// manifest, and it can only do that where the source is available.
    #[test]
    fn manifest_matches_the_pinned_source() {
        let workspace = crate::workspace::workspace_root().expect("workspace root");
        let root = workspace.join(PINNED_SOURCE_DIR);
        if !root.is_dir() {
            eprintln!("pinned source absent at {PINNED_SOURCE_DIR}; manifest cross-check skipped");
            return;
        }
        let manifest = load_manifest(&workspace).expect("manifest loads");
        let derived = derive_manifest(&root).expect("pinned source walks");
        assert_eq!(
            manifest, derived,
            "{MANIFEST_PATH} is stale or hand-edited; regenerate it from {PINNED_SOURCE_DIR}"
        );
    }
}
