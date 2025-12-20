//! Tests for deprecated option aliases and legacy compatibility.
//!
//! Validates that deprecated aliases still work correctly for backwards
//! compatibility with scripts and existing workflows.

use cli::test_utils::parse_args;

// ============================================================================
// Long-form Deprecated Aliases
// ============================================================================

#[test]
fn test_old_dirs_alias_for_no_mkpath() {
    let args = parse_args(["oc-rsync", "--old-dirs", "src", "dest"]).unwrap();
    assert!(!args.mkpath, "--old-dirs should behave like --no-mkpath");
}

#[test]
fn test_old_d_alias_for_no_mkpath() {
    let args = parse_args(["oc-rsync", "--old-d", "src", "dest"]).unwrap();
    assert!(!args.mkpath, "--old-d should behave like --no-mkpath");
}

#[test]
fn test_tmp_dir_alias_for_temp_dir() {
    let args = parse_args(["oc-rsync", "--tmp-dir=/tmp", "src", "dest"]).unwrap();
    assert_eq!(
        args.temp_dir,
        Some("/tmp".into()),
        "--tmp-dir should behave like --temp-dir"
    );
}

#[test]
fn test_del_alias_for_delete() {
    let args = parse_args(["oc-rsync", "--del", "--dirs", "src", "dest"]).unwrap();
    assert!(
        args.delete_mode.is_enabled(),
        "--del should behave like --delete"
    );
}

#[test]
fn test_stderr_option_accepts_mode() {
    let args = parse_args(["oc-rsync", "--stderr=errors", "src", "dest"]).unwrap();
    assert_eq!(
        args.stderr_mode,
        Some("errors".into()),
        "--stderr should accept a mode argument"
    );
}

#[test]
fn test_stderr_option_accepts_all_mode() {
    let args = parse_args(["oc-rsync", "--stderr=all", "src", "dest"]).unwrap();
    assert_eq!(
        args.stderr_mode,
        Some("all".into()),
        "--stderr=all should set stderr_mode to 'all'"
    );
}

#[test]
fn test_log_format_alias_for_out_format() {
    let args = parse_args(["oc-rsync", "--log-format=%n", "src", "dest"]).unwrap();
    assert_eq!(
        args.out_format,
        Some("%n".into()),
        "--log-format should behave like --out-format"
    );
}

#[test]
fn test_i_r_alias_for_inc_recursive() {
    let args = parse_args(["oc-rsync", "--i-r", "src", "dest"]).unwrap();
    assert_eq!(
        args.inc_recursive,
        Some(true),
        "--i-r should behave like --inc-recursive"
    );
}

#[test]
fn test_no_i_r_alias_for_no_inc_recursive() {
    let args = parse_args(["oc-rsync", "--no-i-r", "src", "dest"]).unwrap();
    assert_eq!(
        args.inc_recursive,
        Some(false),
        "--no-i-r should behave like --no-inc-recursive"
    );
}

#[test]
fn test_i_d_alias_for_implied_dirs() {
    let args = parse_args(["oc-rsync", "--i-d", "src", "dest"]).unwrap();
    assert_eq!(
        args.implied_dirs,
        Some(true),
        "--i-d should behave like --implied-dirs"
    );
}

#[test]
fn test_no_i_d_alias_for_no_implied_dirs() {
    let args = parse_args(["oc-rsync", "--no-i-d", "src", "dest"]).unwrap();
    assert_eq!(
        args.implied_dirs,
        Some(false),
        "--no-i-d should behave like --no-implied-dirs"
    );
}

// ============================================================================
// Short-form Negative Aliases
// ============================================================================

#[test]
fn test_no_o_alias_for_no_owner() {
    let args = parse_args(["oc-rsync", "--no-o", "src", "dest"]).unwrap();
    assert_eq!(
        args.owner,
        Some(false),
        "--no-o should behave like --no-owner"
    );
}

#[test]
fn test_no_g_alias_for_no_group() {
    let args = parse_args(["oc-rsync", "--no-g", "src", "dest"]).unwrap();
    assert_eq!(
        args.group,
        Some(false),
        "--no-g should behave like --no-group"
    );
}

#[test]
fn test_no_p_alias_for_no_perms() {
    let args = parse_args(["oc-rsync", "--no-p", "src", "dest"]).unwrap();
    assert_eq!(
        args.perms,
        Some(false),
        "--no-p should behave like --no-perms"
    );
}

#[test]
fn test_no_t_alias_for_no_times() {
    let args = parse_args(["oc-rsync", "--no-t", "src", "dest"]).unwrap();
    assert_eq!(
        args.times,
        Some(false),
        "--no-t should behave like --no-times"
    );
}

#[test]
fn test_no_capital_o_alias_for_no_omit_dir_times() {
    let args = parse_args(["oc-rsync", "--no-O", "src", "dest"]).unwrap();
    assert_eq!(
        args.omit_dir_times,
        Some(false),
        "--no-O should behave like --no-omit-dir-times"
    );
}

#[test]
fn test_no_capital_j_alias_for_no_omit_link_times() {
    let args = parse_args(["oc-rsync", "--no-J", "src", "dest"]).unwrap();
    assert_eq!(
        args.omit_link_times,
        Some(false),
        "--no-J should behave like --no-omit-link-times"
    );
}

#[test]
fn test_no_capital_a_alias_for_no_acls() {
    let args = parse_args(["oc-rsync", "--no-A", "src", "dest"]).unwrap();
    assert_eq!(
        args.acls,
        Some(false),
        "--no-A should behave like --no-acls"
    );
}

#[test]
fn test_no_capital_x_alias_for_no_xattrs() {
    let args = parse_args(["oc-rsync", "--no-X", "src", "dest"]).unwrap();
    assert_eq!(
        args.xattrs,
        Some(false),
        "--no-X should behave like --no-xattrs"
    );
}

#[test]
fn test_no_h_alias_for_no_human_readable() {
    let args = parse_args(["oc-rsync", "--no-h", "src", "dest"]).unwrap();
    // human_readable defaults to None, --no-h sets it to Disabled
    assert!(
        args.human_readable.is_some(),
        "--no-h should behave like --no-human-readable"
    );
}

#[test]
fn test_no_r_alias_for_no_recursive() {
    let args = parse_args(["oc-rsync", "--no-r", "src", "dest"]).unwrap();
    assert_eq!(
        args.recursive_override,
        Some(false),
        "--no-r should behave like --no-recursive"
    );
}

#[test]
fn test_no_d_alias_for_no_dirs() {
    let args = parse_args(["oc-rsync", "--no-d", "src", "dest"]).unwrap();
    assert_eq!(
        args.dirs,
        Some(false),
        "--no-d should behave like --no-dirs"
    );
}

#[test]
fn test_no_c_alias_for_no_checksum() {
    let args = parse_args(["oc-rsync", "--no-c", "src", "dest"]).unwrap();
    assert_eq!(
        args.checksum,
        Some(false),
        "--no-c should behave like --no-checksum"
    );
}

#[test]
fn test_no_capital_s_alias_for_no_sparse() {
    let args = parse_args(["oc-rsync", "--no-S", "src", "dest"]).unwrap();
    assert_eq!(
        args.sparse,
        Some(false),
        "--no-S should behave like --no-sparse"
    );
}

#[test]
fn test_no_l_alias_for_no_links() {
    let args = parse_args(["oc-rsync", "--no-l", "src", "dest"]).unwrap();
    assert_eq!(
        args.links,
        Some(false),
        "--no-l should behave like --no-links"
    );
}

#[test]
fn test_no_capital_h_alias_for_no_hard_links() {
    let args = parse_args(["oc-rsync", "--no-H", "src", "dest"]).unwrap();
    assert_eq!(
        args.hard_links,
        Some(false),
        "--no-H should behave like --no-hard-links"
    );
}

#[test]
fn test_no_v_alias_for_no_verbose() {
    let args = parse_args(["oc-rsync", "--no-v", "src", "dest"]).unwrap();
    // no-verbose doesn't directly map but affects verbosity parsing
    assert_eq!(args.verbosity, 0, "--no-v should behave like --no-verbose");
}

#[test]
fn test_no_capital_r_alias_for_no_relative() {
    let args = parse_args(["oc-rsync", "--no-R", "src", "dest"]).unwrap();
    assert_eq!(
        args.relative,
        Some(false),
        "--no-R should behave like --no-relative"
    );
}

// ============================================================================
// Alias Interaction with Standard Options
// ============================================================================

#[test]
fn test_old_dirs_works_with_other_options() {
    let args = parse_args(["oc-rsync", "--old-dirs", "-a", "src", "dest"]).unwrap();
    assert!(!args.mkpath, "--old-dirs should work with -a");
    assert!(args.archive);
}

#[test]
fn test_del_is_alias_for_delete_during() {
    // In upstream rsync, --del is an alias for --delete-during, not --delete
    let args = parse_args(["oc-rsync", "--del", "--dirs", "src", "dest"]).unwrap();
    // --del should activate delete during (same as --delete-during)
    assert!(
        args.delete_mode.is_enabled(),
        "--del should enable deletion"
    );
}

#[test]
fn test_del_and_delete_before_are_mutually_exclusive() {
    // --del is --delete-during, which is mutually exclusive with --delete-before
    let result = parse_args([
        "oc-rsync",
        "--del",
        "--delete-before",
        "--dirs",
        "src",
        "dest",
    ]);
    assert!(
        result.is_err(),
        "--del (--delete-during) and --delete-before should be mutually exclusive"
    );
}

#[test]
fn test_tmp_dir_and_temp_dir_are_same_option() {
    // --tmp-dir and --temp-dir are the same option (alias), so using both is an error
    let result = parse_args([
        "oc-rsync",
        "--temp-dir=/tmp1",
        "--tmp-dir=/tmp2",
        "src",
        "dest",
    ]);
    assert!(
        result.is_err(),
        "--tmp-dir and --temp-dir are aliases and cannot both be specified"
    );
    let err = result.unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}
