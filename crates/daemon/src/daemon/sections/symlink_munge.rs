// Re-export core munge/unmunge functions from the metadata crate.
// The canonical implementation lives in `metadata::symlink_munge` so that
// both the daemon and the client-side transfer path share the same logic.
//
// upstream: clientserver.c - `munge_symlinks` defaults to `!use_chroot`.
// When enabled, symlink targets are prefixed with `/rsyncd-munged/` on send
// and the prefix is stripped on receive, preventing symlinks from escaping
// the module root on non-chrooted modules.
#[allow(unused_imports)] // REASON: re-export for daemon file list processing; currently used in tests
pub(crate) use ::metadata::symlink_munge::{munge_symlink, unmunge_symlink};

#[cfg(test)]
mod symlink_munge_tests {
    use super::*;


    #[test]
    fn munge_prepends_prefix_to_absolute_path() {
        let result = munge_symlink("/etc/passwd");
        assert_eq!(result, "/rsyncd-munged//etc/passwd");
    }

    #[test]
    fn munge_prepends_prefix_to_relative_path() {
        let result = munge_symlink("../secret");
        assert_eq!(result, "/rsyncd-munged/../secret");
    }

    #[test]
    fn munge_prepends_prefix_to_simple_filename() {
        let result = munge_symlink("file.txt");
        assert_eq!(result, "/rsyncd-munged/file.txt");
    }

    #[test]
    fn munge_handles_empty_target() {
        let result = munge_symlink("");
        assert_eq!(result, "/rsyncd-munged/");
    }

    #[test]
    fn munge_handles_dot_target() {
        let result = munge_symlink(".");
        assert_eq!(result, "/rsyncd-munged/.");
    }


    #[test]
    fn unmunge_strips_prefix() {
        let result = unmunge_symlink("/rsyncd-munged//etc/passwd");
        assert_eq!(result, Some("/etc/passwd".to_owned()));
    }

    #[test]
    fn unmunge_strips_prefix_from_relative() {
        let result = unmunge_symlink("/rsyncd-munged/../secret");
        assert_eq!(result, Some("../secret".to_owned()));
    }

    #[test]
    fn unmunge_returns_none_for_non_munged() {
        let result = unmunge_symlink("/etc/passwd");
        assert!(result.is_none());
    }

    #[test]
    fn unmunge_returns_none_for_partial_prefix() {
        let result = unmunge_symlink("/rsyncd-munged");
        assert!(result.is_none());
    }

    #[test]
    fn unmunge_returns_none_for_empty() {
        let result = unmunge_symlink("");
        assert!(result.is_none());
    }

    #[test]
    fn unmunge_strips_prefix_leaving_empty() {
        let result = unmunge_symlink("/rsyncd-munged/");
        assert_eq!(result, Some(String::new()));
    }


    #[test]
    fn roundtrip_absolute_path() {
        let original = "/usr/local/bin/tool";
        let munged = munge_symlink(original);
        let restored = unmunge_symlink(&munged);
        assert_eq!(restored, Some(original.to_owned()));
    }

    #[test]
    fn roundtrip_relative_path() {
        let original = "../../other/dir/file";
        let munged = munge_symlink(original);
        let restored = unmunge_symlink(&munged);
        assert_eq!(restored, Some(original.to_owned()));
    }

    #[test]
    fn roundtrip_empty_target() {
        let original = "";
        let munged = munge_symlink(original);
        let restored = unmunge_symlink(&munged);
        assert_eq!(restored, Some(original.to_owned()));
    }

    #[test]
    fn roundtrip_preserves_unicode() {
        let original = "/data/\u{00e9}l\u{00e8}ve/notes";
        let munged = munge_symlink(original);
        let restored = unmunge_symlink(&munged);
        assert_eq!(restored, Some(original.to_owned()));
    }


    #[test]
    fn effective_munge_auto_chroot_on() {
        let def = ModuleDefinition {
            use_chroot: true,
            munge_symlinks: None,
            ..Default::default()
        };
        assert!(!def.effective_munge_symlinks());
    }

    #[test]
    fn effective_munge_auto_chroot_off() {
        let def = ModuleDefinition {
            use_chroot: false,
            munge_symlinks: None,
            ..Default::default()
        };
        assert!(def.effective_munge_symlinks());
    }

    #[test]
    fn effective_munge_explicit_true_overrides_chroot() {
        let def = ModuleDefinition {
            use_chroot: true,
            munge_symlinks: Some(true),
            ..Default::default()
        };
        assert!(def.effective_munge_symlinks());
    }

    #[test]
    fn effective_munge_explicit_false_overrides_no_chroot() {
        let def = ModuleDefinition {
            use_chroot: false,
            munge_symlinks: Some(false),
            ..Default::default()
        };
        assert!(!def.effective_munge_symlinks());
    }

    #[test]
    fn effective_munge_explicit_true_no_chroot() {
        let def = ModuleDefinition {
            use_chroot: false,
            munge_symlinks: Some(true),
            ..Default::default()
        };
        assert!(def.effective_munge_symlinks());
    }

    #[test]
    fn effective_munge_explicit_false_with_chroot() {
        let def = ModuleDefinition {
            use_chroot: true,
            munge_symlinks: Some(false),
            ..Default::default()
        };
        assert!(!def.effective_munge_symlinks());
    }
}
