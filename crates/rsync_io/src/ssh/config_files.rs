//! The ssh_config file load order, shared by both config readers.
//!
//! OpenSSH reads the client configuration from up to two files per
//! connection (openssh/ssh.c:561-592 `process_config_files()`):
//!
//! 1. the user file - the `-F` value when given, else `~/.ssh/config`;
//! 2. the system file `/etc/ssh/ssh_config` - but ONLY when no `-F` was
//!    given: an explicit `-F` suppresses the system file entirely
//!    (the `else` arm at openssh/ssh.c:578), and `-F none` (compared
//!    case-insensitively, openssh/ssh.c:571-583) reads no file at all.
//!
//! Both files feed ONE per-keyword claimed-slot state, so
//! first-obtained-wins arbitrates across the file boundary: a keyword the
//! user file claimed is dead in the system file, while an accumulating
//! keyword (`IdentityFile`) keeps appending. Each file's Host/Match block
//! state, by contrast, is its own - `read_config_file` starts every file
//! back at the always-active top level (openssh/readconf.c
//! `read_config_file_depth()` re-initialises `active`).
//!
//! `SSHCONF_CHECKPERM` (openssh/readconf.h:216) applies exactly one row of
//! that table: the DEFAULT user file (openssh/ssh.c:583). Neither an
//! explicit `-F` file nor the system file is permission-checked.
//!
//! This module owns which files are read, in what order, and which of
//! them is permission-checked. What each reader DOES with a file's
//! content - and what it does when a file is refused - stays with that
//! reader, per the split documented in [`crate::ssh::config_options`].

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// The system-wide client config, upstream's `_PATH_HOST_CONFIG_FILE`
/// (openssh/ssh.c:587-589).
pub(in crate::ssh) const SYSTEM_CONFIG: &str = "/etc/ssh/ssh_config";

/// One config file in the load order, plus the two upstream flags a
/// reader needs from it: whether the `SSHCONF_CHECKPERM` owner/permission
/// check applies, and whether it is a USER config (`SSHCONF_USERCONF`).
#[derive(Debug, Clone, Eq, PartialEq)]
pub(in crate::ssh) struct ConfigFile {
    /// The file to read. May not exist; a missing default file is skipped.
    pub(in crate::ssh) path: PathBuf,
    /// Whether to run the owner/permission check before reading. `true`
    /// only for the default `~/.ssh/config` (openssh/ssh.c:583).
    pub(in crate::ssh) check_perm: bool,
    /// Whether upstream reads this file with `SSHCONF_USERCONF`
    /// (openssh/ssh.c:574, :583) - `true` for the `-F` file and the
    /// default `~/.ssh/config`, `false` for the system file. It decides
    /// where a RELATIVE `Include` anchors (`~/.ssh` vs `/etc/ssh`) and
    /// whether a `~`-prefixed include path is accepted at all
    /// (openssh/readconf.c:2095-2103).
    pub(in crate::ssh) user_conf: bool,
}

/// Returns the ordered config-file load list for `options`, the ssh
/// option argv (where a `-F` override may appear).
pub(in crate::ssh) fn config_files(options: &[OsString]) -> Vec<ConfigFile> {
    config_files_from(
        extract_dash_f_path(options),
        home_dir(),
        PathBuf::from(SYSTEM_CONFIG),
    )
}

/// The composition behind [`config_files`], with every input a parameter
/// so tests can inject fixture paths instead of the host's real
/// `/etc/ssh/ssh_config`. [`config_files`] is the one production caller
/// and differs only in supplying the live `-F`/home/system values.
pub(in crate::ssh) fn config_files_from(
    dash_f: Option<PathBuf>,
    home: Option<PathBuf>,
    system: PathBuf,
) -> Vec<ConfigFile> {
    if let Some(file) = dash_f {
        // `-F none` reads no configuration file at all; the comparison is
        // case-insensitive (openssh/ssh.c:571-577 `strcasecmp(config,
        // "none")`).
        if file.as_os_str().eq_ignore_ascii_case("none") {
            return Vec::new();
        }
        // An explicit `-F` file is read INSTEAD of the user file, is not
        // permission-checked, and suppresses the system file entirely
        // (openssh/ssh.c:571-583; the system read sits in the `else`).
        // It is still a USER config (openssh/ssh.c:574 passes
        // `SSHCONF_USERCONF`).
        return vec![ConfigFile {
            path: file,
            check_perm: false,
            user_conf: true,
        }];
    }
    let mut files = Vec::with_capacity(2);
    if let Some(home) = home {
        files.push(ConfigFile {
            path: home.join(".ssh").join("config"),
            check_perm: true,
            user_conf: true,
        });
    }
    files.push(ConfigFile {
        path: system,
        check_perm: false,
        user_conf: false,
    });
    files
}

/// Walks `options` looking for `-F file` (split across two args) or
/// `-Ffile` (concatenated). Returns the first occurrence as a
/// [`PathBuf`]. The ONE owner of `-F` extraction for both readers.
pub(in crate::ssh) fn extract_dash_f_path(options: &[OsString]) -> Option<PathBuf> {
    let mut iter = options.iter();
    while let Some(opt) = iter.next() {
        if opt == OsStr::new("-F") {
            return iter.next().map(PathBuf::from);
        }
        let bytes = opt.to_string_lossy();
        if let Some(rest) = bytes.strip_prefix("-F")
            && !rest.is_empty()
        {
            return Some(PathBuf::from(rest));
        }
    }
    None
}

/// Resolves the per-user home directory from `HOME` (Unix) or
/// `USERPROFILE` (Windows).
pub(in crate::ssh) fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

/// Runs upstream's `SSHCONF_CHECKPERM` owner/permission check on `path`.
///
/// upstream: openssh/readconf.c:2579-2587 - the file must be owned by
/// root or by the caller's REAL uid (`getuid()`), and must not be group-
/// or world-writable (`st_mode & 022`). A failure is upstream's fatal
/// `Bad owner or permissions on <path>`; the caller decides whether that
/// is fatal here (embedded transport) or the degraded no-answer
/// (compression lookup).
///
/// A file that cannot be stat'ed passes: upstream only reaches the check
/// after `fopen` succeeds, and a missing default file is simply skipped.
/// Non-Unix platforms have no POSIX ownership model and always pass.
pub(in crate::ssh) fn check_default_user_config_perms(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Ok(metadata) = std::fs::metadata(path) else {
            return Ok(());
        };
        let owner_ok = metadata.uid() == 0 || metadata.uid() == platform::privilege::real_uid();
        if !owner_ok || (metadata.mode() & 0o022) != 0 {
            return Err(format!("Bad owner or permissions on {}", path.display()));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dash_f(value: &str) -> Vec<OsString> {
        vec![OsString::from("-F"), OsString::from(value)]
    }

    /// The default load order: user file first, system file second, with
    /// the permission check on the user file ONLY (openssh/ssh.c:583,
    /// :587-589).
    #[test]
    fn default_order_is_user_then_system_and_only_user_is_checked() {
        let files = config_files_from(
            None,
            Some(PathBuf::from("/home/u")),
            PathBuf::from("/fixture/etc/ssh_config"),
        );
        assert_eq!(
            files,
            vec![
                ConfigFile {
                    path: PathBuf::from("/home/u/.ssh/config"),
                    check_perm: true,
                    user_conf: true,
                },
                ConfigFile {
                    path: PathBuf::from("/fixture/etc/ssh_config"),
                    check_perm: false,
                    user_conf: false,
                },
            ]
        );
    }

    /// `-F` reads that file INSTEAD of the user file and suppresses the
    /// system file entirely (the `else` at openssh/ssh.c:578), and the
    /// explicit file is not permission-checked.
    #[test]
    fn dash_f_suppresses_both_default_files_and_is_unchecked() {
        let files = config_files_from(
            Some(PathBuf::from("/custom")),
            Some(PathBuf::from("/home/u")),
            PathBuf::from("/fixture/etc/ssh_config"),
        );
        assert_eq!(
            files,
            vec![ConfigFile {
                path: PathBuf::from("/custom"),
                check_perm: false,
                user_conf: true,
            }]
        );
    }

    /// `-F none` reads no configuration at all, matched
    /// case-insensitively (openssh/ssh.c:571-577 `strcasecmp`).
    #[test]
    fn dash_f_none_reads_no_file_at_all() {
        for spelling in ["none", "NONE", "None"] {
            let files = config_files_from(
                Some(PathBuf::from(spelling)),
                Some(PathBuf::from("/home/u")),
                PathBuf::from("/etc/ssh/ssh_config"),
            );
            assert!(files.is_empty(), "-F {spelling}");
        }
    }

    /// No home directory drops the user entry but keeps the system file:
    /// upstream reads the system file unconditionally in the no-`-F` arm.
    #[test]
    fn without_a_home_only_the_system_file_remains() {
        let files = config_files_from(None, None, PathBuf::from("/etc/ssh/ssh_config"));
        assert_eq!(
            files,
            vec![ConfigFile {
                path: PathBuf::from("/etc/ssh/ssh_config"),
                check_perm: false,
                user_conf: false,
            }]
        );
    }

    /// The live wrapper differs from the seam only in supplying the live
    /// `-F`/home/system values - the pin that keeps the injected-path
    /// tests speaking for the production entry.
    #[test]
    fn live_wrapper_is_the_seam_with_live_inputs() {
        for options in [Vec::new(), dash_f("/custom")] {
            assert_eq!(
                config_files(&options),
                config_files_from(
                    extract_dash_f_path(&options),
                    home_dir(),
                    PathBuf::from(SYSTEM_CONFIG),
                ),
                "options {options:?}"
            );
        }
    }

    #[test]
    fn extracts_split_and_combined_dash_f() {
        assert_eq!(
            extract_dash_f_path(&dash_f("/tmp/custom")),
            Some(PathBuf::from("/tmp/custom"))
        );
        assert_eq!(
            extract_dash_f_path(&[OsString::from("-F/tmp/custom")]),
            Some(PathBuf::from("/tmp/custom"))
        );
        assert!(extract_dash_f_path(&[OsString::from("-oBatchMode=yes")]).is_none());
    }

    /// The check itself, on constructed fixtures: a mode a real `ssh`
    /// accepts passes, group- or world-writable is refused with
    /// upstream's own wording, and a missing file passes (the check runs
    /// only after a successful open upstream).
    #[cfg(unix)]
    #[test]
    fn perm_check_refuses_group_or_world_writable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config");
        std::fs::write(&path, "Host a\n").expect("write fixture");

        for good in [0o600, 0o644] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(good)).expect("chmod");
            assert_eq!(
                check_default_user_config_perms(&path),
                Ok(()),
                "mode {good:o}"
            );
        }
        for bad in [0o664, 0o622, 0o666] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(bad)).expect("chmod");
            assert_eq!(
                check_default_user_config_perms(&path),
                Err(format!("Bad owner or permissions on {}", path.display())),
                "mode {bad:o}"
            );
        }
        assert_eq!(
            check_default_user_config_perms(Path::new("/nonexistent/ssh/config")),
            Ok(())
        );
    }
}
