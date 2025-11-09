#![deny(unsafe_code)]

//! Parser for command-line supplied `--out-format` specifications.

use std::ffi::OsStr;
use std::iter::Peekable;
use std::str::Chars;

use rsync_core::message::{Message, Role};
use rsync_core::rsync_error;

use super::tokens::{
    HumanizeMode, MAX_PLACEHOLDER_WIDTH, OutFormat, OutFormatPlaceholder, OutFormatToken,
    PlaceholderAlignment, PlaceholderFormat, PlaceholderToken,
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
