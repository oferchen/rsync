//! Regression test (immunity by defence): a local copy over a destination that
//! already holds a special (FIFO) or symlink whose metadata differs must apply
//! the change WITHOUT opening the node for content.
//!
//! Upstream never opens a special or symlink to transfer content: a
//! same-`_S_IFMT` special quick-checks equal and its attributes are applied in
//! place (`generator.c:2032-2055` `set_file_attrs`), and a symlink is recreated
//! or its attributes set - neither is ever `open()`ed. oc mirrors this: the
//! special/symlink copy path is entirely path-based (`mkfifo`/`mknod` +
//! `utimensat(AT_SYMLINK_NOFOLLOW)` + `fchmodat`), so it never issues an
//! `open()` on the node.
//!
//! Why this test exists / why it is non-vacuous: opening a FIFO with no writer
//! blocks forever in the kernel (`fifo_open` -> `wait_for_partner`). So if the
//! copy path ever regressed to `open()` the node (e.g. to checksum or transfer
//! a special as if it were a regular file), this copy would hang rather than
//! return. The test runs the copy on a worker thread under a wall-clock guard:
//! a regression turns into a bounded failure ("did not return"), and a clean
//! run returns in well under the budget. The post-copy assertions confirm the
//! metadata really was applied, so the test cannot pass vacuously by skipping
//! the entries.
//!
//! IMPORTANT for anyone editing the fixture: set the FIFO's mtime with
//! `utimensat` (here via `touch`), NEVER `filetime::set_file_mtime` -
//! `filetime` opens the path `O_RDONLY` to get an fd for `futimens`, which
//! itself blocks on a FIFO with no writer and would hang this test in *setup*,
//! before the code under test ever runs. `set_symlink_file_times` uses
//! `utimensat(AT_SYMLINK_NOFOLLOW)` and is safe, but `touch -h` is used here for
//! both so the fixture is uniformly open-free.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use tempfile::tempdir;

/// Wall-clock budget for the copy. A clean run finishes in well under a second;
/// a regression that `open()`s the FIFO would block indefinitely, so the guard
/// converts that hang into a bounded, legible failure.
const COPY_BUDGET: Duration = Duration::from_secs(20);

/// Set an entry's mtime via `utimensat` (through `touch`), never `filetime` -
/// see the module doc for why `filetime` would hang the fixture on a FIFO.
fn touch(path: &Path, args: &[&str], date: &str) {
    let ok = Command::new("touch")
        .args(args)
        .arg("-d")
        .arg(date)
        .arg(path)
        .status()
        .expect("spawn touch")
        .success();
    assert!(ok, "touch failed for {}", path.display());
}

fn mkfifo(path: &Path) {
    assert!(
        Command::new("mkfifo")
            .arg(path)
            .status()
            .expect("spawn mkfifo")
            .success(),
        "mkfifo failed for {}",
        path.display()
    );
}

#[test]
fn local_copy_of_preexisting_special_and_symlink_completes_without_opening_the_node() {
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let temp = tempdir().map_err(|e| e.to_string())?;
            let source = temp.path().join("src");
            let dest = temp.path().join("dst");
            fs::create_dir_all(&source).map_err(|e| e.to_string())?;
            fs::create_dir_all(&dest).map_err(|e| e.to_string())?;

            // A pre-existing FIFO whose mode + mtime differ, a pre-existing
            // symlink whose mtime differs (same target), and an identical
            // regular file (quick-check skipped).
            for root in [&source, &dest] {
                mkfifo(&root.join("afifo"));
                fs::write(root.join("tgt"), b"payload\n").map_err(|e| e.to_string())?;
                symlink("tgt", root.join("alink")).map_err(|e| e.to_string())?;
            }
            fs::set_permissions(source.join("afifo"), fs::Permissions::from_mode(0o644))
                .map_err(|e| e.to_string())?;
            fs::set_permissions(dest.join("afifo"), fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
            // Source newer than destination so the mtime comparison flags a
            // change deterministically. `touch` uses utimensat - no open().
            touch(&source.join("afifo"), &[], "2025-01-01");
            touch(&source.join("tgt"), &[], "2025-01-01");
            touch(&source.join("alink"), &["-h"], "2025-06-01");
            touch(&dest.join("afifo"), &[], "2024-01-01");
            touch(&dest.join("tgt"), &[], "2025-01-01"); // identical -> skipped
            touch(&dest.join("alink"), &["-h"], "2024-06-01");

            let options = LocalCopyOptions::default()
                .recursive(true)
                .links(true)
                .specials(true)
                .permissions(true)
                .times(true);

            let mut src_arg = source.clone().into_os_string();
            src_arg.push("/");
            let operands = vec![src_arg, dest.clone().into_os_string()];
            let plan = LocalCopyPlan::from_operands(&operands).map_err(|e| e.to_string())?;
            plan.execute_with_options(LocalCopyExecution::Apply, options)
                .map_err(|e| e.to_string())?;

            // The special really was updated in place (not skipped): its mode
            // now matches the source, and it is still a FIFO (not opened,
            // truncated, or replaced with a regular file).
            let fifo_meta = fs::symlink_metadata(dest.join("afifo")).map_err(|e| e.to_string())?;
            use std::os::unix::fs::FileTypeExt;
            if !fifo_meta.file_type().is_fifo() {
                return Err("destination afifo is no longer a FIFO".into());
            }
            if fifo_meta.permissions().mode() & 0o777 != 0o644 {
                return Err(format!(
                    "destination FIFO mode not applied in place: {:o}",
                    fifo_meta.permissions().mode() & 0o777
                ));
            }
            let link_meta = fs::symlink_metadata(dest.join("alink")).map_err(|e| e.to_string())?;
            if !link_meta.file_type().is_symlink() {
                return Err("destination alink is no longer a symlink".into());
            }
            Ok(())
        })();
        let _ = tx.send(result);
    });

    match rx.recv_timeout(COPY_BUDGET) {
        Ok(Ok(())) => {
            worker.join().expect("worker thread panicked");
        }
        Ok(Err(msg)) => panic!("local copy failed: {msg}"),
        Err(_) => panic!(
            "local copy did not return within {COPY_BUDGET:?}: the special/symlink copy \
             path opened the node for content (a FIFO open with no writer blocks in \
             wait_for_partner). It must stay path-based - mkfifo/mknod + \
             utimensat/fchmodat - and never open() a special or symlink."
        ),
    }
}
