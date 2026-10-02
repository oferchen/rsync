//! A local receiver resolves an existing destination directory through the
//! ownership walk before it writes anything, as upstream's `change_dir()`
//! does: a destination component that is a symlink owned by neither root nor
//! the euid is refused (exit 3, `RERR_FILESELECT`) instead of followed, while
//! the operator's own symlinked destination is still followed.
//!
//! Planting a symlink owned by another uid needs root, so the cells run only
//! as root and report the skip otherwise - the same gate as upstream's
//! `symlink-race-dest` test.
//!
//! upstream: `rsync-3.5.1/util1.c:1363-1392` `change_dir()`,
//! `rsync-3.5.1/syscall.c:499-504` (the refusal diagnostic),
//! `rsync-3.5.1/main.c:778-781` (`RERR_FILESELECT`).

#[cfg(unix)]
mod untrusted_destination_symlink {
    use std::fs;
    use std::os::unix::fs::{lchown, symlink};
    use std::path::Path;

    use core::client::{ClientConfig, run_client};

    /// uid 65534 is `nobody` on every supported Unix.
    const UNTRUSTED_UID: u32 = 65534;

    fn is_root() -> bool {
        rustix::process::geteuid().is_root()
    }

    fn fixture(base: &Path) {
        fs::create_dir_all(base.join("src/sub")).expect("mkdir src");
        for i in 0..3 {
            fs::write(base.join(format!("src/sub/f{i}")), b"payload\n").expect("write");
        }
        fs::create_dir_all(base.join("dest")).expect("mkdir dest");
        fs::create_dir_all(base.join("outside")).expect("mkdir outside");
    }

    fn push(base: &Path) -> Result<core::client::ClientSummary, core::client::ClientError> {
        let config = ClientConfig::builder()
            .transfer_args([
                base.join("src/sub/").into_os_string(),
                base.join("dest/sub/").into_os_string(),
            ])
            .recursive(true)
            .times(true)
            .build();
        run_client(config)
    }

    /// WITNESS. An attacker-owned destination symlink is refused with
    /// `RERR_FILESELECT`, and nothing lands where it points.
    #[test]
    fn an_untrusted_destination_symlink_is_refused() {
        if !is_root() {
            eprintln!("skipped: planting a symlink owned by another uid needs root");
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let base = fs::canonicalize(temp.path()).expect("canonicalize");
        fixture(&base);
        symlink(base.join("outside"), base.join("dest/sub")).expect("plant");
        lchown(
            base.join("dest/sub"),
            Some(UNTRUSTED_UID),
            Some(UNTRUSTED_UID),
        )
        .expect("lchown");

        let error = push(&base).expect_err("an untrusted destination symlink must be refused");
        assert_eq!(error.exit_code(), 3, "{error}");
        assert_eq!(
            fs::read_dir(base.join("outside")).expect("outside").count(),
            0,
            "the receiver wrote through the attacker's symlink"
        );
    }

    /// CONTROL. The operator's own (root-owned) symlinked destination - the
    /// `/backup -> /mnt/disk` admin pattern - is still followed.
    #[test]
    fn a_trusted_destination_symlink_is_followed() {
        if !is_root() {
            eprintln!("skipped: the matching witness needs root");
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let base = fs::canonicalize(temp.path()).expect("canonicalize");
        fixture(&base);
        symlink(base.join("outside"), base.join("dest/sub")).expect("plant");

        push(&base).expect("a trusted destination symlink is followed");
        assert!(base.join("outside/f0").is_file());
    }
}
