//! A local receiver enters its destination before it writes anything, as
//! upstream's `get_local_name()` does, and refuses a destination component
//! that is a symlink owned by neither root nor the euid.
//!
//! Each cell plants an ordinary symlink and has the owner walk judge it as
//! another uid's through `fast_io::test_seams`, so the cells run unprivileged.
//! The expected lines and exit codes are rsync 3.5.2's, measured on the same
//! fixtures with a real foreign-owned symlink:
//!
//! - an existing destination directory: `change_dir#1` (main.c:777-781);
//! - a directory the run creates: `change_dir#2` (main.c:837-839);
//! - the parent of a single-file destination: `change_dir#3`
//!   (main.c:865-868).
//!
//! All three exit `RERR_FILESELECT` (3) after the refusal line
//! (syscall.c:561-564). One deliberate deviation: 3.5.2 creates the new
//! directory through the symlink before `change_dir#2` refuses it, leaving
//! `outside/newdir` behind; oc refuses before creating anything.

#[cfg(unix)]
mod untrusted_destination_symlink {
    use std::fs;
    use std::io;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};

    use core::client::{ClientConfig, ClientError, run_client};
    use engine::local_copy::upstream_io_error;

    /// The name every cell gives the planted symlink.
    const LINK: &str = "sub";

    fn foreign_uid() -> u32 {
        let euid = rustix::process::geteuid().as_raw();
        (1..=8u32)
            .find(|candidate| *candidate != euid)
            .expect("at least one of uids 1..=8 differs from the euid")
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        base: PathBuf,
    }

    impl Fixture {
        /// `src/f`, `src/d/g`, an empty `dest/` and an empty `outside/`.
        fn new() -> Self {
            let temp = test_support::create_tempdir();
            let base = fs::canonicalize(temp.path()).expect("canonicalize");
            fs::create_dir_all(base.join("src/d")).expect("mkdir src");
            fs::write(base.join("src/f"), b"hello\n").expect("write f");
            fs::write(base.join("src/d/g"), b"two\n").expect("write g");
            fs::create_dir(base.join("dest")).expect("mkdir dest");
            fs::create_dir(base.join("outside")).expect("mkdir outside");
            Self { _temp: temp, base }
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.base.join(relative)
        }

        /// `dest/sub -> outside`.
        fn link_sub_to_outside(&self) {
            symlink(self.path("outside"), self.path("dest").join(LINK)).expect("plant");
        }

        /// What the transfer left in `outside/`, sorted.
        fn outside(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(self.path("outside"))
                .expect("outside")
                .map(|entry| {
                    entry
                        .expect("entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            names.sort();
            names
        }

        fn copy(&self, operands: &[&str], recursive: bool) -> Result<(), ClientError> {
            let config = ClientConfig::builder()
                .transfer_args(
                    operands
                        .iter()
                        .map(|operand| self.path(operand).into_os_string()),
                )
                .recursive(recursive)
                .times(true)
                .build();
            run_client(config).map(drop)
        }

        /// Runs `operands` with `dest/sub` judged as another uid's.
        fn copy_with_foreign_link(
            &self,
            operands: &[&str],
            recursive: bool,
        ) -> Result<(), ClientError> {
            let _owner = fast_io::test_seams::judge_symlink_owner(LINK, foreign_uid());
            self.copy(operands, recursive)
        }
    }

    /// `[Receiver] change_dir#N "<dir>" failed: <ELOOP>`, exit 3.
    fn assert_change_dir_refusal(error: &ClientError, number: u8, directory: &Path) {
        let eloop = io::Error::from_raw_os_error(rustix::io::Errno::LOOP.raw_os_error());
        let expected = format!(
            "[Receiver] change_dir#{number} \"{}\" failed: {}",
            directory.display(),
            upstream_io_error(&eloop)
        );
        assert_eq!(error.program_line(), Some(expected.as_str()), "{error}");
        assert_eq!(error.exit_code(), 3, "{error}");
    }

    /// 3.5.2: `change_dir#3 ".../dest/sub"`, exit 3, nothing in `outside/`.
    /// Before the fix oc judged the parent symlink an obstruction and exited
    /// 23 with a line 3.5.2 never prints.
    #[test]
    fn a_single_file_under_an_untrusted_parent_symlink_is_refused_at_change_dir_3() {
        let fx = Fixture::new();
        fx.link_sub_to_outside();

        let error = fx
            .copy_with_foreign_link(&["src/f", "dest/sub/g"], false)
            .expect_err("the untrusted parent must be refused");

        assert_change_dir_refusal(&error, 3, &fx.path("dest/sub"));
        assert!(
            fx.outside().is_empty(),
            "wrote through the symlink: {:?}",
            fx.outside()
        );
    }

    /// 3.5.2: `change_dir#3 ".../dest/sub/deeper"`, exit 3. The untrusted
    /// link is an interior component of the parent, not the parent itself.
    /// Before the fix the parent stat'ed as a directory through the link and
    /// oc wrote `outside/deeper/g`, exit 0.
    #[test]
    fn a_single_file_under_an_untrusted_interior_symlink_is_refused_at_change_dir_3() {
        let fx = Fixture::new();
        fs::create_dir(fx.path("outside/deeper")).expect("mkdir deeper");
        fx.link_sub_to_outside();

        let error = fx
            .copy_with_foreign_link(&["src/f", "dest/sub/deeper/g"], false)
            .expect_err("the untrusted interior component must be refused");

        assert_change_dir_refusal(&error, 3, &fx.path("dest/sub/deeper"));
        assert_eq!(fx.outside(), ["deeper"]);
        assert!(
            !fx.path("outside/deeper/g").exists(),
            "wrote through the symlink"
        );
    }

    /// CONTROL. 3.5.2 follows the operator's own symlinked parent and writes
    /// `outside/g`, exit 0. Before the fix oc exited 23.
    #[test]
    fn a_single_file_under_the_operators_own_parent_symlink_is_written_through_it() {
        let fx = Fixture::new();
        fx.link_sub_to_outside();

        fx.copy(&["src/f", "dest/sub/g"], false)
            .expect("the operator's own parent symlink is followed");

        assert_eq!(
            fs::read(fx.path("outside/g")).expect("outside/g"),
            b"hello\n"
        );
    }

    /// 3.5.2: `change_dir#2 ".../dest/sub/newdir"`, exit 3, after creating
    /// `outside/newdir` through the link. oc refuses before the mkdir.
    #[test]
    fn a_directory_created_for_several_entries_under_an_untrusted_symlink_is_refused() {
        let fx = Fixture::new();
        fx.link_sub_to_outside();

        let error = fx
            .copy_with_foreign_link(&["src/f", "src/d", "dest/sub/newdir"], true)
            .expect_err("the untrusted symlink must be refused");

        assert_change_dir_refusal(&error, 2, &fx.path("dest/sub/newdir"));
        assert!(
            fx.outside().is_empty(),
            "created through the symlink: {:?}",
            fx.outside()
        );
    }

    /// 3.5.2: the trailing slash asks for a directory; same `change_dir#2`
    /// line and exit 3 as the several-entry form.
    #[test]
    fn a_directory_created_for_a_trailing_slash_under_an_untrusted_symlink_is_refused() {
        let fx = Fixture::new();
        fx.link_sub_to_outside();

        let error = fx
            .copy_with_foreign_link(&["src/f", "dest/sub/newdir/"], false)
            .expect_err("the untrusted symlink must be refused");

        assert_change_dir_refusal(&error, 2, &fx.path("dest/sub/newdir"));
        assert!(
            fx.outside().is_empty(),
            "created through the symlink: {:?}",
            fx.outside()
        );
    }

    /// CONTROL. The operator's own symlink above a created directory is
    /// followed: 3.5.2 creates `outside/newdir` and copies into it.
    #[test]
    fn a_directory_created_under_the_operators_own_symlink_is_written_through_it() {
        let fx = Fixture::new();
        fx.link_sub_to_outside();

        fx.copy(&["src/f", "dest/sub/newdir/"], false)
            .expect("the operator's own symlink is followed");

        assert_eq!(
            fs::read(fx.path("outside/newdir/f")).expect("newdir/f"),
            b"hello\n"
        );
    }

    /// 3.5.2: `change_dir#1 ".../dest/sub"`, exit 3, for an existing
    /// destination directory reached through the link. Before the fix oc
    /// printed `failed to access destination directory`, a line 3.5.2 never
    /// prints.
    #[test]
    fn an_existing_destination_through_an_untrusted_symlink_is_refused_at_change_dir_1() {
        let fx = Fixture::new();
        fx.link_sub_to_outside();

        let error = fx
            .copy_with_foreign_link(&["src/d/", "dest/sub"], true)
            .expect_err("the untrusted destination symlink must be refused");

        assert_change_dir_refusal(&error, 1, &fx.path("dest/sub"));
        assert!(
            fx.outside().is_empty(),
            "wrote through the symlink: {:?}",
            fx.outside()
        );
    }

    /// CONTROL. The operator's own symlinked destination - the
    /// `/backup -> /mnt/disk` admin layout - is followed.
    #[test]
    fn an_existing_destination_through_the_operators_own_symlink_is_followed() {
        let fx = Fixture::new();
        fx.link_sub_to_outside();

        fx.copy(&["src/d/", "dest/sub"], true)
            .expect("the operator's own destination symlink is followed");

        assert_eq!(fs::read(fx.path("outside/g")).expect("outside/g"), b"two\n");
    }

    /// `--mkpath` builds a single-file destination's missing parents the way
    /// 3.5.2's `make_path()` does, through `do_mkdir_at()`
    /// (syscall.c:2290-2335). A relative destination takes the
    /// `secure_relative_dirfd()` arm: an in-tree symlink is followed and one
    /// that leaves the tree fails the mkdir with `ELOOP`, whoever owns it
    /// (main.c:757-760, :819, exit 11). Cells with a relative destination
    /// run with the cwd at the fixture base; nextest gives each test its own
    /// process.
    mod mkpath {
        use super::*;

        impl Fixture {
            fn enter(&self) {
                std::env::set_current_dir(&self.base).expect("enter the fixture");
            }

            /// `--mkpath src/f <dest>`, the destination spelled as given.
            fn mkpath_copy(&self, dest: &Path, insecure_links: bool) -> Result<(), ClientError> {
                let config = ClientConfig::builder()
                    .transfer_args([
                        self.path("src/f").into_os_string(),
                        dest.as_os_str().to_owned(),
                    ])
                    .mkpath(true)
                    .insecure_links(insecure_links)
                    .times(true)
                    .build();
                run_client(config).map(drop)
            }
        }

        /// `[Receiver] mkdir "<base>/dest/sub/new/g" failed: <ELOOP>`, exit 11.
        fn assert_mkdir_refusal(error: &ClientError, destination: &Path) {
            let eloop = io::Error::from_raw_os_error(rustix::io::Errno::LOOP.raw_os_error());
            let expected = format!(
                "[Receiver] mkdir \"{}\" failed: {}",
                destination.display(),
                upstream_io_error(&eloop)
            );
            assert_eq!(error.program_line(), Some(expected.as_str()), "{error}");
            assert_eq!(error.exit_code(), 11, "{error}");
        }

        /// 3.5.2: the user's own `dest/sub -> <abs>/outside` is refused by
        /// the confined walk, exit 11, nothing created outside.
        #[test]
        fn a_relative_destination_through_an_own_absolute_symlink_fails_the_mkdir() {
            let fx = Fixture::new();
            fx.link_sub_to_outside();
            fx.enter();

            let error = fx
                .mkpath_copy(Path::new("dest/sub/new/g"), false)
                .expect_err("an absolute target leaves the tree");

            assert_mkdir_refusal(&error, &fx.path("dest/sub/new/g"));
            assert!(
                fx.outside().is_empty(),
                "created outside: {:?}",
                fx.outside()
            );
        }

        /// The same refusal for another user's link: the confined arm judges
        /// where the link points, not who owns it.
        #[test]
        fn a_relative_destination_through_a_foreign_absolute_symlink_fails_the_mkdir() {
            let fx = Fixture::new();
            fx.link_sub_to_outside();
            fx.enter();
            let _owner = fast_io::test_seams::judge_symlink_owner(LINK, foreign_uid());

            let error = fx
                .mkpath_copy(Path::new("dest/sub/new/g"), false)
                .expect_err("an absolute target leaves the tree");

            assert_mkdir_refusal(&error, &fx.path("dest/sub/new/g"));
            assert!(
                fx.outside().is_empty(),
                "created outside: {:?}",
                fx.outside()
            );
        }

        /// 3.5.2 follows `dest/sub -> ../outside`, which stays beneath the
        /// cwd, and copies through it, exit 0.
        #[test]
        fn a_relative_destination_through_an_in_tree_symlink_is_followed() {
            let fx = Fixture::new();
            symlink("../outside", fx.path("dest").join(LINK)).expect("plant");
            fx.enter();

            fx.mkpath_copy(Path::new("dest/sub/new/g"), false)
                .expect("an in-tree symlink is followed");

            assert_eq!(
                fs::read(fx.path("outside/new/g")).expect("outside/new/g"),
                b"hello\n"
            );
        }

        /// 3.5.2 builds an absolute destination with a plain mkdir, so the
        /// user's own link is followed, exit 0.
        #[test]
        fn an_absolute_destination_through_an_own_symlink_is_followed() {
            let fx = Fixture::new();
            fx.link_sub_to_outside();

            fx.mkpath_copy(&fx.path("dest/sub/new/g"), false)
                .expect("the user's own symlink is followed");

            assert_eq!(
                fs::read(fx.path("outside/new/g")).expect("outside/new/g"),
                b"hello\n"
            );
        }

        /// Deliberate deviation: 3.5.2 creates `outside/new` through another
        /// user's link and then refuses at `change_dir#3`; oc walks the
        /// leading directories first and refuses with the same line and
        /// exit 3 before creating anything.
        #[test]
        fn an_absolute_destination_through_a_foreign_symlink_is_refused_before_any_mkdir() {
            let fx = Fixture::new();
            fx.link_sub_to_outside();
            let _owner = fast_io::test_seams::judge_symlink_owner(LINK, foreign_uid());

            let error = fx
                .mkpath_copy(&fx.path("dest/sub/new/g"), false)
                .expect_err("the untrusted parent must be refused");

            assert_change_dir_refusal(&error, 3, &fx.path("dest/sub/new"));
            assert!(
                fx.outside().is_empty(),
                "created outside: {:?}",
                fx.outside()
            );
        }

        /// `--insecure-links` turns off both arms (syscall.c:117-130
        /// secure_relpath_active() returns 0 under symlink_optout_allowed()):
        /// even another user's absolute link is followed.
        #[test]
        fn insecure_links_follows_a_foreign_absolute_symlink_on_a_relative_destination() {
            let fx = Fixture::new();
            fx.link_sub_to_outside();
            fx.enter();
            let _owner = fast_io::test_seams::judge_symlink_owner(LINK, foreign_uid());
            // The CLI publishes the flag for the whole process before the
            // transfer (cli drive/workflow/run.rs); this in-process cell does
            // the same.
            fast_io::confinement::install_local_session(
                fast_io::confinement::LocalInsecureLinks::from_local_flag(true),
                None,
            );

            fx.mkpath_copy(Path::new("dest/sub/new/g"), true)
                .expect("the opt-out follows every symlink");

            assert_eq!(
                fs::read(fx.path("outside/new/g")).expect("outside/new/g"),
                b"hello\n"
            );
        }
    }
}
