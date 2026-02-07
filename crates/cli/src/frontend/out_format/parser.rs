#![deny(unsafe_code)]

//! Parser for command-line supplied `--out-format` specifications.

use std::ffi::OsStr;
use std::iter::Peekable;
use std::str::Chars;

use core::message::{Message, Role};
use core::rsync_error;

use super::tokens::{
    HumanizeMode, OutFormat, OutFormatPlaceholder, OutFormatToken, PlaceholderAlignment,
    PlaceholderFormat, PlaceholderToken, MAX_PLACEHOLDER_WIDTH,
};

fn parse_placeholder_format(chars: &mut Peekable<Chars<'_>>) -> PlaceholderFormat {
    let mut apostrophes = 0usize;
    while matches!(chars.peek(), Some('\'')) {
        apostrophes += 1;
        chars.next();
    }

    let mut align = PlaceholderAlignment::Right;
    if matches!(chars.peek(), Some('-')) {
        align = PlaceholderAlignment::Left;
        chars.next();
    }

    let mut width_value: usize = 0;
    let mut saw_width = false;
    while let Some(peeked) = chars.peek().copied() {
        if let Some(digit) = peeked.to_digit(10) {
            saw_width = true;
            width_value = width_value
                .saturating_mul(10)
                .saturating_add(digit as usize);
            chars.next();
        } else {
            break;
        }
    }

    while matches!(chars.peek(), Some('\'')) {
        apostrophes += 1;
        chars.next();
    }

    let width = if saw_width {
        Some(width_value.min(MAX_PLACEHOLDER_WIDTH))
    } else {
        None
    };

    let humanize = match apostrophes {
        0 => HumanizeMode::None,
        1 => HumanizeMode::Separator,
        2 => HumanizeMode::DecimalUnits,
        _ => HumanizeMode::BinaryUnits,
    };

    PlaceholderFormat::new(width, align, humanize)
}

/// Parses a command-line supplied `--out-format` specification into tokens.
pub(crate) fn parse_out_format(value: &OsStr) -> Result<OutFormat, Message> {
    let text = value.to_string_lossy();
    if text.is_empty() {
        return Err(rsync_error!(1, "--out-format value must not be empty").with_role(Role::Client));
    }

    let mut tokens = Vec::new();
    let mut literal = String::new();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '%' {
            literal.push(ch);
            continue;
        }

        let format_spec = parse_placeholder_format(&mut chars);
        let Some(next) = chars.next() else {
            return Err(
                rsync_error!(1, "--out-format value may not end with '%'").with_role(Role::Client)
            );
        };

        if next == '%' {
            literal.push('%');
            continue;
        }

        if !literal.is_empty() {
            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
        }

        let placeholder = match next {
            'n' => OutFormatPlaceholder::FileName,
            'N' => OutFormatPlaceholder::FileNameWithSymlinkTarget,
            'f' => OutFormatPlaceholder::FullPath,
            'i' => OutFormatPlaceholder::ItemizedChanges,
            'l' => OutFormatPlaceholder::FileLength,
            'b' => OutFormatPlaceholder::BytesTransferred,
            'c' => OutFormatPlaceholder::ChecksumBytes,
            'o' => OutFormatPlaceholder::Operation,
            'M' => OutFormatPlaceholder::ModifyTime,
            'B' => OutFormatPlaceholder::PermissionString,
            'L' => OutFormatPlaceholder::SymlinkTarget,
            't' => OutFormatPlaceholder::CurrentTime,
            'u' => OutFormatPlaceholder::OwnerName,
            'g' => OutFormatPlaceholder::GroupName,
            'U' => OutFormatPlaceholder::OwnerUid,
            'G' => OutFormatPlaceholder::OwnerGid,
            'p' => OutFormatPlaceholder::ProcessId,
            'h' => OutFormatPlaceholder::RemoteHost,
            'a' => OutFormatPlaceholder::RemoteAddress,
            'm' => OutFormatPlaceholder::ModuleName,
            'P' => OutFormatPlaceholder::ModulePath,
            'C' => OutFormatPlaceholder::FullChecksum,
            other => {
                return Err(rsync_error!(
                    1,
                    format!("unsupported --out-format placeholder '%{other}'"),
                )
                .with_role(Role::Client));
            }
        };

        tokens.push(OutFormatToken::Placeholder(PlaceholderToken::new(
            placeholder,
            format_spec,
        )));
    }

    if !literal.is_empty() {
        tokens.push(OutFormatToken::Literal(literal));
    }

    Ok(OutFormat::new(tokens))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn parse_out_format_literal_only() {
        let result = parse_out_format(&os("hello"));
        assert!(result.is_ok());
        assert!(!result.unwrap().is_empty());
    }

    #[test]
    fn parse_out_format_filename() {
        let result = parse_out_format(&os("%n"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_filename_with_target() {
        let result = parse_out_format(&os("%N"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_full_path() {
        let result = parse_out_format(&os("%f"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_itemized() {
        let result = parse_out_format(&os("%i"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_length() {
        let result = parse_out_format(&os("%l"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_bytes_transferred() {
        let result = parse_out_format(&os("%b"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_checksum_bytes() {
        let result = parse_out_format(&os("%c"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_operation() {
        let result = parse_out_format(&os("%o"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_modify_time() {
        let result = parse_out_format(&os("%M"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_permission_string() {
        let result = parse_out_format(&os("%B"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_symlink_target() {
        let result = parse_out_format(&os("%L"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_current_time() {
        let result = parse_out_format(&os("%t"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_owner_name() {
        let result = parse_out_format(&os("%u"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_group_name() {
        let result = parse_out_format(&os("%g"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_owner_uid() {
        let result = parse_out_format(&os("%U"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_owner_gid() {
        let result = parse_out_format(&os("%G"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_process_id() {
        let result = parse_out_format(&os("%p"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_remote_host() {
        let result = parse_out_format(&os("%h"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_remote_address() {
        let result = parse_out_format(&os("%a"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_module_name() {
        let result = parse_out_format(&os("%m"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_module_path() {
        let result = parse_out_format(&os("%P"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_full_checksum() {
        let result = parse_out_format(&os("%C"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_escaped_percent() {
        let result = parse_out_format(&os("%%"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_mixed() {
        let result = parse_out_format(&os("[%n] %l bytes"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_with_width() {
        let result = parse_out_format(&os("%10n"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_left_align() {
        let result = parse_out_format(&os("%-10n"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_separator_mode() {
        let result = parse_out_format(&os("%'l"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_decimal_units() {
        let result = parse_out_format(&os("%''l"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_binary_units() {
        let result = parse_out_format(&os("%'''l"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_empty_error() {
        let result = parse_out_format(&os(""));
        assert!(result.is_err());
    }

    #[test]
    fn parse_out_format_trailing_percent_error() {
        let result = parse_out_format(&os("hello%"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_out_format_unsupported_placeholder_error() {
        let result = parse_out_format(&os("%z"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_placeholder_format_no_modifiers() {
        let mut chars = "n".chars().peekable();
        let format = parse_placeholder_format(&mut chars);
        assert_eq!(format.width(), None);
        assert_eq!(format.align(), PlaceholderAlignment::Right);
        assert_eq!(format.humanize(), HumanizeMode::None);
    }

    #[test]
    fn parse_placeholder_format_width_only() {
        let mut chars = "15n".chars().peekable();
        let format = parse_placeholder_format(&mut chars);
        assert_eq!(format.width(), Some(15));
    }

    #[test]
    fn parse_placeholder_format_left_align_and_width() {
        let mut chars = "-20n".chars().peekable();
        let format = parse_placeholder_format(&mut chars);
        assert_eq!(format.width(), Some(20));
        assert_eq!(format.align(), PlaceholderAlignment::Left);
    }

    #[test]
    fn parse_placeholder_format_apostrophe_before_width() {
        let mut chars = "'10n".chars().peekable();
        let format = parse_placeholder_format(&mut chars);
        assert_eq!(format.width(), Some(10));
        assert_eq!(format.humanize(), HumanizeMode::Separator);
    }

    #[test]
    fn parse_placeholder_format_apostrophe_after_width() {
        let mut chars = "10'n".chars().peekable();
        let format = parse_placeholder_format(&mut chars);
        assert_eq!(format.width(), Some(10));
        assert_eq!(format.humanize(), HumanizeMode::Separator);
    }

    #[test]
    fn parse_placeholder_format_width_clamped() {
        let mut chars = "99999n".chars().peekable();
        let format = parse_placeholder_format(&mut chars);
        // Width should be clamped to MAX_PLACEHOLDER_WIDTH
        assert!(format.width().unwrap() <= MAX_PLACEHOLDER_WIDTH);
    }

    #[test]
    fn parse_placeholder_format_zero_width() {
        let mut chars = "0n".chars().peekable();
        let format = parse_placeholder_format(&mut chars);
        assert_eq!(format.width(), Some(0));
    }

    #[test]
    fn parse_placeholder_format_two_apostrophes() {
        let mut chars = "''n".chars().peekable();
        let format = parse_placeholder_format(&mut chars);
        assert_eq!(format.humanize(), HumanizeMode::DecimalUnits);
    }

    #[test]
    fn parse_placeholder_format_three_apostrophes() {
        let mut chars = "'''n".chars().peekable();
        let format = parse_placeholder_format(&mut chars);
        assert_eq!(format.humanize(), HumanizeMode::BinaryUnits);
    }

    #[test]
    fn parse_placeholder_format_four_apostrophes() {
        // 4+ apostrophes should still be BinaryUnits
        let mut chars = "''''n".chars().peekable();
        let format = parse_placeholder_format(&mut chars);
        assert_eq!(format.humanize(), HumanizeMode::BinaryUnits);
    }

    #[test]
    fn parse_out_format_all_placeholders_combined() {
        let result = parse_out_format(&os(
            "%n %N %f %i %l %b %c %o %M %B %L %t %u %g %U %G %p %h %a %m %P %C",
        ));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_out_format_multiple_escaped_percent() {
        let result = parse_out_format(&os("%% %% %%"));
        assert!(result.is_ok());
    }

    // ---- Token content verification tests ----
    //
    // These tests verify that each placeholder letter maps to the correct
    // OutFormatPlaceholder variant, not just that parsing succeeds.

    fn assert_single_placeholder(input: &str, expected: OutFormatPlaceholder) {
        let format = parse_out_format(&os(input)).unwrap_or_else(|_| panic!("parse {input}"));
        let tokens: Vec<_> = format.tokens().collect();
        assert_eq!(tokens.len(), 1, "expected 1 token for {input}");
        match &tokens[0] {
            OutFormatToken::Placeholder(p) => assert!(
                std::mem::discriminant(&p.kind) == std::mem::discriminant(&expected),
                "for {input}, expected {expected:?}, got {:?}",
                p.kind,
            ),
            other => panic!("expected placeholder for {input}, got {other:?}"),
        }
    }

    #[test]
    fn parse_token_content_filename() {
        assert_single_placeholder("%n", OutFormatPlaceholder::FileName);
    }

    #[test]
    fn parse_token_content_filename_with_symlink_target() {
        assert_single_placeholder("%N", OutFormatPlaceholder::FileNameWithSymlinkTarget);
    }

    #[test]
    fn parse_token_content_full_path() {
        assert_single_placeholder("%f", OutFormatPlaceholder::FullPath);
    }

    #[test]
    fn parse_token_content_itemized_changes() {
        assert_single_placeholder("%i", OutFormatPlaceholder::ItemizedChanges);
    }

    #[test]
    fn parse_token_content_file_length() {
        assert_single_placeholder("%l", OutFormatPlaceholder::FileLength);
    }

    #[test]
    fn parse_token_content_bytes_transferred() {
        assert_single_placeholder("%b", OutFormatPlaceholder::BytesTransferred);
    }

    #[test]
    fn parse_token_content_checksum_bytes() {
        assert_single_placeholder("%c", OutFormatPlaceholder::ChecksumBytes);
    }

    #[test]
    fn parse_token_content_operation() {
        assert_single_placeholder("%o", OutFormatPlaceholder::Operation);
    }

    #[test]
    fn parse_token_content_modify_time() {
        assert_single_placeholder("%M", OutFormatPlaceholder::ModifyTime);
    }

    #[test]
    fn parse_token_content_permission_string() {
        assert_single_placeholder("%B", OutFormatPlaceholder::PermissionString);
    }

    #[test]
    fn parse_token_content_symlink_target() {
        assert_single_placeholder("%L", OutFormatPlaceholder::SymlinkTarget);
    }

    #[test]
    fn parse_token_content_current_time() {
        assert_single_placeholder("%t", OutFormatPlaceholder::CurrentTime);
    }

    #[test]
    fn parse_token_content_owner_name() {
        assert_single_placeholder("%u", OutFormatPlaceholder::OwnerName);
    }

    #[test]
    fn parse_token_content_group_name() {
        assert_single_placeholder("%g", OutFormatPlaceholder::GroupName);
    }

    #[test]
    fn parse_token_content_owner_uid() {
        assert_single_placeholder("%U", OutFormatPlaceholder::OwnerUid);
    }

    #[test]
    fn parse_token_content_owner_gid() {
        assert_single_placeholder("%G", OutFormatPlaceholder::OwnerGid);
    }

    #[test]
    fn parse_token_content_process_id() {
        assert_single_placeholder("%p", OutFormatPlaceholder::ProcessId);
    }

    #[test]
    fn parse_token_content_remote_host() {
        assert_single_placeholder("%h", OutFormatPlaceholder::RemoteHost);
    }

    #[test]
    fn parse_token_content_remote_address() {
        assert_single_placeholder("%a", OutFormatPlaceholder::RemoteAddress);
    }

    #[test]
    fn parse_token_content_module_name() {
        assert_single_placeholder("%m", OutFormatPlaceholder::ModuleName);
    }

    #[test]
    fn parse_token_content_module_path() {
        assert_single_placeholder("%P", OutFormatPlaceholder::ModulePath);
    }

    #[test]
    fn parse_token_content_full_checksum() {
        assert_single_placeholder("%C", OutFormatPlaceholder::FullChecksum);
    }

    // ---- Escaped percent produces a literal token ----

    #[test]
    fn parse_escaped_percent_produces_literal_percent_token() {
        let format = parse_out_format(&os("%%")).unwrap();
        let tokens: Vec<_> = format.tokens().collect();
        assert_eq!(tokens.len(), 1);
        match &tokens[0] {
            OutFormatToken::Literal(s) => assert_eq!(s, "%"),
            other => panic!("expected literal '%', got {other:?}"),
        }
    }

    #[test]
    fn parse_double_escaped_percent_produces_two_percent_literals() {
        let format = parse_out_format(&os("%%%%")).unwrap();
        let tokens: Vec<_> = format.tokens().collect();
        // Both %% sequences are adjacent so they get merged into one literal "%%"
        assert_eq!(tokens.len(), 1);
        match &tokens[0] {
            OutFormatToken::Literal(s) => assert_eq!(s, "%%"),
            other => panic!("expected literal '%%', got {other:?}"),
        }
    }

    // ---- Token ordering with mixed literals and placeholders ----

    #[test]
    fn parse_mixed_format_preserves_token_order() {
        let format = parse_out_format(&os("[%i] %n (%l bytes)")).unwrap();
        let tokens: Vec<_> = format.tokens().collect();
        // Expected: Literal("["), Placeholder(%i), Literal("] "), Placeholder(%n),
        //           Literal(" ("), Placeholder(%l), Literal(" bytes)")
        assert_eq!(tokens.len(), 7, "expected 7 tokens, got {tokens:?}");

        assert!(matches!(&tokens[0], OutFormatToken::Literal(s) if s == "["));
        assert!(
            matches!(&tokens[1], OutFormatToken::Placeholder(p) if matches!(p.kind, OutFormatPlaceholder::ItemizedChanges))
        );
        assert!(matches!(&tokens[2], OutFormatToken::Literal(s) if s == "] "));
        assert!(
            matches!(&tokens[3], OutFormatToken::Placeholder(p) if matches!(p.kind, OutFormatPlaceholder::FileName))
        );
        assert!(matches!(&tokens[4], OutFormatToken::Literal(s) if s == " ("));
        assert!(
            matches!(&tokens[5], OutFormatToken::Placeholder(p) if matches!(p.kind, OutFormatPlaceholder::FileLength))
        );
        assert!(matches!(&tokens[6], OutFormatToken::Literal(s) if s == " bytes)"));
    }

    #[test]
    fn parse_adjacent_placeholders_without_separator() {
        let format = parse_out_format(&os("%i%n")).unwrap();
        let tokens: Vec<_> = format.tokens().collect();
        assert_eq!(tokens.len(), 2, "expected 2 tokens for %%i%%n");
        assert!(
            matches!(&tokens[0], OutFormatToken::Placeholder(p) if matches!(p.kind, OutFormatPlaceholder::ItemizedChanges))
        );
        assert!(
            matches!(&tokens[1], OutFormatToken::Placeholder(p) if matches!(p.kind, OutFormatPlaceholder::FileName))
        );
    }

    // ---- Apostrophe before and after width combined ----

    #[test]
    fn parse_placeholder_format_apostrophe_before_and_after_width() {
        let mut chars = "'10'n".chars().peekable();
        let format = parse_placeholder_format(&mut chars);
        assert_eq!(format.width(), Some(10));
        // 1 apostrophe before + 1 after = 2 total = DecimalUnits
        assert_eq!(format.humanize(), HumanizeMode::DecimalUnits);
    }

    #[test]
    fn parse_placeholder_format_two_before_one_after_width() {
        let mut chars = "''10'n".chars().peekable();
        let format = parse_placeholder_format(&mut chars);
        assert_eq!(format.width(), Some(10));
        // 2 apostrophes before + 1 after = 3 total = BinaryUnits
        assert_eq!(format.humanize(), HumanizeMode::BinaryUnits);
    }

    // ---- Unsupported placeholder error messages ----

    #[test]
    fn parse_out_format_unsupported_placeholder_includes_letter_in_error() {
        let err = parse_out_format(&os("%z")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("%z"), "error should mention '%z', got: {msg}",);
    }

    #[test]
    fn parse_out_format_unsupported_uppercase_placeholder_includes_letter() {
        let err = parse_out_format(&os("%Q")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("%Q"), "error should mention '%Q', got: {msg}",);
    }

    // ---- Literal-only input produces single literal token ----

    #[test]
    fn parse_literal_only_input_produces_single_literal_token() {
        let format = parse_out_format(&os("hello world")).unwrap();
        let tokens: Vec<_> = format.tokens().collect();
        assert_eq!(tokens.len(), 1);
        match &tokens[0] {
            OutFormatToken::Literal(s) => assert_eq!(s, "hello world"),
            other => panic!("expected literal, got {other:?}"),
        }
    }

    // ---- Placeholder with format metadata ----

    #[test]
    fn parse_width_format_attached_to_correct_placeholder() {
        let format = parse_out_format(&os("%-20n")).unwrap();
        let tokens: Vec<_> = format.tokens().collect();
        assert_eq!(tokens.len(), 1);
        match &tokens[0] {
            OutFormatToken::Placeholder(p) => {
                assert!(matches!(p.kind, OutFormatPlaceholder::FileName));
                assert_eq!(p.format.width(), Some(20));
                assert_eq!(p.format.align(), PlaceholderAlignment::Left);
            }
            other => panic!("expected placeholder, got {other:?}"),
        }
    }
}
