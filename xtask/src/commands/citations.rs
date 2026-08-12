//! Verifies that `upstream: <file>:<line>` citations point inside the pinned
//! upstream rsync source.
//!
//! A citation naming a line past the end of the file it names cannot be
//! checked by a reader and cannot be right. `crates/metadata/src/apply/
//! permissions.rs` cited `rsync.c:954-965` for a `dest_mode()` call site; rsync
//! 3.4.4's `rsync.c` is 831 lines. The claim looked specific enough that seven
//! call sites copied it.
//!
//! # Why only the line-range check
//!
//! Checking that a comment's quoted text appears in the cited file was measured
//! and rejected: 27-52% of citations fail it depending on the matcher, and the
//! failures are dominated by correct documentation idioms - `foo()` names a
//! function and never appears verbatim in C, a comment routinely cites a call
//! site while naming the callee, and quoted spans are deliberately elided with
//! `...` or paraphrased. Gating on that would mean annotating hundreds of
//! correct comments, and an exemption that common stops being read.
//!
//! This check has no such tension, so it carries no exemption mechanism: every
//! violation is a defect, and the only way to silence one is to fix it.
//!
//! # Why a manifest instead of the source tree
//!
//! The required `nextest` cell does not fetch the upstream tarball, so a gate
//! that reads `target/interop/upstream-src/` there would skip - and a gate that
//! skips in the one place it must run is not a gate. `tools/ci/
//! upstream-3.4.4-lines.tsv` carries one line count per upstream file. rsync
//! 3.4.4 is a released tarball and is immutable, so the manifest cannot drift
//! from it; `tests::manifest_matches_the_pinned_source` re-derives it wherever
//! the source is present.

use crate::error::TaskResult;
use crate::util::{list_rust_sources_via_git, validation_error};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Committed line counts for every `.c`/`.h` in the pinned upstream release.
pub const MANIFEST_PATH: &str = "tools/ci/upstream-3.4.4-lines.tsv";

/// The pinned upstream source, present only where the interop tarball was fetched.
pub const PINNED_SOURCE_DIR: &str = "target/interop/upstream-src/rsync-3.4.4";

/// Header written above the generated counts, explaining why the file exists.
const MANIFEST_HEADER: &str = "\
# Line counts for every .c/.h in rsync 3.4.4, the pinned upstream source.
# Lets the citation gate run where the source tree is absent (the required
# nextest cell does not fetch it). rsync 3.4.4 is a released tarball and is
# immutable, so this cannot drift; it is regenerated only when the pin moves.
# Regenerate: cargo xtask citations --write-manifest  (needs the source present)
";

/// One citation that names a line the cited file does not have.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Violation {
    /// Workspace-relative Rust file carrying the citation.
    pub source: String,
    /// Line in the Rust file.
    pub line: usize,
    /// Upstream file as resolved against the manifest.
    pub upstream: String,
    /// The out-of-range line number that was cited.
    pub cited: usize,
    /// Number of lines the upstream file actually has.
    pub actual: usize,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}: cites {}:{} but {} has {} lines",
            self.source, self.line, self.upstream, self.cited, self.upstream, self.actual
        )
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

/// Resolves a cited path against the manifest.
///
/// Returns `None` for anything that is not a file of the pinned rsync release,
/// which is how citations to a *different* upstream stay out of scope: the
/// zsync references in `crates/matching` are written `librcksum/rsum.c:262`,
/// and `librcksum/` is not a directory of the rsync tarball. Resolution is by
/// exact path first so `lib/wildmatch.c` beats a bare-name guess, then by
/// unique basename so the common `flist.c:2477` form keeps working.
pub fn resolve_upstream_path(cited: &str, manifest: &BTreeMap<String, usize>) -> Option<String> {
    if manifest.contains_key(cited) {
        return Some(cited.to_owned());
    }
    if cited.contains('/') {
        // A directory that the tarball does not have means a different upstream.
        return None;
    }
    let mut hits = manifest.keys().filter(|k| {
        Path::new(k)
            .file_name()
            .is_some_and(|n| n == std::ffi::OsStr::new(cited))
    });
    let first = hits.next()?;
    if hits.next().is_some() {
        return None;
    }
    Some(first.clone())
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
    while i + 2 < bytes.len() {
        let is_ext = (bytes[i] == b'.')
            && (bytes[i + 1] == b'c' || bytes[i + 1] == b'h')
            && bytes[i + 2] == b':';
        if !is_ext {
            i += 1;
            continue;
        }
        let mut start = i;
        while start > 0 {
            let c = bytes[start - 1];
            if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'/' || c == b'.' {
                start -= 1;
            } else {
                break;
            }
        }
        let path = &line[start..i + 2];
        let mut j = i + 3;
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
        i = j.max(i + 3);
    }
    out
}

/// Collects every out-of-range citation in `contents`, and counts how many
/// citations were actually resolved and checked. The count is what lets callers
/// tell "clean" apart from "examined nothing".
pub fn scan_contents(
    source: &str,
    contents: &str,
    manifest: &BTreeMap<String, usize>,
    checked: &mut usize,
) -> Vec<Violation> {
    let mut out = Vec::new();
    for (lineno, line) in contents.lines().enumerate() {
        if !line.to_ascii_lowercase().contains("upstream") {
            continue;
        }
        for (cited_path, nums) in citations_in_line(line) {
            let Some(resolved) = resolve_upstream_path(&cited_path, manifest) else {
                continue;
            };
            *checked += 1;
            let actual = manifest[&resolved];
            for n in nums {
                if n > actual {
                    out.push(Violation {
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

/// What one full scan of the workspace saw.
#[derive(Debug, Default)]
pub struct ScanReport {
    /// Citations naming a line the cited file does not have.
    pub violations: Vec<Violation>,
    /// Rust sources opened.
    pub files_read: usize,
    /// Citations that resolved to a pinned upstream file and were range-checked.
    pub citations_checked: usize,
}

/// Scans every tracked Rust source for out-of-range upstream citations.
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
    for relative in list_rust_sources_via_git(workspace)? {
        let contents = match fs::read_to_string(workspace.join(&relative)) {
            Ok(c) => c,
            Err(_) => continue,
        };
        report.files_read += 1;
        report.violations.extend(scan_contents(
            &relative.display().to_string(),
            &contents,
            &manifest,
            &mut report.citations_checked,
        ));
    }
    if report.files_read == 0 {
        return Err(validation_error(
            "read zero Rust sources; `git ls-files` returned nothing, so a clean \
             result would mean nothing",
        ));
    }
    if report.citations_checked == 0 {
        return Err(validation_error(format!(
            "read {} Rust source(s) but resolved ZERO upstream citations; the \
             citation extractor or {MANIFEST_PATH} is broken",
            report.files_read
        )));
    }
    Ok(report)
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
    let mut rows: Vec<(String, usize)> = Vec::new();
    for entry in walkdir::WalkDir::new(&root)
        .into_iter()
        .filter_map(Result::ok)
    {
        let p = entry.path();
        if !p.is_file() || !p.extension().is_some_and(|e| e == "c" || e == "h") {
            continue;
        }
        let rel = p
            .strip_prefix(&root)
            .map_err(|e| validation_error(e.to_string()))?
            .to_string_lossy()
            .replace('\\', "/");
        rows.push((rel, fs::read_to_string(p)?.lines().count()));
    }
    rows.sort();
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
            "citations: {} checked across {} file(s) - all name a line that \
             exists. This does NOT mean the cited line says what the comment \
             claims; nothing here checks that.",
            report.citations_checked, report.files_read
        );
        return Ok(());
    }
    for v in &report.violations {
        eprintln!("{v}");
    }
    Err(validation_error(format!(
        "{} upstream citation(s) name a line the cited file does not have; \
         re-locate the construct in {PINNED_SOURCE_DIR} and correct the number",
        report.violations.len()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `scan_contents` with the check-counter discarded, for tests that only
    /// care about the violations.
    fn scan(source: &str, contents: &str, m: &BTreeMap<String, usize>) -> Vec<Violation> {
        let mut checked = 0;
        scan_contents(source, contents, m, &mut checked)
    }

    fn manifest() -> BTreeMap<String, usize> {
        parse_manifest("# c\nrsync.c\t831\nflist.c\t3500\nlib/wildmatch.c\t100\n")
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
    fn resolves_bare_name_and_exact_path() {
        let m = manifest();
        assert_eq!(
            resolve_upstream_path("rsync.c", &m).as_deref(),
            Some("rsync.c")
        );
        assert_eq!(
            resolve_upstream_path("wildmatch.c", &m).as_deref(),
            Some("lib/wildmatch.c")
        );
        assert_eq!(
            resolve_upstream_path("lib/wildmatch.c", &m).as_deref(),
            Some("lib/wildmatch.c")
        );
    }

    #[test]
    fn a_foreign_upstream_is_out_of_scope() {
        // zsync citations carry a `librcksum/` prefix the rsync tarball lacks.
        // They must not be gated against rsync's line counts.
        let m = manifest();
        assert_eq!(resolve_upstream_path("librcksum/rsum.c", &m), None);
        assert_eq!(resolve_upstream_path("rsum.c", &m), None);
    }

    #[test]
    fn flags_a_line_past_the_end_of_the_file() {
        let v = scan("a.rs", &cite("rsync.c", "954-965"), &manifest());
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].cited, 954);
        assert_eq!(v[0].actual, 831);
    }

    #[test]
    fn flags_a_range_whose_end_overruns() {
        // The start is inside the file, so only checking the first number
        // would miss this.
        let v = scan("a.rs", &cite("rsync.c", "820-900"), &manifest());
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].cited, 900);
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

    /// THE GATE. Every upstream citation in the workspace must name a line the
    /// cited file actually has. Runs from the committed manifest, so it is not
    /// silently skipped in the cell that lacks the upstream tarball.
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
    fn every_upstream_citation_names_a_line_that_exists() {
        let workspace = crate::workspace::workspace_root().expect("workspace root");
        let report = collect_violations(&workspace).expect("scan succeeds");
        assert!(
            report.violations.is_empty(),
            "upstream citations name lines their files do not have:\n{}",
            report
                .violations
                .iter()
                .map(|v| format!("  {v}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        // A green run must have been earned. `collect_violations` already
        // refuses to return an unearned clean report; this pins the floor so a
        // future change that quietly narrows the scan cannot pass as success.
        assert!(
            report.citations_checked > 100,
            "only {} citations checked across {} file(s) - the scan collapsed, \
             and an empty violation list here would mean nothing",
            report.citations_checked,
            report.files_read
        );
    }

    #[test]
    fn an_empty_manifest_is_an_error_not_a_clean_run() {
        // Every citation would resolve to nothing, so violations would be empty
        // and the gate would pass having checked nothing. That is the failure
        // mode this whole check exists to make impossible.
        let m: BTreeMap<String, usize> = parse_manifest("# only comments\n");
        assert!(m.is_empty());
        let mut checked = 0;
        let v = scan_contents("a.rs", &cite("rsync.c", "954-965"), &m, &mut checked);
        assert!(v.is_empty(), "no manifest means nothing resolves");
        assert_eq!(checked, 0, "and nothing was checked - the tell");
    }

    #[test]
    fn a_resolved_citation_is_counted_as_checked() {
        let mut checked = 0;
        scan_contents(
            "a.rs",
            &cite("rsync.c", "457-465"),
            &manifest(),
            &mut checked,
        );
        assert_eq!(checked, 1);
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
        let mut derived = BTreeMap::new();
        for entry in walkdir::WalkDir::new(&root)
            .into_iter()
            .filter_map(Result::ok)
        {
            let p = entry.path();
            let is_source = p.extension().is_some_and(|e| e == "c" || e == "h");
            if !is_source || !p.is_file() {
                continue;
            }
            let rel = p
                .strip_prefix(&root)
                .expect("under root")
                .to_string_lossy()
                .replace('\\', "/");
            let n = fs::read_to_string(p)
                .map(|s| s.lines().count())
                .unwrap_or_default();
            derived.insert(rel, n);
        }
        assert_eq!(
            manifest, derived,
            "{MANIFEST_PATH} is stale or hand-edited; regenerate it from {PINNED_SOURCE_DIR}"
        );
    }
}
