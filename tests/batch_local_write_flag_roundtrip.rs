//! Local `--write-batch` must produce a batch its own `--read-batch` can decode.
//!
//! The local write-batch path is not a byte tee of a real wire stream the way
//! upstream's is (`io.c:1962-1963`, armed at `io.c:2528-2529`; upstream forks a
//! real `local_child` server at `main.c:648-654` precisely so a stream exists to
//! tee). oc re-encodes the file list with a second `FileListWriter`, which must
//! agree with the reader entry field for entry field. Every option exercised
//! here gates a per-entry field, so a disagreement makes the reader decode the
//! following entry's flag byte as payload and desynchronise the stream - the
//! observable symptom being `xattr index N out of range (cache size 0)` under
//! `-X`, and a silent garbage decode or an empty replay under the rest.
//!
//! These are round-trips on purpose. A unit test over the writer's builder chain
//! would pass against a chain that still disagrees with the reader; only feeding
//! the bytes back through the real reader proves the two agree.

mod integration;

use integration::helpers::{RsyncCommand, TestDir};
use test_support::Capabilities;
use test_support::capabilities::names;

/// One option combination whose flist encoding must survive a round-trip.
struct FieldGatingCase {
    /// Options passed to both the recording and the replaying invocation.
    options: &'static [&'static str],
    /// Capabilities the binary must advertise, or the case cannot run.
    ///
    /// Empty means unconditional: those cases are what keeps the test from
    /// passing vacuously on a build with every optional feature disabled.
    requires: &'static [&'static str],
    /// Whether the fixture needs a special file for the case to mean anything.
    ///
    /// The per-entry rdev field only appears for an entry that *is* special, so
    /// a `--specials` case over plain files would pass for want of a FIFO
    /// rather than for want of a bug. On a platform that cannot create one the
    /// case is skipped for that reason rather than run vacuously.
    needs_special_file: bool,
}

/// Whether this platform can create the FIFO a `needs_special_file` case wants.
///
/// `cfg!` rather than `#[cfg]` deliberately: the field must be read on every
/// platform, otherwise the Windows build sees a never-read field.
const PLATFORM_HAS_FIFO: bool = cfg!(unix);

impl FieldGatingCase {
    /// Returns why this case cannot run here, or `None` if it can.
    ///
    /// Two independent reasons - a capability the binary was not built with,
    /// and a fixture this platform cannot create - reported through one
    /// accessor so callers cannot honour only one of them.
    fn skip_reason(&self, capabilities: &Capabilities) -> Option<String> {
        if self.needs_special_file && !PLATFORM_HAS_FIFO {
            return Some(
                "platform cannot create a FIFO, so the rdev field \
                         would never be exercised"
                    .to_owned(),
            );
        }
        capabilities.skip_reason(self.requires)
    }

    /// Reports whether the case runs everywhere, so it can anchor non-vacuity.
    const fn is_unconditional(&self) -> bool {
        self.requires.is_empty() && (!self.needs_special_file || PLATFORM_HAS_FIFO)
    }
}

/// Options that each gate a per-entry flist field on the local write path.
///
/// `-X`/`-A`/`-c` have a `batch.c:59-76 flag_ptr[]` bit, so the writer derives
/// them from the recorded header. `-U`/`-N` have no bit - upstream does not need
/// one because its replaying receiver reads them off the replay script's argv -
/// so oc carries them into the reader from the `--read-batch` invocation.
const FIELD_GATING_CASES: &[FieldGatingCase] = &[
    FieldGatingCase {
        options: &["-X"],
        requires: &[names::XATTRS],
        needs_special_file: false,
    },
    FieldGatingCase {
        options: &["-A"],
        requires: &[names::ACLS],
        needs_special_file: false,
    },
    FieldGatingCase {
        options: &["-c"],
        requires: &[],
        needs_special_file: false,
    },
    FieldGatingCase {
        options: &["-U"],
        requires: &[names::ATIMES],
        needs_special_file: false,
    },
    FieldGatingCase {
        options: &["-N"],
        requires: &[names::CRTIMES],
        needs_special_file: false,
    },
    FieldGatingCase {
        options: &["--specials"],
        requires: &[],
        needs_special_file: true,
    },
    FieldGatingCase {
        options: &["-X", "-A", "-c", "-U", "-N"],
        requires: &[names::XATTRS, names::ACLS, names::ATIMES, names::CRTIMES],
        needs_special_file: false,
    },
];

/// A local `--write-batch` under any per-entry-field option must replay into a
/// byte-identical tree.
#[test]
fn local_write_batch_replays_under_every_field_gating_option() {
    let capabilities = Capabilities::probe();
    let mut ran = 0usize;

    for case in FIELD_GATING_CASES {
        if let Some(reason) = case.skip_reason(capabilities) {
            // Printed, not silent: a skipped row must leave a trace, or a build
            // with the feature off looks identical to one that passed.
            println!("skipping {:?}: {reason}", case.options);
            continue;
        }
        run_round_trip(case);
        ran += 1;
    }

    // Derived, not a magic number: every case that needs no optional feature
    // and no fixture this platform lacks must have run. That is what makes a
    // "0 tests ran, success" outcome impossible, and it stays correct when the
    // table grows or when a platform loses a capability.
    let unconditional = FIELD_GATING_CASES
        .iter()
        .filter(|case| case.is_unconditional())
        .count();
    assert!(
        unconditional >= 1,
        "the case table has no unconditional case left, so this test could \
         pass without exercising anything"
    );
    assert!(
        ran >= unconditional,
        "only {ran} of {} cases ran but {unconditional} are unconditional here",
        FIELD_GATING_CASES.len()
    );
}

/// Records a batch with `case.options`, replays it, and compares the replayed
/// tree against the one the same invocation wrote directly.
///
/// Multiple files are required: a single-entry flist cannot reveal a desync,
/// because the reader only runs off the end of an entry when another follows it.
fn run_round_trip(case: &FieldGatingCase) {
    const PAYLOAD_FILES: &[&str] = &["alpha.txt", "beta.txt", "gamma.txt", "nested/delta.txt"];

    let options = case.options;
    let test_dir = TestDir::new().expect("create test dir");
    let src = test_dir.mkdir("src").expect("create src");

    for name in PAYLOAD_FILES {
        test_dir
            .write_file(&format!("src/{name}"), name.as_bytes())
            .expect("write source file");
    }
    if case.needs_special_file {
        // Same `mkfifo(1)` shell-out as tests/drop_devices.rs, rather than a new
        // dependency for one fixture. Unreachable where `PLATFORM_HAS_FIFO` is
        // false, so no `#[cfg]` is needed and the field stays read everywhere.
        let status = std::process::Command::new("mkfifo")
            .arg(src.join("fifo"))
            .status()
            .expect("spawn mkfifo");
        assert!(status.success(), "mkfifo failed: {status}");
    }

    let direct = test_dir.mkdir("direct").expect("create direct");
    let replayed = test_dir.mkdir("replayed").expect("create replayed");
    let batch_path = test_dir.path().join("BATCH");

    // Record. This also performs the transfer into `direct/`, which is the
    // reference tree the replay must reproduce.
    let mut record = RsyncCommand::new();
    record.arg("-a");
    for opt in options {
        record.arg(opt);
    }
    record
        .arg(format!("--write-batch={}", batch_path.display()))
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", direct.display()));
    record.assert_success();

    assert!(
        batch_path.exists(),
        "{options:?}: --write-batch must create '{}'",
        batch_path.display()
    );

    // Replay. The same options ride the replay invocation, mirroring the
    // generated BATCH.sh, which is where --atimes/--crtimes reach the reader.
    let mut replay = RsyncCommand::new();
    replay.arg("-a");
    for opt in options {
        replay.arg(opt);
    }
    replay
        .arg(format!("--read-batch={}", batch_path.display()))
        .arg(format!("{}/", replayed.display()));
    let output = replay.assert_success();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("out of range"),
        "{options:?}: replay reported a decode error, so the writer and reader \
         disagree about the entry layout: {stderr}"
    );

    for name in PAYLOAD_FILES {
        let want = direct.join(name);
        let got = replayed.join(name);
        assert!(
            got.exists(),
            "{options:?}: replay did not produce '{}' (stderr: {stderr})",
            got.display()
        );
        assert_eq!(
            std::fs::read(&want).expect("read reference"),
            std::fs::read(&got).expect("read replayed"),
            "{options:?}: replayed '{name}' differs from the directly-copied tree"
        );
    }
}
