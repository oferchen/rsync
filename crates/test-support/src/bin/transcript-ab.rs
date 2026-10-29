//! Byte-neutrality A/B gate between two `oc-rsync` builds.
//!
//! ```text
//! transcript-ab --baseline <oc-rsync> --candidate <oc-rsync> [--cell <name>]...
//! ```
//!
//! Captures every network cell (or the named ones) with each build, from one
//! shared backdated fixture, and compares the transcripts direction by
//! direction. Prints one row per cell and exits 1 when any cell differs, so a
//! refactor that must not move a wire byte can be checked against the commit
//! it started from. Cell names: `rsh-push`, `rsh-pull`, `daemon-push`,
//! `daemon-pull`. The `capture-rsh` trampoline is taken from this binary's own
//! directory, where Cargo builds both.

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn main() {
    std::process::exit(ab::run(std::env::args_os().skip(1).collect()));
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn main() {
    eprintln!("transcript-ab: the daemon cells need Linux or macOS");
    std::process::exit(2);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod ab {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use test_support::WireTranscript;
    use test_support::transcript_cells::{CellBinaries, NetworkCell, build_fixture, capture_cell};

    const USAGE: &str =
        "usage: transcript-ab --baseline <oc-rsync> --candidate <oc-rsync> [--cell <name>]...";

    struct Args {
        baseline: PathBuf,
        candidate: PathBuf,
        cells: Vec<NetworkCell>,
    }

    fn parse(args: Vec<OsString>) -> Result<Args, String> {
        let mut baseline = None;
        let mut candidate = None;
        let mut cells = Vec::new();
        let mut it = args.into_iter();
        while let Some(flag) = it.next() {
            let mut value = || {
                it.next()
                    .ok_or_else(|| format!("{} needs a value", flag.to_string_lossy()))
            };
            match flag.to_str() {
                Some("--baseline") => baseline = Some(PathBuf::from(value()?)),
                Some("--candidate") => candidate = Some(PathBuf::from(value()?)),
                Some("--cell") => {
                    let name = value()?;
                    let name = name.to_string_lossy();
                    cells.push(
                        NetworkCell::from_name(&name)
                            .ok_or_else(|| format!("unknown cell {name:?}"))?,
                    );
                }
                _ => return Err(format!("unknown argument {flag:?}")),
            }
        }
        let require = |path: Option<PathBuf>, flag: &str| {
            let path = path.ok_or_else(|| format!("{flag} is required"))?;
            if path.is_file() {
                Ok(path)
            } else {
                Err(format!("{flag} {} is not a file", path.display()))
            }
        };
        Ok(Args {
            baseline: require(baseline, "--baseline")?,
            candidate: require(candidate, "--candidate")?,
            cells: if cells.is_empty() {
                NetworkCell::ALL.to_vec()
            } else {
                cells
            },
        })
    }

    /// The first offset at which `a` and `b` differ, or `None` when equal.
    fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
        (a != b).then(|| {
            a.iter()
                .zip(b)
                .position(|(x, y)| x != y)
                .unwrap_or(a.len().min(b.len()))
        })
    }

    fn verdict(baseline: &[u8], candidate: &[u8]) -> String {
        match first_difference(baseline, candidate) {
            None => format!("same ({} bytes)", baseline.len()),
            Some(at) => format!(
                "DIFFERS at {at} ({} vs {} bytes)",
                baseline.len(),
                candidate.len()
            ),
        }
    }

    fn capture(
        cell: NetworkCell,
        oc_rsync: &Path,
        capture_rsh: &Path,
        src: &Path,
    ) -> Result<WireTranscript, String> {
        let binaries = CellBinaries {
            oc_rsync,
            capture_rsh,
        };
        capture_cell(cell, binaries, src)
            .map_err(|e| format!("{cell} with {}: {e}", oc_rsync.display()))
    }

    pub(super) fn run(raw: Vec<OsString>) -> i32 {
        let args = match parse(raw) {
            Ok(args) => args,
            Err(error) => {
                eprintln!("transcript-ab: {error}\n{USAGE}");
                return 2;
            }
        };
        let capture_rsh = match std::env::current_exe() {
            Ok(exe) => exe.with_file_name("capture-rsh"),
            Err(error) => {
                eprintln!("transcript-ab: cannot locate this binary: {error}");
                return 2;
            }
        };
        if !capture_rsh.is_file() {
            eprintln!("transcript-ab: {} is missing", capture_rsh.display());
            return 2;
        }
        let src = test_support::create_tempdir();
        if let Err(error) = build_fixture(src.path(), false) {
            eprintln!("transcript-ab: fixture: {error}");
            return 2;
        }

        let mut differs = false;
        println!("| cell | client->server | server->client |");
        println!("|---|---|---|");
        for cell in args.cells {
            let pair = capture(cell, &args.baseline, &capture_rsh, src.path()).and_then(|base| {
                capture(cell, &args.candidate, &capture_rsh, src.path()).map(|cand| (base, cand))
            });
            let (base, cand) = match pair {
                Ok(pair) => pair,
                Err(error) => {
                    eprintln!("transcript-ab: {error}");
                    return 2;
                }
            };
            differs |= base != cand;
            println!(
                "| {cell} | {} | {} |",
                verdict(&base.client_to_server, &cand.client_to_server),
                verdict(&base.server_to_client, &cand.server_to_client)
            );
        }
        i32::from(differs)
    }
}
