#![deny(unsafe_code)]

//! Parser for command-line supplied `--out-format` specifications.

use std::ffi::OsStr;

use rsync_core::message::{Message, Role};
use rsync_core::rsync_error;

use super::tokens::{OutFormat, OutFormatPlaceholder, OutFormatToken};

/// Parses a command-line supplied `--out-format` specification into tokens.
pub(crate) fn parse_out_format(value: &OsStr) -> Result<OutFormat, Message> {
    let text = value.to_string_lossy();
    if text.is_empty() {
        return Err(rsync_error!(1, "--out-format value must not be empty").with_role(Role::Client));
    }

    let mut tokens = Vec::new();
    let mut literal = String::new();
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '%' => {
                let Some(next) = chars.next() else {
                    return Err(rsync_error!(1, "--out-format value may not end with '%'")
                        .with_role(Role::Client));
                };
                match next {
                    '%' => literal.push('%'),
                    'n' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::FileName));
                    }
                    'N' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::FileNameWithSymlinkTarget,
                        ));
                    }
                    'f' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::FullPath));
                    }
                    'i' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::ItemizedChanges,
                        ));
                    }
                    'l' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::FileLength,
                        ));
                    }
                    'b' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::BytesTransferred,
                        ));
                    }
                    'c' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::ChecksumBytes,
                        ));
                    }
                    'o' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::Operation));
                    }
                    'M' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::ModifyTime,
                        ));
                    }
                    'B' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::PermissionString,
                        ));
                    }
                    'L' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::SymlinkTarget,
                        ));
                    }
                    't' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::CurrentTime,
                        ));
                    }
                    'u' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::OwnerName));
                    }
                    'g' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::GroupName));
                    }
                    'U' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::OwnerUid));
                    }
                    'G' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::OwnerGid));
                    }
                    'p' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(OutFormatPlaceholder::ProcessId));
                    }
                    'h' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::RemoteHost,
                        ));
                    }
                    'a' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::RemoteAddress,
                        ));
                    }
                    'm' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::ModuleName,
                        ));
                    }
                    'P' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::ModulePath,
                        ));
                    }
                    'C' => {
                        if !literal.is_empty() {
                            tokens.push(OutFormatToken::Literal(std::mem::take(&mut literal)));
                        }
                        tokens.push(OutFormatToken::Placeholder(
                            OutFormatPlaceholder::FullChecksum,
                        ));
                    }
                    other => {
                        return Err(rsync_error!(
                            1,
                            format!("unsupported --out-format placeholder '%{other}'"),
                        )
                        .with_role(Role::Client));
                    }
                }
            }
            _ => literal.push(ch),
        }
    }

    if !literal.is_empty() {
        tokens.push(OutFormatToken::Literal(literal));
    }

    Ok(OutFormat::new(tokens))
}
