use std::io::{self, BufRead};

pub(super) fn read_trimmed_line<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut line = String::new();
    let bytes = reader.read_line(&mut line)?;

    if bytes == 0 {
        return Ok(None);
    }

    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }

    Ok(Some(line))
}

pub(super) fn strip_prefix_ignore_ascii_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if text.len() < prefix.len() {
        return None;
    }

    let (candidate, remainder) = text.split_at(prefix.len());
    if candidate.eq_ignore_ascii_case(prefix) {
        Some(remainder)
    } else {
        None
    }
}
