//! Local `--write-batch` must produce a batch its own `--read-batch` can decode.
//!
//! The local write-batch path is not a byte tee of a real wire stream the way
//! upstream's is (`io.c:1962-1963`, armed at `io.c:2528-2529`; upstream forks a
//! real `local_child` server at `main.c:648-654` precisely so a stream exists to
//! tee). oc re-encodes the file list with a second `FileListWriter`, which must
//! agree with the reader entry field for entry field. Every option below gates a
//! per-entry field, so any disagreement makes the reader decode the following
//! entry's flag byte as payload and desynchronise the whole stream - the
//! observable symptom being `xattr index N out of range (cache size 0)` under
//! `-X`, and a silent garbage decode under the rest.
//!
//! These are round-trips on purpose. A unit test over the writer's builder chain
//! would pass against a chain that still disagrees with the reader; only feeding
//! the bytes back through the real reader proves the two agree.

mod integration;

use integration::helpers::{RsyncCommand, TestDir};

/// Options that each gate a per-entry flist field on the local write path.
///
/// `-X`/`-A`/`-c` have a `batch.c:59-76 flag_ptr[]` bit, so the writer derives
/// them from the recorded header. `-U`/`-N` have no bit - upstream does not need
/// one because its replaying receiver reads them off the replay script's argv -
/// so oc carries them into the reader from the `--read-batch` invocation.
const FIELD_GATING_OPTIONS: &[&[&str]] = &[
    &["-X"],
    &["-A"],
    &["-c"],
    &["-U"],
    &["-N"],
    &["--specials"],
    &["-X", "-A", "-c", "-U", "-N"],
];

/// A local `--write-batch` under any per-entry-field option must replay into a
/// byte-identical tree.
///
/// Multiple files are required: a single-entry flist cannot reveal a desync,
/// because the reader only runs off the end of an entry when another entry
/// follows it.
#[test]
fn local_write_batch_replays_under_every_field_gating_option() {
    for extra in FIELD_GATING_OPTIONS {
        let test_dir = TestDir::new().expect("create test dir");
        let src = test_dir.mkdir("src").expect("create src");

        for name in ["alpha.txt", "beta.txt", "gamma.txt"] {
            test_dir
                .write_file(&format!("src/{name}"), name.as_bytes())
                .expect("write source file");
        }
        test_dir
            .write_file("src/nested/delta.txt", b"nested payload")
            .expect("write nested source file");

        // A special file must exist or the `--specials` row is vacuous: the
        // per-entry rdev field only appears for an entry that is one, so a
        // fixture of plain files could never expose a specials disagreement.
        #[cfg(unix)]
        if extra.contains(&"--specials") {
            // Same `mkfifo(1)` shell-out as tests/drop_devices.rs, rather than a
            // new dependency for one fixture.
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
        for opt in *extra {
            record.arg(opt);
        }
        record
            .arg(format!("--write-batch={}", batch_path.display()))
            .arg(format!("{}/", src.display()))
            .arg(format!("{}/", direct.display()));
        record.assert_success();

        assert!(
            batch_path.exists(),
            "{extra:?}: --write-batch must create '{}'",
            batch_path.display()
        );

        // Replay. The same options ride the replay invocation, mirroring the
        // generated BATCH.sh, which is where --atimes/--crtimes reach the
        // reader.
        let mut replay = RsyncCommand::new();
        replay.arg("-a");
        for opt in *extra {
            replay.arg(opt);
        }
        replay
            .arg(format!("--read-batch={}", batch_path.display()))
            .arg(format!("{}/", replayed.display()));
        let output = replay.assert_success();

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("out of range"),
            "{extra:?}: replay reported a decode error, so the writer and reader \
             disagree about the entry layout: {stderr}"
        );

        for name in ["alpha.txt", "beta.txt", "gamma.txt", "nested/delta.txt"] {
            let want = direct.join(name);
            let got = replayed.join(name);
            assert!(
                got.exists(),
                "{extra:?}: replay did not produce '{}' (stderr: {stderr})",
                got.display()
            );
            assert_eq!(
                std::fs::read(&want).expect("read reference"),
                std::fs::read(&got).expect("read replayed"),
                "{extra:?}: replayed '{name}' differs from the directly-copied tree"
            );
        }
    }
}
