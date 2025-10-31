use super::{
    modifiers::{parse_merge_modifiers, split_short_rule_modifiers},
    types::{FilterParseError, ParsedFilterDirective},
};
use std::path::PathBuf;

pub(super) fn parse_merge_directive(
    text: &str,
) -> Result<Option<ParsedFilterDirective>, FilterParseError> {
    const MERGE_PREFIX: &str = "merge";

    if text.len() < MERGE_PREFIX.len() {
        return Ok(None);
    }

    let (prefix, rest) = text.split_at(MERGE_PREFIX.len());
    if !prefix.eq_ignore_ascii_case(MERGE_PREFIX) {
        return Ok(None);
    }

    let mut remainder = rest.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    let mut modifiers = "";
    if let Some(next) = remainder.strip_prefix(',') {
        let mut split = next.splitn(2, |ch: char| ch.is_ascii_whitespace() || ch == '_');
        modifiers = split.next().unwrap_or("");
        remainder = split
            .next()
            .unwrap_or("")
            .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    }

    let (options, assume_cvsignore) = parse_merge_modifiers(modifiers, text, false)?;

    if remainder == "-" {
        return Err(FilterParseError::new(
            "merge from standard input is not supported in .rsync-filter files",
        ));
    }

    let path_text = remainder.trim_end();
    let path_text = if path_text.is_empty() {
        if assume_cvsignore {
            ".cvsignore"
        } else {
            return Err(FilterParseError::new(
                "merge directive requires a file path",
            ));
        }
    } else {
        path_text
    };

    let options = if modifiers.is_empty() && !assume_cvsignore {
        None
    } else {
        Some(options)
    };

    Ok(Some(ParsedFilterDirective::Merge {
        path: PathBuf::from(path_text),
        options,
    }))
}

pub(super) fn parse_short_merge_directive_line(
    text: &str,
) -> Result<Option<ParsedFilterDirective>, FilterParseError> {
    let mut chars = text.chars();
    let first = match chars.next() {
        Some(first) => first,
        None => return Ok(None),
    };

    let allow_extended = match first {
        '.' => false,
        ':' => true,
        _ => return Ok(None),
    };

    let remainder = chars.as_str();
    let (modifiers, rest) = split_short_rule_modifiers(remainder);
    let (options, assume_cvsignore) = parse_merge_modifiers(modifiers, text, allow_extended)?;

    let pattern = rest.trim();
    let pattern = if pattern.is_empty() {
        if assume_cvsignore {
            ".cvsignore"
        } else if allow_extended {
            return Err(FilterParseError::new(format!(
                "dir-merge directive '{}' is missing a file name",
                text
            )));
        } else {
            return Err(FilterParseError::new(format!(
                "merge directive '{}' is missing a file path",
                text
            )));
        }
    } else {
        pattern
    };

    if allow_extended {
        return Ok(Some(ParsedFilterDirective::Merge {
            path: PathBuf::from(pattern),
            options: Some(options),
        }));
    }

    let options = if modifiers.is_empty() && !assume_cvsignore {
        None
    } else {
        Some(options)
    };

    Ok(Some(ParsedFilterDirective::Merge {
        path: PathBuf::from(pattern),
        options,
    }))
}
