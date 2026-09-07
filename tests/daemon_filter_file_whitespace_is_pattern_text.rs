//! A daemon `exclude from` / `include from` rule's whitespace is pattern text.
//!
//! upstream builds `daemon_filter_list` in `clientserver.c:rsync_module()`, and
//! the two FILE-valued parameters go through `parse_filter_file()`:
//!
//! ```c
//! /* clientserver.c:937-948 */
//! p = lp_include_from(module_id);
//! parse_filter_file(&daemon_filter_list, p, rule_template(FILTRULE_INCLUDE), ...);
//! p = lp_exclude_from(module_id);
//! parse_filter_file(&daemon_filter_list, p, rule_template(0), ...);
//! ```
//!
//! Neither template carries `FILTRULE_WORD_SPLIT`, so both are line-parsed and
//! the reader's two decisions are upstream's:
//!
//! * where a record ends (`exclude.c:1774-1793`): `\n`, a lone `\r`, or `\r\n`
//!   as ONE terminator; and
//! * whether it carries a rule (`exclude.c:1806`): `if (*line && (word_split ||
//!   (*line != ';' && *line != '#')))` - the FIRST BYTE, with no trimming.
//!
//! `parse_rule_tok` then takes `len = strlen((char*)s)` (`exclude.c:1465`), so
//! trailing whitespace is pattern text.
//!
//! oc's daemon reader was the one that never got the fix that `crates/filters`,
//! the engine dir-merge loader and the CLI `--*clude-from` reader all received:
//! it split with `str::lines` and then `.map(str::trim)`, testing for a blank
//! and a comment AFTER the trim. All four consequences change WHICH FILES THE
//! DAEMON SERVES, at exit 0, with no diagnostic.
//!
//! MEASURED against a real rsync 3.5.0 daemon on Linux, module holding `a` and
//! `a ` (trailing space), `exclude from` file holding the single line `a `:
//! upstream serves `a` and hides `a `; oc at f054c633b served `a ` and hid `a`.
//! Every row below is asserted against that measured upstream behaviour.
//!
//! The assertion is the SET OF FILES THE CLIENT RECEIVES, not a parsed pattern
//! string, because that set is the property that matters.
//!
//! Skip conditions (the test passes with a printed reason):
//! - Loopback TCP is unavailable.
//! - The cross-implementation cell additionally needs a built upstream 3.5.0
//!   binary; without it it reports why it did not run.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn upstream_binary() -> Option<PathBuf> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target/interop/upstream-src/rsync-3.5.0/rsync");
    path.is_file().then_some(path)
}

fn free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port())
}

/// The module's contents. Every name is chosen so that ONE of the rules below
/// selects it and the others do not, which is what makes each row
/// discriminating rather than merely green.
const PLANTED: &[&str] = &[
    "a",     // the plain name
    "a ",    // the same name with a trailing space
    " #a",   // an INDENTED comment marker: a pattern, not a comment
    "   ",   // a whitespace-only name: a pattern, not a blank line
    "x.tmp", // reached only through the second record of the `\r` row
    "keep",  // the control: no row here may hide it
];

/// One row of the reader's truth table.
struct Case {
    /// The `exclude from` file's exact bytes.
    body: &'static str,
    /// The names the daemon must still serve.
    served: &'static [&'static str],
    /// Why this row is in the table.
    why: &'static str,
}

const CASES: &[Case] = &[
    Case {
        body: "a \n",
        served: &["a", " #a", "   ", "x.tmp", "keep"],
        why: "the trailing space is pattern text (exclude.c:1465, strlen), so \
              the rule names `a ` and not `a`; trimming inverted the pair",
    },
    Case {
        body: " #a\n",
        served: &["a", "a ", "   ", "x.tmp", "keep"],
        why: "exclude.c:1806 tests the FIRST byte, so ` #a` is a pattern, not \
              a comment; trimming first dropped the rule and SERVED a file the \
              operator asked the daemon to hide",
    },
    Case {
        body: "   \n",
        served: &["a", "a ", " #a", "x.tmp", "keep"],
        why: "exclude.c:1806 tests `*line`, so a whitespace-only record is not \
              empty and becomes a pattern",
    },
    Case {
        body: "a \r*.tmp\r",
        served: &["a", " #a", "   ", "keep"],
        why: "a lone `\\r` ends a record (exclude.c:1774), so this file holds \
              TWO rules; `str::lines` fused them into one pattern carrying the \
              `\\r`, which matched nothing and served both files",
    },
    // CONTROL. Without it every row above is satisfied by a reader that drops
    // its input entirely, and the first row is satisfied by one that trims -
    // this is the row where the trimmed and untrimmed readings agree on the
    // rule but disagree on which of `a` / `a ` it names.
    Case {
        body: "a\n",
        served: &["a ", " #a", "   ", "x.tmp", "keep"],
        why: "CONTROL: the same rule with NO trailing space names `a`, so the \
              pair is selected the other way round and the filter is live",
    },
];

struct Fixture {
    root: tempfile::TempDir,
    port: u16,
}

impl Fixture {
    fn new(body: &str) -> Option<Self> {
        let root = tempfile::tempdir().expect("temp dir");
        let port = free_port()?;
        let module = root.path().join("mod");
        fs::create_dir_all(&module).expect("module dir");
        for name in PLANTED {
            fs::write(module.join(name), b"x\n").expect("plant module file");
        }
        fs::write(root.path().join("ef"), body.as_bytes()).expect("filter file");
        let conf = format!(
            "port = {port}\n\
             use chroot = no\n\
             \n\
             [m]\n\
             \tpath = {module}\n\
             \tread only = yes\n\
             \texclude from = {ef}\n",
            module = module.display(),
            ef = root.path().join("ef").display(),
        );
        fs::write(root.path().join("rsyncd.conf"), conf).expect("config");
        Some(Self { root, port })
    }

    fn conf(&self) -> PathBuf {
        self.root.path().join("rsyncd.conf")
    }

    /// Pulls the whole module and reports the names that arrived.
    fn served(&self, daemon_bin: &Path, client_bin: &Path) -> BTreeSet<String> {
        let mut daemon = spawn_daemon(daemon_bin, &self.conf(), self.port);
        let dest = self.root.path().join("dest");
        fs::create_dir_all(&dest).expect("dest dir");
        let status = Command::new(client_bin)
            .args([
                "-r",
                "-q",
                &format!("rsync://127.0.0.1:{}/m/", self.port),
                &format!("{}/", dest.display()),
            ])
            .stdin(Stdio::null())
            .status()
            .expect("run client");
        let _ = daemon.kill();
        let _ = daemon.wait();
        assert!(status.success(), "the pull itself must succeed: {status:?}");

        fs::read_dir(&dest)
            .expect("read dest")
            .map(|entry| {
                entry
                    .expect("dest entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }
}

/// Starts a daemon and waits until its port answers.
///
/// `stdin` is `/dev/null` deliberately: with an inherited terminal or pipe the
/// daemon takes its single-connection path and never listens.
fn spawn_daemon(binary: &Path, conf: &Path, port: u16) -> Child {
    let child = Command::new(binary)
        .arg("--daemon")
        .arg("--no-detach")
        .arg(format!("--config={}", conf.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return child;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

fn assert_table(binary: &Path, label: &str) {
    for case in CASES {
        let Some(fixture) = Fixture::new(case.body) else {
            println!("SKIP: no loopback port available");
            return;
        };
        let served = fixture.served(binary, binary);
        let expected: BTreeSet<String> = case.served.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(
            served, expected,
            "{label}: `exclude from` holding {:?} must serve {:?} - {}",
            case.body, case.served, case.why
        );
    }
}

#[test]
fn oc_keeps_daemon_filter_file_whitespace_as_pattern_text() {
    assert_table(&oc_binary(), "oc");
}

/// CROSS-IMPLEMENTATION: the expected column is upstream's behaviour, so assert
/// it against upstream rather than trusting the transcription.
#[test]
fn upstream_keeps_daemon_filter_file_whitespace_as_pattern_text() {
    let Some(upstream) = upstream_binary() else {
        println!(
            "SKIP: upstream 3.5.0 oracle not built \
             (target/interop/upstream-src/rsync-3.5.0/rsync)"
        );
        return;
    };
    assert_table(&upstream, "upstream 3.5.0");
}
