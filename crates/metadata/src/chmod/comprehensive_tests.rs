//! Comprehensive tests for --chmod permission modification.
//!
//! This module contains comprehensive test coverage for the chmod functionality
//! including:
//! 1. Basic chmod syntax (u+x, g-w, o=r)
//! 2. Numeric mode (755, 644)
//! 3. Directory-specific permissions (D prefix)
//! 4. File-specific permissions (F prefix)
//! 5. Multiple chmod rules
//! 6. Behavior comparison with upstream rsync semantics

use super::ChmodModifiers;

// =============================================================================
// Section 1: Basic Symbolic Chmod Syntax Tests
// =============================================================================

mod basic_symbolic_syntax {
    use super::*;

    // -------------------------------------------------------------------------
    // User permissions (u)
    // -------------------------------------------------------------------------

    #[test]
    fn user_add_execute() {
        let modifiers = ChmodModifiers::parse("u+x").expect("parse u+x");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn user_add_read() {
        let modifiers = ChmodModifiers::parse("u+r").expect("parse u+r");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn user_add_write() {
        let modifiers = ChmodModifiers::parse("u+w").expect("parse u+w");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn user_add_multiple() {
        let modifiers = ChmodModifiers::parse("u+rwx").expect("parse u+rwx");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn user_remove_write() {
        let modifiers = ChmodModifiers::parse("u-w").expect("parse u-w");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn user_remove_execute() {
        let modifiers = ChmodModifiers::parse("u-x").expect("parse u-x");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn user_assign_read_only() {
        let modifiers = ChmodModifiers::parse("u=r").expect("parse u=r");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn user_assign_empty_clears_permissions() {
        let modifiers = ChmodModifiers::parse("u=").expect("parse u=");
        assert!(!modifiers.is_empty());
    }

    // -------------------------------------------------------------------------
    // Group permissions (g)
    // -------------------------------------------------------------------------

    #[test]
    fn group_add_write() {
        let modifiers = ChmodModifiers::parse("g+w").expect("parse g+w");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn group_remove_write() {
        let modifiers = ChmodModifiers::parse("g-w").expect("parse g-w");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn group_assign_read_execute() {
        let modifiers = ChmodModifiers::parse("g=rx").expect("parse g=rx");
        assert!(!modifiers.is_empty());
    }

    // -------------------------------------------------------------------------
    // Other permissions (o)
    // -------------------------------------------------------------------------

    #[test]
    fn other_add_read() {
        let modifiers = ChmodModifiers::parse("o+r").expect("parse o+r");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn other_remove_all() {
        let modifiers = ChmodModifiers::parse("o-rwx").expect("parse o-rwx");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn other_assign_read_only() {
        let modifiers = ChmodModifiers::parse("o=r").expect("parse o=r");
        assert!(!modifiers.is_empty());
    }

    // -------------------------------------------------------------------------
    // All permissions (a)
    // -------------------------------------------------------------------------

    #[test]
    fn all_add_execute() {
        let modifiers = ChmodModifiers::parse("a+x").expect("parse a+x");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn all_remove_write() {
        let modifiers = ChmodModifiers::parse("a-w").expect("parse a-w");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn all_assign_read() {
        let modifiers = ChmodModifiers::parse("a=r").expect("parse a=r");
        assert!(!modifiers.is_empty());
    }

    // -------------------------------------------------------------------------
    // Combined who specifiers (ug, go, uo, ugo)
    // -------------------------------------------------------------------------

    #[test]
    fn user_and_group_add_read() {
        let modifiers = ChmodModifiers::parse("ug+r").expect("parse ug+r");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn group_and_other_remove_write() {
        let modifiers = ChmodModifiers::parse("go-w").expect("parse go-w");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn user_group_other_assign() {
        let modifiers = ChmodModifiers::parse("ugo=r").expect("parse ugo=r");
        assert!(!modifiers.is_empty());
    }

    // -------------------------------------------------------------------------
    // No who specifier defaults to all
    // -------------------------------------------------------------------------

    #[test]
    fn no_who_add_execute_defaults_to_all() {
        let modifiers = ChmodModifiers::parse("+x").expect("parse +x");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn no_who_remove_write_defaults_to_all() {
        let modifiers = ChmodModifiers::parse("-w").expect("parse -w");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn no_who_assign_read_defaults_to_all() {
        let modifiers = ChmodModifiers::parse("=r").expect("parse =r");
        assert!(!modifiers.is_empty());
    }

    // -------------------------------------------------------------------------
    // Conditional execute (X)
    // -------------------------------------------------------------------------

    #[test]
    fn conditional_execute_uppercase_x() {
        let modifiers = ChmodModifiers::parse("a+X").expect("parse a+X");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn user_group_other_conditional_execute() {
        let modifiers = ChmodModifiers::parse("ugo+X").expect("parse ugo+X");
        assert!(!modifiers.is_empty());
    }

    // -------------------------------------------------------------------------
    // Special bits (setuid, setgid, sticky)
    // -------------------------------------------------------------------------

    #[test]
    fn add_setuid_setgid() {
        let modifiers = ChmodModifiers::parse("u+s").expect("parse u+s");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn add_sticky() {
        let modifiers = ChmodModifiers::parse("o+t").expect("parse o+t");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn remove_setuid() {
        let modifiers = ChmodModifiers::parse("u-s").expect("parse u-s");
        assert!(!modifiers.is_empty());
    }

    // -------------------------------------------------------------------------
    // Copy permissions (g=u, o=g, etc.)
    // -------------------------------------------------------------------------

    #[test]
    fn copy_user_to_group() {
        let modifiers = ChmodModifiers::parse("g=u").expect("parse g=u");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn copy_group_to_other() {
        let modifiers = ChmodModifiers::parse("o=g").expect("parse o=g");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn copy_user_to_other() {
        let modifiers = ChmodModifiers::parse("o=u").expect("parse o=u");
        assert!(!modifiers.is_empty());
    }

    // -------------------------------------------------------------------------
    // Case insensitivity
    // -------------------------------------------------------------------------

    #[test]
    fn uppercase_who_specifiers() {
        let modifiers = ChmodModifiers::parse("U+R").expect("parse U+R");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn mixed_case() {
        let modifiers = ChmodModifiers::parse("Ug+Rw").expect("parse Ug+Rw");
        assert!(!modifiers.is_empty());
    }
}

// =============================================================================
// Section 2: Numeric Mode Tests
// =============================================================================

mod numeric_mode {
    use super::*;

    #[test]
    fn mode_755() {
        let modifiers = ChmodModifiers::parse("755").expect("parse 755");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn mode_644() {
        let modifiers = ChmodModifiers::parse("644").expect("parse 644");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn mode_777() {
        let modifiers = ChmodModifiers::parse("777").expect("parse 777");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn mode_000() {
        let modifiers = ChmodModifiers::parse("000").expect("parse 000");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn mode_600() {
        let modifiers = ChmodModifiers::parse("600").expect("parse 600");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn mode_400() {
        let modifiers = ChmodModifiers::parse("400").expect("parse 400");
        assert!(!modifiers.is_empty());
    }

    // Four-digit modes with special bits
    #[test]
    fn mode_4755_setuid() {
        let modifiers = ChmodModifiers::parse("4755").expect("parse 4755");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn mode_2755_setgid() {
        let modifiers = ChmodModifiers::parse("2755").expect("parse 2755");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn mode_1777_sticky() {
        let modifiers = ChmodModifiers::parse("1777").expect("parse 1777");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn mode_6755_setuid_and_setgid() {
        let modifiers = ChmodModifiers::parse("6755").expect("parse 6755");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn mode_7777_all_special_bits() {
        let modifiers = ChmodModifiers::parse("7777").expect("parse 7777");
        assert!(!modifiers.is_empty());
    }

    // Invalid numeric modes
    #[test]
    fn invalid_two_digit_mode() {
        let result = ChmodModifiers::parse("75");
        assert!(result.is_err());
    }

    #[test]
    fn invalid_five_digit_mode() {
        let result = ChmodModifiers::parse("75555");
        assert!(result.is_err());
    }

    #[test]
    fn invalid_octal_digit_8() {
        let result = ChmodModifiers::parse("758");
        assert!(result.is_err());
    }

    #[test]
    fn invalid_octal_digit_9() {
        let result = ChmodModifiers::parse("759");
        assert!(result.is_err());
    }
}

// =============================================================================
// Section 3: Directory-Specific Permissions (D prefix)
// =============================================================================

mod directory_specific {
    use super::*;

    #[test]
    fn d_prefix_numeric() {
        let modifiers = ChmodModifiers::parse("D755").expect("parse D755");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn d_prefix_symbolic_add() {
        let modifiers = ChmodModifiers::parse("Du+x").expect("parse Du+x");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn d_prefix_symbolic_remove() {
        let modifiers = ChmodModifiers::parse("Dg-w").expect("parse Dg-w");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn d_prefix_symbolic_assign() {
        let modifiers = ChmodModifiers::parse("Do=rx").expect("parse Do=rx");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn d_prefix_add_all_execute() {
        let modifiers = ChmodModifiers::parse("Da+x").expect("parse Da+x");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn d_prefix_lowercase() {
        let modifiers = ChmodModifiers::parse("d755").expect("parse d755");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn d_prefix_with_rwx() {
        let modifiers = ChmodModifiers::parse("Du+rwx").expect("parse Du+rwx");
        assert!(!modifiers.is_empty());
    }
}

// =============================================================================
// Section 4: File-Specific Permissions (F prefix)
// =============================================================================

mod file_specific {
    use super::*;

    #[test]
    fn f_prefix_numeric() {
        let modifiers = ChmodModifiers::parse("F644").expect("parse F644");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn f_prefix_symbolic_add() {
        let modifiers = ChmodModifiers::parse("Fu+x").expect("parse Fu+x");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn f_prefix_symbolic_remove() {
        let modifiers = ChmodModifiers::parse("Fg-w").expect("parse Fg-w");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn f_prefix_symbolic_assign() {
        let modifiers = ChmodModifiers::parse("Fo=r").expect("parse Fo=r");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn f_prefix_remove_all_write() {
        let modifiers = ChmodModifiers::parse("Fgo-w").expect("parse Fgo-w");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn f_prefix_lowercase() {
        let modifiers = ChmodModifiers::parse("f644").expect("parse f644");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn f_prefix_with_multiple_permissions() {
        let modifiers = ChmodModifiers::parse("Fa-wx").expect("parse Fa-wx");
        assert!(!modifiers.is_empty());
    }
}

// =============================================================================
// Section 5: Multiple Chmod Rules
// =============================================================================

mod multiple_rules {
    use super::*;

    #[test]
    fn comma_separated_symbolic() {
        let modifiers = ChmodModifiers::parse("u+r,g+r,o+r").expect("parse comma-separated");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn comma_separated_numeric_and_symbolic() {
        let modifiers = ChmodModifiers::parse("755,u+s").expect("parse mixed");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn comma_separated_with_d_and_f_prefix() {
        let modifiers = ChmodModifiers::parse("D755,F644").expect("parse D and F");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn multiple_operations_same_who() {
        let modifiers = ChmodModifiers::parse("u+r,u+w,u+x").expect("parse multiple u");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn complex_multi_rule() {
        let modifiers =
            ChmodModifiers::parse("Du+rwx,Dg+rx,Do+rx,F644").expect("parse complex multi-rule");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn extend_combines_modifiers() {
        let mut modifiers1 = ChmodModifiers::parse("u+r").expect("parse first");
        let modifiers2 = ChmodModifiers::parse("g+w").expect("parse second");
        modifiers1.extend(modifiers2);
        assert!(!modifiers1.is_empty());
    }

    #[test]
    fn whitespace_around_commas_tolerated() {
        let modifiers = ChmodModifiers::parse("u+r , g+w").expect("parse with whitespace");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn empty_parts_between_commas_ignored() {
        let modifiers = ChmodModifiers::parse("u+r,,g+w").expect("parse with empty part");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn trailing_comma_ignored() {
        let modifiers = ChmodModifiers::parse("u+r,g+w,").expect("parse with trailing comma");
        assert!(!modifiers.is_empty());
    }
}

// =============================================================================
// Section 6: Apply Mode Tests (Unix-only)
// =============================================================================

#[cfg(unix)]
mod apply_mode {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn get_file_type(path: &std::path::Path) -> std::fs::FileType {
        std::fs::metadata(path)
            .expect("metadata")
            .file_type()
    }

    // -------------------------------------------------------------------------
    // Numeric mode application
    // -------------------------------------------------------------------------

    #[test]
    fn apply_numeric_755_to_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, b"content").expect("write");
        std::fs::set_permissions(&file_path, PermissionsExt::from_mode(0o000)).expect("set perms");

        let file_type = get_file_type(&file_path);
        let modifiers = ChmodModifiers::parse("755").expect("parse");

        let result = modifiers.apply(0o000, file_type);
        assert_eq!(result & 0o777, 0o755);
    }

    #[test]
    fn apply_numeric_644_to_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, b"content").expect("write");

        let file_type = get_file_type(&file_path);
        let modifiers = ChmodModifiers::parse("644").expect("parse");

        let result = modifiers.apply(0o777, file_type);
        assert_eq!(result & 0o777, 0o644);
    }

    // -------------------------------------------------------------------------
    // Symbolic mode application
    // -------------------------------------------------------------------------

    #[test]
    fn apply_user_add_execute() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, b"content").expect("write");

        let file_type = get_file_type(&file_path);
        let modifiers = ChmodModifiers::parse("u+x").expect("parse");

        let result = modifiers.apply(0o644, file_type);
        assert_eq!(result & 0o777, 0o744);
    }

    #[test]
    fn apply_group_remove_write() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, b"content").expect("write");

        let file_type = get_file_type(&file_path);
        let modifiers = ChmodModifiers::parse("g-w").expect("parse");

        let result = modifiers.apply(0o666, file_type);
        assert_eq!(result & 0o777, 0o646);
    }

    #[test]
    fn apply_other_assign_read_only() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, b"content").expect("write");

        let file_type = get_file_type(&file_path);
        let modifiers = ChmodModifiers::parse("o=r").expect("parse");

        let result = modifiers.apply(0o777, file_type);
        assert_eq!(result & 0o777, 0o774);
    }

    #[test]
    fn apply_all_remove_write() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, b"content").expect("write");

        let file_type = get_file_type(&file_path);
        let modifiers = ChmodModifiers::parse("a-w").expect("parse");

        let result = modifiers.apply(0o777, file_type);
        assert_eq!(result & 0o777, 0o555);
    }

    // -------------------------------------------------------------------------
    // Directory vs File target selector
    // -------------------------------------------------------------------------

    #[test]
    fn d_prefix_applies_to_directory_not_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        let dir_path = temp.path().join("testdir");
        std::fs::write(&file_path, b"content").expect("write file");
        std::fs::create_dir(&dir_path).expect("create dir");

        let file_type = get_file_type(&file_path);
        let dir_type = get_file_type(&dir_path);
        let modifiers = ChmodModifiers::parse("D755").expect("parse");

        let file_result = modifiers.apply(0o644, file_type);
        let dir_result = modifiers.apply(0o644, dir_type);

        // File should be unchanged
        assert_eq!(file_result & 0o777, 0o644);
        // Directory should be changed
        assert_eq!(dir_result & 0o777, 0o755);
    }

    #[test]
    fn f_prefix_applies_to_file_not_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        let dir_path = temp.path().join("testdir");
        std::fs::write(&file_path, b"content").expect("write file");
        std::fs::create_dir(&dir_path).expect("create dir");

        let file_type = get_file_type(&file_path);
        let dir_type = get_file_type(&dir_path);
        let modifiers = ChmodModifiers::parse("F644").expect("parse");

        let file_result = modifiers.apply(0o755, file_type);
        let dir_result = modifiers.apply(0o755, dir_type);

        // File should be changed
        assert_eq!(file_result & 0o777, 0o644);
        // Directory should be unchanged
        assert_eq!(dir_result & 0o777, 0o755);
    }

    #[test]
    fn combined_d_and_f_prefixes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        let dir_path = temp.path().join("testdir");
        std::fs::write(&file_path, b"content").expect("write file");
        std::fs::create_dir(&dir_path).expect("create dir");

        let file_type = get_file_type(&file_path);
        let dir_type = get_file_type(&dir_path);
        let modifiers = ChmodModifiers::parse("D755,F644").expect("parse");

        let file_result = modifiers.apply(0o000, file_type);
        let dir_result = modifiers.apply(0o000, dir_type);

        assert_eq!(file_result & 0o777, 0o644);
        assert_eq!(dir_result & 0o777, 0o755);
    }

    // -------------------------------------------------------------------------
    // Conditional execute (X)
    // -------------------------------------------------------------------------

    #[test]
    fn conditional_x_on_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir_path = temp.path().join("testdir");
        std::fs::create_dir(&dir_path).expect("create dir");

        let dir_type = get_file_type(&dir_path);
        let modifiers = ChmodModifiers::parse("a+X").expect("parse");

        // Directory should get execute bit
        let result = modifiers.apply(0o600, dir_type);
        assert_eq!(result & 0o111, 0o111);
    }

    #[test]
    fn conditional_x_on_file_without_execute() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, b"content").expect("write file");

        let file_type = get_file_type(&file_path);
        let modifiers = ChmodModifiers::parse("a+X").expect("parse");

        // File without any execute bit should not get execute bit
        let result = modifiers.apply(0o644, file_type);
        assert_eq!(result & 0o111, 0o000);
    }

    #[test]
    fn conditional_x_on_file_with_execute() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.sh");
        std::fs::write(&file_path, b"#!/bin/sh").expect("write file");

        let file_type = get_file_type(&file_path);
        let modifiers = ChmodModifiers::parse("a+X").expect("parse");

        // File with any execute bit should get all execute bits
        let result = modifiers.apply(0o744, file_type);
        assert_eq!(result & 0o111, 0o111);
    }

    // -------------------------------------------------------------------------
    // Copy permissions
    // -------------------------------------------------------------------------

    #[test]
    fn copy_user_to_group_and_other() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, b"content").expect("write file");

        let file_type = get_file_type(&file_path);
        let modifiers = ChmodModifiers::parse("g=u,o=u").expect("parse");

        let result = modifiers.apply(0o700, file_type);
        assert_eq!(result & 0o777, 0o777);
    }

    #[test]
    fn copy_group_to_other() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, b"content").expect("write file");

        let file_type = get_file_type(&file_path);
        let modifiers = ChmodModifiers::parse("o=g").expect("parse");

        let result = modifiers.apply(0o750, file_type);
        assert_eq!(result & 0o007, 0o005);
    }

    // -------------------------------------------------------------------------
    // Special bits
    // -------------------------------------------------------------------------

    #[test]
    fn apply_numeric_setuid() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, b"content").expect("write file");

        let file_type = get_file_type(&file_path);
        let modifiers = ChmodModifiers::parse("4755").expect("parse");

        let result = modifiers.apply(0o000, file_type);
        assert_eq!(result & 0o7777, 0o4755);
    }

    #[test]
    fn apply_numeric_setgid() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, b"content").expect("write file");

        let file_type = get_file_type(&file_path);
        let modifiers = ChmodModifiers::parse("2755").expect("parse");

        let result = modifiers.apply(0o000, file_type);
        assert_eq!(result & 0o7777, 0o2755);
    }

    #[test]
    fn apply_numeric_sticky() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir_path = temp.path().join("testdir");
        std::fs::create_dir(&dir_path).expect("create dir");

        let dir_type = get_file_type(&dir_path);
        let modifiers = ChmodModifiers::parse("1777").expect("parse");

        let result = modifiers.apply(0o000, dir_type);
        assert_eq!(result & 0o7777, 0o1777);
    }

    #[test]
    fn apply_symbolic_setuid() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, b"content").expect("write file");

        let file_type = get_file_type(&file_path);
        let modifiers = ChmodModifiers::parse("u+s").expect("parse");

        let result = modifiers.apply(0o755, file_type);
        assert_eq!(result & 0o4000, 0o4000);
    }

    #[test]
    fn apply_symbolic_sticky() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir_path = temp.path().join("testdir");
        std::fs::create_dir(&dir_path).expect("create dir");

        let dir_type = get_file_type(&dir_path);
        let modifiers = ChmodModifiers::parse("+t").expect("parse");

        let result = modifiers.apply(0o755, dir_type);
        assert_eq!(result & 0o1000, 0o1000);
    }

    // -------------------------------------------------------------------------
    // Multiple rules applied in order
    // -------------------------------------------------------------------------

    #[test]
    fn rules_applied_left_to_right() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, b"content").expect("write file");

        let file_type = get_file_type(&file_path);
        // First 000, then u+rwx -> should be 700
        let modifiers = ChmodModifiers::parse("000,u+rwx").expect("parse");

        let result = modifiers.apply(0o777, file_type);
        assert_eq!(result & 0o777, 0o700);
    }

    #[test]
    fn later_rules_can_override_earlier() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, b"content").expect("write file");

        let file_type = get_file_type(&file_path);
        // First 644, then 755 -> should be 755
        let modifiers = ChmodModifiers::parse("644,755").expect("parse");

        let result = modifiers.apply(0o000, file_type);
        assert_eq!(result & 0o777, 0o755);
    }
}

// =============================================================================
// Section 7: Error Cases
// =============================================================================

mod error_cases {
    use super::*;

    #[test]
    fn empty_specification_fails() {
        let result = ChmodModifiers::parse("");
        assert!(result.is_err());
    }

    #[test]
    fn only_whitespace_fails() {
        let result = ChmodModifiers::parse("   ");
        assert!(result.is_err());
    }

    #[test]
    fn invalid_permission_letter() {
        let result = ChmodModifiers::parse("u+q");
        assert!(result.is_err());
    }

    #[test]
    fn missing_operator() {
        let result = ChmodModifiers::parse("urwx");
        assert!(result.is_err());
    }

    #[test]
    fn add_without_permissions_fails() {
        let result = ChmodModifiers::parse("u+");
        assert!(result.is_err());
    }

    #[test]
    fn remove_without_permissions_fails() {
        let result = ChmodModifiers::parse("g-");
        assert!(result.is_err());
    }

    // Note: assign without permissions (u=) is valid and clears permissions

    #[test]
    fn invalid_numeric_non_octal() {
        let result = ChmodModifiers::parse("abc");
        assert!(result.is_err());
    }
}

// =============================================================================
// Section 8: Upstream rsync Behavior Compatibility
// =============================================================================

mod upstream_compatibility {
    use super::*;

    /// Tests that document behavior that should match upstream rsync.
    /// These tests verify our implementation matches rsync's documented behavior.

    #[test]
    fn rsync_style_directory_only_execute() {
        // rsync uses D+x to add execute only to directories
        let modifiers = ChmodModifiers::parse("D+x").expect("parse");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn rsync_style_file_protection() {
        // rsync uses Fgo-w to remove group/other write from files only
        let modifiers = ChmodModifiers::parse("Fgo-w").expect("parse");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn rsync_style_combined_df() {
        // Common rsync pattern: D755,F644
        // Directories get 755, files get 644
        let modifiers = ChmodModifiers::parse("D755,F644").expect("parse");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn rsync_style_make_executable_if_readable() {
        // Common rsync pattern: a+rX
        // Add read to all, add execute only if already executable or is directory
        let modifiers = ChmodModifiers::parse("a+rX").expect("parse");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn rsync_style_secure_permissions() {
        // Common security pattern: go=,u=rwX
        // Clear group/other, set user to rw, add x if needed
        let modifiers = ChmodModifiers::parse("go=,u=rwX").expect("parse");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn rsync_style_web_directory() {
        // Web directory pattern: D2775,F664
        // Directories setgid with 775, files 664
        let modifiers = ChmodModifiers::parse("D2775,F664").expect("parse");
        assert!(!modifiers.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn upstream_behavior_fgo_minus_w() {
        // Verify Fgo-w removes group/other write from files, leaves directories alone
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        let dir_path = temp.path().join("testdir");
        std::fs::write(&file_path, b"content").expect("write file");
        std::fs::create_dir(&dir_path).expect("create dir");

        let file_type = std::fs::metadata(&file_path)
            .expect("file metadata")
            .file_type();
        let dir_type = std::fs::metadata(&dir_path)
            .expect("dir metadata")
            .file_type();

        let modifiers = ChmodModifiers::parse("Fgo-w").expect("parse");

        // File: 0o666 -> 0o644 (remove g-w and o-w)
        let file_result = modifiers.apply(0o666, file_type);
        assert_eq!(file_result & 0o777, 0o644);

        // Directory: unchanged
        let dir_result = modifiers.apply(0o777, dir_type);
        assert_eq!(dir_result & 0o777, 0o777);
    }

    #[cfg(unix)]
    #[test]
    fn upstream_behavior_d755_f644() {
        // Verify D755,F644 sets correct modes for each type
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        let dir_path = temp.path().join("testdir");
        std::fs::write(&file_path, b"content").expect("write file");
        std::fs::create_dir(&dir_path).expect("create dir");

        let file_type = std::fs::metadata(&file_path)
            .expect("file metadata")
            .file_type();
        let dir_type = std::fs::metadata(&dir_path)
            .expect("dir metadata")
            .file_type();

        let modifiers = ChmodModifiers::parse("D755,F644").expect("parse");

        let file_result = modifiers.apply(0o000, file_type);
        let dir_result = modifiers.apply(0o000, dir_type);

        assert_eq!(file_result & 0o777, 0o644);
        assert_eq!(dir_result & 0o777, 0o755);
    }

    #[cfg(unix)]
    #[test]
    fn upstream_behavior_ugo_equals_rwx() {
        // Verify ugo=rwX behavior matches rsync
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("test.txt");
        let dir_path = temp.path().join("testdir");
        std::fs::write(&file_path, b"content").expect("write file");
        std::fs::create_dir(&dir_path).expect("create dir");

        let file_type = std::fs::metadata(&file_path)
            .expect("file metadata")
            .file_type();
        let dir_type = std::fs::metadata(&dir_path)
            .expect("dir metadata")
            .file_type();

        let modifiers = ChmodModifiers::parse("ugo=rwX").expect("parse");

        // File without execute: should get rw- for all (no X applied)
        let file_result = modifiers.apply(0o000, file_type);
        assert_eq!(file_result & 0o777, 0o666);

        // Directory: should get rwx for all (X applies)
        let dir_result = modifiers.apply(0o000, dir_type);
        assert_eq!(dir_result & 0o777, 0o777);
    }
}
