use std::ffi::OsString;

/// Joins multiple `OsString` values with commas into a single `OsString`.
///
/// Returns `None` if the iterator is empty, or the single value if there is
/// exactly one. Multiple values are comma-separated so the downstream mapping
/// parser (`NameMapping::parse`) sees them as additional rules.
pub(crate) fn join_os_values(values: Option<impl Iterator<Item = OsString>>) -> Option<OsString> {
    let mut iter = values?.peekable();
    let first = iter.next()?;
    if iter.peek().is_none() {
        return Some(first);
    }
    let mut joined = first;
    for value in iter {
        joined.push(",");
        joined.push(&value);
    }
    Some(joined)
}
