//! Differential harness: oc's ssh_config resolution against real `ssh -G`.
//!
//! This is the gate the ssh_config parity work is measured by. It exists
//! before the parser changes so that every later claim of "oc now matches
//! upstream" is a reading off this harness rather than an assertion.
//!
//! # Why `ssh -G` is the sharp instrument
//!
//! `config_test` is declared at openssh/ssh.c:677, set at openssh/ssh.c:790-792, and read
//! at exactly one place, openssh/ssh.c:1630-1633 (`dump_client_config(&options,
//! host); exit(0)`). It gates nothing else, so every config-processing
//! step runs identically with and without `-G`: the first pass
//! (openssh/ssh.c:1221), `fill_default_options_for_canonicalization` (:1225), the
//! `HostName` `%h` substitution (:1228-1237), `lowercase(host)` (:1241),
//! `resolve_canonicalize` (:1247), the `SSHCONF_FINAL` re-parse
//! (:1286-1298), `fill_default_options` (:1301), the
//! `ProxyJump`-to-`ProxyCommand` synthesis (:1310-1360), and the percent
//! and tilde expansion (:1432-1628). The dump itself is
//! `dump_client_config`, openssh/readconf.c:3618-3847.
//!
//! # Three sharp edges this harness controls for
//!
//! 1. **`-G` is not side-effect free.** It resolves DNS, and it *executes*
//!    `Match exec` commands - twice when both passes reach them. Every
//!    fixture here is therefore free of `Match exec`, and
//!    [`FIXTURES`] documents that as a requirement rather than an
//!    accident. This matters more once oc mirrors upstream and executes
//!    `Match exec` itself: at that point neither side of the comparison
//!    is a passive read.
//!
//! 2. **`-G` output mixes expanded and unexpanded values,** and which is
//!    which cannot be inferred from the output. `IdentityFile` and
//!    `CertificateFile` are dumped *unexpanded* because their expansion
//!    happens at openssh/ssh.c:2428 and :2476, after the dump. `ControlPath`,
//!    `IdentityAgent`, `UserKnownHostsFile`, `RevokedHostKeys`,
//!    `VersionAddendum`, `SetEnv`, `RemoteCommand`, `User` and the
//!    forward paths are dumped *expanded*. [`Expansion`] encodes the
//!    table so a comparison never silently assumes the wrong one.
//!
//! 3. **`hostname` is not `o->hostname`.** openssh/readconf.c:3639 dumps
//!    `dump_cfg_string(oHostname, host)` - the caller's `host` variable.
//!    When a `HostName` directive matched, openssh/ssh.c:1229-1237 has already
//!    replaced `host` with the `%h`-expanded value; openssh/ssh.c:1239-1241 then
//!    lowercases it unless it is an address literal. So the dumped
//!    `hostname` is the resolved name *lowercased*, and when no
//!    `HostName` matched it is the alias, also lowercased. oc stores the
//!    value as written, so [`Normalization::LowercaseUpstream`] applies.
//!
//! # What oc can currently answer, and what it cannot
//!
//! Measured, not assumed. oc has two independent ssh_config readers and
//! neither produces an upstream-shaped resolved config:
//!
//! - `ssh::config_lookup` has the `Host`/`Match` matching machinery but
//!   resolves exactly one value: a `bool` for `Compression`. That one
//!   value is wired into [`oc_resolution`] as the `compression` row (behind
//!   the `ssh-config-parse` feature that owns the parser), so parser B's
//!   `Host`-pattern behaviour is measured here rather than only by unit
//!   tests asserting our own reading of the C.
//! - `ssh::embedded::ssh_config` (this module's neighbour) is the
//!   value-carrying resolver - [`ResolvedHost`], six directives - but it
//!   understands `Host` only, with no `Match`, no `Include`, and no token
//!   expansion.
//!
//! So a keyword-for-keyword diff against upstream's ~86-line dump is not
//! available today and this harness does not pretend otherwise. Every
//! keyword upstream emits that oc has no resolver for is reported as
//! [`Verdict::NotResolvedByOc`] - a counted, visible outcome. A harness
//! that printed only the six comparable rows would report a parity it has
//! not earned.
//!
//! Adding a resolver later is a one-row edit to [`oc_resolution`].
//!
//! # What the first run already corrected
//!
//! The harness earned its keep before any parser changed. Two beliefs
//! held while writing it were wrong, and the tests now pin the measured
//! answers instead:
//!
//! - oc was expected to lowercase the alias before matching `Host`, and
//!   therefore to diverge from upstream's case-SENSITIVE match. It does
//!   not - both decline `Host WEB1` for alias `web1`.
//! - The divergence that does exist is token expansion: oc stores
//!   `HostName %h.example.com` verbatim where upstream expands it. That
//!   is the non-vacuity fixture.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use super::ssh_config::resolve_host_str;
#[cfg(feature = "ssh-config-parse")]
use crate::ssh::config_lookup::{MatchContext, parse_enables_compression};

/// Whether `ssh -G` prints a keyword's value with tokens already expanded.
///
/// Encoded rather than inferred: the dump interleaves both kinds and the
/// output gives no clue which is which. See the module docs, edge 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Expansion {
    /// Percent tokens and `~` are already resolved in the dumped value.
    Expanded,
    /// The dumped value is the raw config text. `IdentityFile` and
    /// `CertificateFile` are the two that matter: openssh/ssh.c:2428/:2476 expand
    /// them after openssh/ssh.c:1630 has already dumped.
    Unexpanded,
}

/// How a raw value must be adjusted before oc and upstream are comparable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Normalization {
    /// Compare byte-for-byte.
    None,
    /// Upstream lowercases its side (openssh/ssh.c:1239-1241) and oc does not, so
    /// oc's value is lowercased before the comparison. Recorded here so
    /// the adjustment is visible rather than hidden inside a helper.
    LowercaseUpstream,
}

/// The verdict for one keyword.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Verdict {
    /// Both sides resolved a value and they agree.
    Match,
    /// Both sides resolved a value and they differ. This is the finding.
    Mismatch,
    /// oc's parsers have no resolver for this keyword at all. Counted, not
    /// silently dropped - the census of these rows is the epic's scope.
    NotResolvedByOc,
    /// oc has a resolver but this fixture left it unset, so oc would fall
    /// back to a default the harness does not model. Reported, never
    /// asserted on: an unmodelled default is not evidence either way.
    OcUnset,
}

/// One keyword's comparison.
#[derive(Debug, Clone)]
pub(super) struct Cell {
    pub keyword: String,
    pub upstream: Vec<String>,
    pub oc: Option<Vec<String>>,
    pub verdict: Verdict,
    /// Whether upstream's side of this row is expanded. Carried into the
    /// report because it separates a real divergence from a
    /// representation artefact: on an `Unexpanded` row a mismatch may
    /// mean only that oc resolved a token upstream had not reached yet.
    pub expansion: Expansion,
}

/// Why a differential run produced no comparison.
///
/// Never conflated with a pass: a caller that cannot tell "matched" from
/// "did not run" is the skip-as-pass vacuity this repo has hit repeatedly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Skipped {
    /// No `ssh` on PATH.
    NoSshBinary,
    /// `ssh -G` ran but failed; carries its stderr.
    SshFailed(String),
}

/// A completed differential run.
#[derive(Debug)]
pub(super) struct Differential {
    /// The `ssh` that produced the oracle. Recorded because the pinned
    /// reference tarball (10.0p1) fixes the *citations*, while this binary
    /// is whatever the host installed - the two can diverge in behaviour
    /// and an unattributed mismatch would be a mystery.
    pub oracle_version: String,
    pub cells: Vec<Cell>,
}

impl Differential {
    pub fn mismatches(&self) -> Vec<&Cell> {
        self.cells
            .iter()
            .filter(|c| c.verdict == Verdict::Mismatch)
            .collect()
    }

    pub fn cell(&self, keyword: &str) -> Option<&Cell> {
        self.cells.iter().find(|c| c.keyword == keyword)
    }

    pub fn count(&self, verdict: &Verdict) -> usize {
        self.cells.iter().filter(|c| &c.verdict == verdict).count()
    }
}

/// Run `ssh -G <alias> -F <path>` and parse the dump.
///
/// The output shape is one `keyword value` line per option, keyword
/// lowercased, value the remainder of the line. A keyword may repeat -
/// `identityfile` does in every default config - so values collect into a
/// Vec in emission order.
fn upstream_dump(
    config: &Path,
    alias: &str,
) -> Result<(String, BTreeMap<String, Vec<String>>), Skipped> {
    let version = Command::new("ssh")
        .arg("-V")
        .output()
        .map_err(|_| Skipped::NoSshBinary)?;
    // `ssh -V` prints to stderr.
    let version = String::from_utf8_lossy(&version.stderr).trim().to_owned();

    let out = Command::new("ssh")
        .arg("-G")
        .arg("-F")
        .arg(config)
        .arg(alias)
        .output()
        .map_err(|_| Skipped::NoSshBinary)?;
    if !out.status.success() {
        return Err(Skipped::SshFailed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }

    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((keyword, value)) = line.split_once(' ') else {
            // A valueless line is upstream emitting an empty option; keep
            // the keyword so the census still counts it.
            map.entry(line.to_owned()).or_default();
            continue;
        };
        map.entry(keyword.to_owned())
            .or_default()
            .push(value.to_owned());
    }
    Ok((version, map))
}

/// Everything oc resolves today, keyed by the `ssh -G` keyword name.
///
/// `None` means "oc has a resolver but this fixture left it unset".
/// A keyword absent from the returned map means oc has no resolver at all.
fn oc_resolution(config_text: &str, alias: &str) -> BTreeMap<String, Option<Vec<String>>> {
    let mut map: BTreeMap<String, Option<Vec<String>>> = BTreeMap::new();

    // Parser B's one resolved value. Inserted BEFORE the parser-A rows so
    // it still contributes when parser A refuses the file - the two
    // readers have deliberately different failure policies (see
    // `config_lookup`'s module docs) and the harness must show both.
    //
    // Its `false` covers both "Compression no" and "no directive at all",
    // and upstream's default is also `no` (openssh/readconf.c dumps
    // `compression no`), so the boolean is comparable end to end. Without
    // the feature the row is absent and upstream's `compression` line
    // lands in `NotResolvedByOc`, which is the truth for that build
    // rather than a hidden pass.
    #[cfg(feature = "ssh-config-parse")]
    {
        let ctx = MatchContext::new(alias, alias, "", "");
        let enabled = parse_enables_compression(config_text, &ctx);
        map.insert(
            "compression".to_owned(),
            Some(vec![if enabled { "yes" } else { "no" }.to_owned()]),
        );
    }

    // A config parser A REFUSES leaves every one of its rows ABSENT, so
    // upstream's dumped values land in `NotResolvedByOc` instead of
    // panicking the harness. The refusal itself is asserted through
    // [`upstream_refusal`], which is the only instrument that can see it:
    // `ssh -G` reports these rules as a nonzero exit plus stderr text,
    // never as a dumped option.
    let Ok(resolved) = resolve_host_str(config_text, alias) else {
        return map;
    };

    map.insert(
        "hostname".to_owned(),
        // Upstream always dumps a hostname: the resolved HostName if one
        // matched, else the alias (openssh/ssh.c:1229-1241). oc's `None` means no
        // HostName matched, which is the same fallback.
        Some(vec![
            resolved
                .hostname
                .clone()
                .unwrap_or_else(|| alias.to_owned()),
        ]),
    );
    map.insert(
        "port".to_owned(),
        resolved.port.map(|p| vec![p.to_string()]),
    );
    map.insert("user".to_owned(), resolved.user.clone().map(|u| vec![u]));
    map.insert(
        "identityfile".to_owned(),
        if resolved.identity_files.is_empty() {
            None
        } else {
            Some(
                resolved
                    .identity_files
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect(),
            )
        },
    );
    map.insert(
        "identitiesonly".to_owned(),
        resolved
            .identities_only
            .map(|v| vec![if v { "yes" } else { "no" }.to_owned()]),
    );
    map.insert(
        "identityagent".to_owned(),
        resolved.identity_agent.clone().map(|a| vec![a]),
    );

    map
}

/// Whether `ssh -G` REFUSED this fixture, and with what diagnostic.
///
/// `Ok(None)` means the oracle accepted it; `Ok(Some(text))` carries the
/// first stderr line of a refusal.
///
/// This exists because [`differential`] can only compare a SUCCESSFUL
/// dump, and two of upstream's tokeniser rules are observable *only* as a
/// refusal: the empty-token guard (openssh/readconf.c:1832-1836) and the
/// unterminated-quote guard (openssh/readconf.c:1196-1199) both abort the
/// load and print to stderr rather than dumping a different option value.
/// A harness that could only read accepted dumps would be structurally
/// blind to exactly that half.
///
/// # Errors
///
/// [`Skipped::NoSshBinary`] when no `ssh` is on PATH.
pub(super) fn upstream_refusal(config: &Path, alias: &str) -> Result<Option<String>, Skipped> {
    let out = Command::new("ssh")
        .arg("-G")
        .arg("-F")
        .arg(config)
        .arg(alias)
        .output()
        .map_err(|_| Skipped::NoSshBinary)?;
    if out.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned(),
    ))
}

/// The per-keyword comparison rules, for the keywords oc can answer.
fn rules(keyword: &str) -> (Expansion, Normalization) {
    match keyword {
        // openssh/ssh.c:1239-1241 lowercases the resolved host; oc stores it as
        // written.
        "hostname" => (Expansion::Expanded, Normalization::LowercaseUpstream),
        // Dumped before openssh/ssh.c:2428 expands it.
        "identityfile" => (Expansion::Unexpanded, Normalization::None),
        // Dumped after expansion (openssh/ssh.c's IdentityAgent handling).
        "identityagent" => (Expansion::Expanded, Normalization::None),
        _ => (Expansion::Expanded, Normalization::None),
    }
}

fn normalize(values: &[String], how: Normalization) -> Vec<String> {
    match how {
        Normalization::None => values.to_vec(),
        Normalization::LowercaseUpstream => values.iter().map(|v| v.to_lowercase()).collect(),
    }
}

/// Compare oc's resolution of `alias` against `ssh -G`'s, for one fixture.
pub(super) fn differential(
    config: &Path,
    config_text: &str,
    alias: &str,
) -> Result<Differential, Skipped> {
    let (oracle_version, upstream) = upstream_dump(config, alias)?;
    let oc = oc_resolution(config_text, alias);

    let mut cells = Vec::new();
    for (keyword, up_values) in &upstream {
        let (expansion, how) = rules(keyword);
        let (oc_values, verdict) = match oc.get(keyword) {
            None => (None, Verdict::NotResolvedByOc),
            Some(None) => (None, Verdict::OcUnset),
            Some(Some(values)) => {
                let verdict = if normalize(values, how) == normalize(up_values, Normalization::None)
                {
                    Verdict::Match
                } else {
                    Verdict::Mismatch
                };
                (Some(values.clone()), verdict)
            }
        };
        cells.push(Cell {
            keyword: keyword.clone(),
            upstream: up_values.clone(),
            oc: oc_values,
            verdict,
            expansion,
        });
    }

    Ok(Differential {
        oracle_version,
        cells,
    })
}

/// Realistic fixtures the later parity rows share.
///
/// ⚠ None of these may contain `Match exec`: `ssh -G` executes it, and
/// once oc mirrors upstream so will oc. See the module docs, edge 1.
pub(super) const FIXTURES: &[(&str, &str)] = &[
    (
        "wildcard_host",
        "Host web*.example.com\n  User deploy\n  Port 2222\n\nHost *\n  User fallback\n",
    ),
    (
        "identity_file_list",
        "Host multi\n  IdentityFile ~/.ssh/id_a\n  IdentityFile ~/.ssh/id_b\n  IdentitiesOnly yes\n",
    ),
    (
        "bastion_proxyjump",
        "Host inner\n  HostName 10.0.0.5\n  ProxyJump bastion.example.com\n\nHost bastion.example.com\n  User jump\n",
    ),
    (
        "match_host",
        "Match host inner.example.com\n  User matched\n  Port 2022\n\nHost *\n  User plain\n",
    ),
    (
        "negated_host",
        "Host !banned.example.com *.example.com\n  User allowed\n  Port 2200\n",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Materialise a fixture and run the differential.
    ///
    /// Returns `Err(Skipped)` rather than passing when `ssh` is missing,
    /// so a host without the oracle produces a reason, never a false green.
    fn run(text: &str, alias: &str) -> Result<Differential, Skipped> {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ssh_config");
        let mut f = std::fs::File::create(&path).expect("create fixture");
        f.write_all(text.as_bytes()).expect("write fixture");
        drop(f);
        differential(&path, text, alias)
    }

    /// Environment switch that converts a skip into a failure.
    ///
    /// Set it wherever `ssh` is known to be installed. Without it a host
    /// with no `ssh` silently exercises nothing, and every test in this
    /// module reports `ok` - the skip-conflated-with-pass shape this
    /// repo has hit repeatedly. Printing the reason makes the skip
    /// visible; this makes it *decidable*.
    const REQUIRE_ORACLE: &str = "OC_RSYNC_REQUIRE_SSH_G";

    /// Report a skip loudly, or fail outright when the oracle was required.
    fn report_skip(what: &str, why: &Skipped) {
        if std::env::var_os(REQUIRE_ORACLE).is_some() {
            panic!(
                "{REQUIRE_ORACLE} is set but the ssh -G oracle was unusable for {what}: {why:?}"
            );
        }
        eprintln!("ssh -G differential SKIPPED for {what}: {why:?}");
    }

    /// NON-VACUITY: the harness must REPORT a divergence oc has today.
    ///
    /// oc's embedded resolver performs no token expansion, so a `HostName`
    /// carrying `%h` is stored as written. Upstream expands it during the
    /// first pass (openssh/ssh.c:1228-1237, `%h` -> the alias) and the expanded
    /// value is what reaches the dump.
    ///
    /// Measured against real ssh before this test was written: alias `t`
    /// against `HostName %h.example.com` gives upstream `t.example.com`
    /// and oc `%h.example.com`.
    ///
    /// If this test ever passes with zero mismatches, the harness has gone
    /// blind - that is the failure it exists to prevent.
    #[test]
    fn harness_reports_the_missing_token_expansion() {
        const FIXTURE: &str = "Host t\n  HostName %h.example.com\n";

        let diff = match run(FIXTURE, "t") {
            Ok(d) => d,
            Err(why) => {
                report_skip("token-expansion non-vacuity", &why);
                return;
            }
        };

        let hostname = diff
            .cell("hostname")
            .expect("upstream always dumps hostname");
        assert_eq!(
            hostname.verdict,
            Verdict::Mismatch,
            "harness went blind: upstream {:?} vs oc {:?} on {}",
            hostname.upstream,
            hostname.oc,
            diff.oracle_version
        );
        assert_eq!(hostname.upstream, vec!["t.example.com".to_owned()]);
        assert_eq!(hostname.oc, Some(vec!["%h.example.com".to_owned()]));

        // The divergence must reach the REPORT, not just the cell: a
        // harness that computed the right verdict and then failed to
        // surface it would be just as blind.
        assert!(
            diff.mismatches().iter().any(|c| c.keyword == "hostname"),
            "hostname mismatch computed but not reported"
        );
    }

    /// The control for the cell above: with no token to expand, both
    /// sides agree. Without this, a harness that reported Mismatch
    /// unconditionally would also satisfy the test above.
    #[test]
    fn a_literal_hostname_matches_on_both_sides() {
        const FIXTURE: &str = "Host t\n  HostName real.example.com\n  Port 2222\n  User alice\n";

        let diff = match run(FIXTURE, "t") {
            Ok(d) => d,
            Err(why) => {
                report_skip("literal-hostname control", &why);
                return;
            }
        };

        for keyword in ["port", "user", "hostname"] {
            let cell = diff.cell(keyword).expect("dumped");
            assert_eq!(
                cell.verdict,
                Verdict::Match,
                "{keyword}: upstream {:?} vs oc {:?}",
                cell.upstream,
                cell.oc
            );
        }
    }

    /// oc's `Host` matching is case-SENSITIVE, matching upstream.
    ///
    /// This was measured, not assumed, and it corrected a belief held
    /// before the harness existed: oc's resolver was expected to
    /// lowercase the alias and therefore to match `Host WEB1` for alias
    /// `web1`. It does not - both sides decline, and both fall back to
    /// their defaults. Pinned here so the harness records the agreement
    /// rather than leaving a refuted guess in the notes.
    #[test]
    fn host_pattern_matching_is_case_sensitive_on_both_sides() {
        const FIXTURE: &str = "Host WEB1\n  Port 2222\n  User alice\n  HostName real.example.com\n";

        let mismatched = match run(FIXTURE, "web1") {
            Ok(d) => d,
            Err(why) => {
                report_skip("case-sensitivity", &why);
                return;
            }
        };
        // Neither side matched, so neither resolved a port.
        assert_eq!(
            mismatched.cell("port").expect("dumped").upstream,
            vec!["22"]
        );
        assert_eq!(
            mismatched.cell("port").expect("dumped").verdict,
            Verdict::OcUnset
        );

        let exact = match run(FIXTURE, "WEB1") {
            Ok(d) => d,
            Err(why) => {
                report_skip("case-sensitivity control", &why);
                return;
            }
        };
        for keyword in ["port", "user", "hostname"] {
            let cell = exact.cell(keyword).expect("dumped");
            assert_eq!(
                cell.verdict,
                Verdict::Match,
                "{keyword}: upstream {:?} vs oc {:?}",
                cell.upstream,
                cell.oc
            );
        }
    }

    /// A COMMA is not a `Host` separator, measured on both parsers at once.
    ///
    /// Upstream tokenises the `Host` line with `argv_split`
    /// (openssh/misc.c:2130-2185), whose only separators are `' '` and
    /// `'\t'` (openssh/misc.c:2141), then matches each token individually
    /// with `match_pattern` in the `argv_next` loop
    /// (openssh/readconf.c:1831-1858, the call at :1844). The `oHost` arm
    /// never reaches `match_pattern_list`, the function that does cut at a
    /// comma (openssh/match.c:143), so `a,b` is one pattern matching only
    /// the literal string `a,b`.
    ///
    /// Measured before the fix on `OpenSSH_10.3p1`: upstream reports
    /// `port 22` and `compression no` for alias `a`, while oc applied the
    /// block - port 2222 from the value-carrying parser and compression
    /// yes from the `config_lookup` parser. One fixture, both parsers: the
    /// `port`/`user` rows are `embedded::ssh_config`, the `compression`
    /// row is `config_lookup`.
    ///
    /// ⚠ The mirror image - alias `a,b`, which upstream DOES match - cannot
    /// be measured here: `ssh -G a,b` refuses with "hostname contains
    /// invalid characters" and exits nonzero, so the oracle has no verdict
    /// to give. That half is pinned by unit tests beside each parser.
    #[test]
    fn a_comma_in_a_host_line_is_not_a_separator() {
        const FIXTURE: &str = "Host a,b\n  Port 2222\n  User alice\n  Compression yes\n";

        let diff = match run(FIXTURE, "a") {
            Ok(d) => d,
            Err(why) => {
                report_skip("host comma separator", &why);
                return;
            }
        };

        // Upstream declined the block, so its port is the built-in
        // default. The harness models no defaults, so oc's side must be
        // unresolved - `OcUnset` is the assertion, and a pre-fix oc would
        // land on `Mismatch` with 2222.
        let port = diff.cell("port").expect("dumped");
        assert_eq!(port.upstream, vec!["22"]);
        assert_eq!(
            port.verdict,
            Verdict::OcUnset,
            "oc applied a comma-separated Host block: {:?} on {}",
            port.oc,
            diff.oracle_version
        );
        // `user` is dumped as the local account when no directive matched,
        // so the value is host-dependent; the verdict is not.
        assert_eq!(diff.cell("user").expect("dumped").verdict, Verdict::OcUnset);

        #[cfg(feature = "ssh-config-parse")]
        {
            let cell = diff.cell("compression").expect("dumped");
            assert_eq!(cell.upstream, vec!["no".to_owned()]);
            assert_eq!(
                cell.verdict,
                Verdict::Match,
                "compression parser applied a comma-separated Host block: oc {:?} on {}",
                cell.oc,
                diff.oracle_version
            );
        }
    }

    /// A `Host` BLOCK pattern is matched byte-exactly, in the compression
    /// parser too.
    ///
    /// `oHost` calls `match_pattern` directly (openssh/readconf.c:1844) and
    /// `match_pattern` folds nothing:
    /// `if (*pattern != '?' && *pattern != *s) return 0;`
    /// (openssh/match.c:105-106). `Match host` is the case-INSENSITIVE one,
    /// because it routes through `match_hostname`
    /// (openssh/match.c:193-203), which lowercases the host and passes
    /// `dolower=1` - a different keyword with a different rule.
    ///
    /// Measured before the fix on `OpenSSH_10.3p1`: alias `web1` against
    /// `Host WEB1` gives upstream `compression no`, oc `yes`. The
    /// value-carrying parser already agreed with upstream here - see
    /// `host_pattern_matching_is_case_sensitive_on_both_sides` - so this
    /// row exists to cover the second parser.
    #[cfg(feature = "ssh-config-parse")]
    #[test]
    fn host_block_pattern_case_is_significant_for_compression() {
        const FIXTURE: &str = "Host WEB1\n  Compression yes\n  Port 2222\n";

        let mismatched_case = match run(FIXTURE, "web1") {
            Ok(d) => d,
            Err(why) => {
                report_skip("compression case-sensitivity", &why);
                return;
            }
        };
        let cell = mismatched_case.cell("compression").expect("dumped");
        assert_eq!(cell.upstream, vec!["no".to_owned()]);
        assert_eq!(
            cell.verdict,
            Verdict::Match,
            "compression parser case-folded a Host pattern: oc {:?} on {}",
            cell.oc,
            mismatched_case.oracle_version
        );

        // Control: the exact-case alias must still fire. Without it, a
        // parser that answered `no` unconditionally would pass above.
        let exact_case = match run(FIXTURE, "WEB1") {
            Ok(d) => d,
            Err(why) => {
                report_skip("compression case-sensitivity control", &why);
                return;
            }
        };
        let cell = exact_case.cell("compression").expect("dumped");
        assert_eq!(cell.upstream, vec!["yes".to_owned()]);
        assert_eq!(cell.verdict, Verdict::Match, "oc {:?}", cell.oc);
    }

    /// SHARP EDGE 2, demonstrated live rather than asserted from the C.
    ///
    /// `IdentityFile` is dumped UNEXPANDED because openssh/ssh.c:2428 expands it
    /// only after openssh/ssh.c:1630 has already dumped and exited. oc expands
    /// `~` at resolve time, so the two sides disagree in REPRESENTATION
    /// while naming the same file.
    ///
    /// The harness reports this as a mismatch on purpose. Papering over
    /// it - by expanding upstream's side to compare - would destroy the
    /// evidence that the expansion table is load-bearing, and would hide
    /// a genuine divergence the day oc's expansion stops agreeing.
    #[test]
    fn identityfile_is_dumped_unexpanded_by_upstream() {
        const FIXTURE: &str = "Host t\n  IdentityFile ~/.ssh/id_probe\n";

        let diff = match run(FIXTURE, "t") {
            Ok(d) => d,
            Err(why) => {
                report_skip("identityfile expansion", &why);
                return;
            }
        };

        let cell = diff.cell("identityfile").expect("dumped");
        assert_eq!(cell.expansion, Expansion::Unexpanded);
        assert_eq!(cell.upstream, vec!["~/.ssh/id_probe".to_owned()]);
        assert_eq!(cell.verdict, Verdict::Mismatch);
        let oc = cell.oc.clone().expect("oc resolved an identity file");
        assert!(
            !oc[0].starts_with('~'),
            "expected oc to expand the tilde, got {oc:?}"
        );
        // Compare components, not bytes: the expansion joins the platform home
        // with the fixture's literal path, so the separator before `.ssh` is a
        // backslash on Windows. `Path::ends_with` accepts either separator.
        assert!(
            Path::new(&oc[0]).ends_with(Path::new(".ssh/id_probe")),
            "expected the expansion to still name .ssh/id_probe, got {oc:?}"
        );
    }

    /// The census is the point: upstream dumps far more than oc resolves,
    /// and every one of those keywords must land in a counted bucket
    /// rather than vanishing. A harness that silently dropped them would
    /// report parity over six rows and call it a config parser.
    #[test]
    fn every_upstream_keyword_lands_in_a_counted_bucket() {
        let diff = match run(FIXTURES[0].1, "web1.example.com") {
            Ok(d) => d,
            Err(why) => {
                report_skip("keyword census", &why);
                return;
            }
        };

        let counted = diff.count(&Verdict::Match)
            + diff.count(&Verdict::Mismatch)
            + diff.count(&Verdict::NotResolvedByOc)
            + diff.count(&Verdict::OcUnset);
        assert_eq!(counted, diff.cells.len(), "a keyword escaped the census");

        // oc resolves a small minority. Asserting the shape, not a
        // brittle exact count: upstream's dump grows between releases.
        assert!(
            diff.count(&Verdict::NotResolvedByOc) > diff.cells.len() / 2,
            "expected most keywords unresolved by oc today, got {} of {}",
            diff.count(&Verdict::NotResolvedByOc),
            diff.cells.len()
        );
    }

    /// Every fixture must be runnable and free of `Match exec`, which
    /// `ssh -G` would execute. This is a property of the corpus, checkable
    /// without the oracle, so it never skips.
    #[test]
    fn no_fixture_contains_match_exec() {
        for (name, text) in FIXTURES {
            assert!(
                !text.to_lowercase().contains("match exec"),
                "fixture {name} contains Match exec, which ssh -G executes"
            );
        }
    }

    /// Each fixture resolves without panicking, and the corpus reports a
    /// COUNT of what actually ran.
    ///
    /// The count is the point: "5 produced, 0 skipped" and "0 produced, 5
    /// skipped" are both green to a test runner, and only the printed
    /// tally distinguishes them. `OC_RSYNC_REQUIRE_SSH_G` turns the second
    /// into a failure where the oracle is meant to exist.
    #[test]
    fn every_fixture_produces_a_differential() {
        let mut produced = 0usize;
        let mut skipped = 0usize;
        for (name, text) in FIXTURES {
            let alias = match *name {
                "wildcard_host" => "web1.example.com",
                "identity_file_list" => "multi",
                "bastion_proxyjump" => "inner",
                "match_host" => "inner.example.com",
                "negated_host" => "ok.example.com",
                other => panic!("fixture {other} has no alias"),
            };
            match run(text, alias) {
                Ok(diff) => {
                    assert!(!diff.cells.is_empty(), "{name}: empty dump");
                    produced += 1;
                }
                Err(why) => {
                    report_skip(name, &why);
                    skipped += 1;
                }
            }
        }
        eprintln!(
            "ssh -G differential corpus: {produced} produced, {skipped} skipped, {} total",
            FIXTURES.len()
        );
        assert_eq!(produced + skipped, FIXTURES.len(), "a fixture went missing");
    }

    /// Materialise a fixture and ask `ssh -G` whether it REFUSES the file.
    fn refusal(text: &str, alias: &str) -> Result<Option<String>, Skipped> {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ssh_config");
        let mut f = std::fs::File::create(&path).expect("create fixture");
        f.write_all(text.as_bytes()).expect("write fixture");
        drop(f);
        upstream_refusal(&path, alias)
    }

    /// oc's own verdict on the same text, as the reason line or `None`.
    fn oc_refusal(text: &str, alias: &str) -> Option<String> {
        resolve_host_str(text, alias)
            .err()
            .map(|err| err.to_string())
    }

    /// An empty `Host` token is a HARD ERROR on both sides.
    ///
    /// `argv_split` PRODUCES the empty token (a quote pair is a state
    /// toggle that consumes its delimiters, openssh/misc.c:2168-2172), and
    /// the `oHost` arm is what refuses it - openssh/readconf.c:1832-1836
    /// `fatal("%s line %d: keyword %s empty argument", ...)`. Splitting the
    /// rule that way is deliberate: the `Match` criteria consume the same
    /// tokeniser and have no such guard.
    ///
    /// The companion below is not decoration - it pins the ORDER, which is
    /// the part a plausible-looking implementation gets wrong.
    #[test]
    fn an_empty_host_token_is_refused_by_both() {
        const FIXTURE: &str = "Host a \"\"\n  Port 2222\n";

        let upstream = match refusal(FIXTURE, "a") {
            Ok(v) => v,
            Err(why) => {
                report_skip("empty-host-token refusal", &why);
                return;
            }
        };
        let upstream = upstream.expect("ssh -G must refuse an empty Host token");
        assert!(
            upstream.contains("keyword host empty argument"),
            "oracle refused with unexpected text: {upstream}"
        );

        let oc = oc_refusal(FIXTURE, "a").expect("oc must refuse it too");
        assert!(
            oc.contains("keyword host empty argument"),
            "oc refused with unexpected text: {oc}"
        );
    }

    /// ORDER CELL: a negated token that MATCHES breaks out of the loop
    /// before the empty token is ever examined, so the same line is
    /// ACCEPTED for that alias and refused for every other.
    ///
    /// Measured against real `ssh -G` before this test was written:
    /// `Host !a ""` gives `port 22` for alias `a` (block inactive, no
    /// refusal) and exits 255 for alias `z`. openssh/readconf.c:1839-1855 -
    /// the negation `break` at :1849 precedes nothing, but the empty check
    /// at :1832 sits INSIDE the same per-token loop, so a break skips it.
    ///
    /// Without this cell an implementation that validated every token up
    /// front would look correct: it passes the refusal test above and
    /// diverges only here.
    #[test]
    fn a_negated_match_breaks_before_the_empty_token_is_seen() {
        const FIXTURE: &str = "Host !a \"\"\n  Port 2222\n";

        let matched = match refusal(FIXTURE, "a") {
            Ok(v) => v,
            Err(why) => {
                report_skip("negated-match ordering", &why);
                return;
            }
        };
        assert_eq!(
            matched, None,
            "the oracle must ACCEPT the line whose negated token already matched"
        );
        assert_eq!(
            oc_refusal(FIXTURE, "a"),
            None,
            "oc must accept it too - the empty-token guard is inside the match loop"
        );

        // Non-vacuity: the SAME line refuses for an alias the negation
        // does not match, so the acceptance above is the ordering and not
        // a missing guard.
        let unmatched = refusal(FIXTURE, "z")
            .expect("oracle available")
            .expect("ssh -G must refuse when the walk reaches the empty token");
        assert!(
            unmatched.contains("keyword host empty argument"),
            "oracle refused with unexpected text: {unmatched}"
        );
        assert!(
            oc_refusal(FIXTURE, "z")
                .expect("oc must refuse it too")
                .contains("keyword host empty argument"),
        );
    }

    /// An unterminated quote is a HARD ERROR on both sides.
    ///
    /// `argv_split` returns NULL when it runs off the end of the line with
    /// a quote still open (openssh/misc.c:2174-2181), and the caller turns
    /// that into `fatal("%s line %d: invalid quotes", ...)`
    /// (openssh/readconf.c:1196-1199). It is a property of the TOKENISER,
    /// so it fires on any keyword - the companion uses a value line, not a
    /// `Host` line, to show the guard is not the `oHost` arm's.
    #[test]
    fn an_unterminated_quote_is_refused_by_both() {
        const FIXTURE: &str = "Host a\n  HostName \"unclosed\n";

        let upstream = match refusal(FIXTURE, "a") {
            Ok(v) => v,
            Err(why) => {
                report_skip("unterminated-quote refusal", &why);
                return;
            }
        };
        let upstream = upstream.expect("ssh -G must refuse an unterminated quote");
        assert!(
            upstream.contains("invalid quotes"),
            "oracle refused with unexpected text: {upstream}"
        );
        assert!(
            oc_refusal(FIXTURE, "a")
                .expect("oc must refuse it too")
                .contains("invalid quotes"),
        );

        // Non-vacuity: CLOSING the quote makes the same line load.
        const CLOSED: &str = "Host a\n  HostName \"closed\"\n";
        assert_eq!(
            refusal(CLOSED, "a").expect("oracle available"),
            None,
            "the oracle must accept the closed-quote control"
        );
        assert_eq!(oc_refusal(CLOSED, "a"), None, "oc must accept it too");
    }

    /// A `#` inside a token is ORDINARY TEXT, not a comment marker.
    ///
    /// `argv_split` only ends the line on a `#` that starts a token
    /// (openssh/misc.c:2143-2144, tested at the top of the outer loop
    /// after whitespace is skipped). oc's readers used to cut the line at
    /// the first `#` anywhere, which truncated this hostname.
    #[test]
    fn a_hash_mid_token_is_hostname_text_on_both_sides() {
        const FIXTURE: &str = "Host t\n  HostName x#y\n";

        let diff = match run(FIXTURE, "t") {
            Ok(d) => d,
            Err(why) => {
                report_skip("mid-token hash", &why);
                return;
            }
        };
        let hostname = diff.cell("hostname").expect("upstream dumps hostname");
        assert_eq!(
            hostname.verdict,
            Verdict::Match,
            "upstream {:?} vs oc {:?} on {}",
            hostname.upstream,
            hostname.oc,
            diff.oracle_version
        );
        assert_eq!(hostname.upstream, vec!["x#y".to_owned()]);

        // Non-vacuity: a `#` that STARTS a token still ends the line, so
        // the value is the one token before it and the default survives
        // for the rest.
        const COMMENTED: &str = "Host t\n  HostName real # x#y\n";
        let diff = run(COMMENTED, "t").expect("oracle available");
        let hostname = diff.cell("hostname").expect("upstream dumps hostname");
        assert_eq!(hostname.upstream, vec!["real".to_owned()]);
        assert_eq!(hostname.verdict, Verdict::Match);
    }

    /// Quotes GROUP and are CONSUMED, so a quoted `Host` pattern matches
    /// the bare alias.
    ///
    /// openssh/misc.c:2163-2166 toggles the quote state without copying the
    /// delimiter, so `Host "a"` yields the one-character pattern `a`, and
    /// the `oHost` arm matches it against the alias with `match_pattern`
    /// (openssh/readconf.c:1844). A reader that kept the quote bytes would
    /// fail to match and silently fall through to the defaults.
    #[test]
    fn a_quoted_host_pattern_matches_the_bare_alias_on_both_sides() {
        const FIXTURE: &str = "Host \"a\"\n  HostName inside.example.com\n";

        let diff = match run(FIXTURE, "a") {
            Ok(d) => d,
            Err(why) => {
                report_skip("quoted host pattern", &why);
                return;
            }
        };
        let hostname = diff.cell("hostname").expect("upstream dumps hostname");
        assert_eq!(
            hostname.verdict,
            Verdict::Match,
            "upstream {:?} vs oc {:?} on {}",
            hostname.upstream,
            hostname.oc,
            diff.oracle_version
        );
        assert_eq!(hostname.upstream, vec!["inside.example.com".to_owned()]);

        // Non-vacuity: the quotes are not merely tolerated, they are
        // CONSUMED - a quote opened mid-token splices, so `Host "a"b`
        // matches the alias `ab` and NOT `a`.
        const SPLICED: &str = "Host \"a\"b\n  HostName spliced.example.com\n";
        let diff = run(SPLICED, "ab").expect("oracle available");
        let hostname = diff.cell("hostname").expect("upstream dumps hostname");
        assert_eq!(hostname.upstream, vec!["spliced.example.com".to_owned()]);
        assert_eq!(hostname.verdict, Verdict::Match);
    }
}
