/// Formats a number with thousands separators (commas).
///
/// Matches the formatting used by upstream rsync for file sizes and counts.
///
/// # Examples
///
/// ```
/// use cli::format_number_with_commas;
///
/// assert_eq!(format_number_with_commas(0), "0");
/// assert_eq!(format_number_with_commas(1234), "1,234");
/// assert_eq!(format_number_with_commas(1234567), "1,234,567");
/// ```
#[must_use]
pub fn format_number_with_commas(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let len = bytes.len();

    if len <= 3 {
        return s;
    }

    let mut result = String::with_capacity(len + (len - 1) / 3);
    let first_group_len = len % 3;

    if first_group_len > 0 {
        result.push_str(&s[..first_group_len]);
        if len > first_group_len {
            result.push(',');
        }
    }

    let mut i = first_group_len;
    while i < len {
        if i > first_group_len && i > 0 {
            result.push(',');
        }
        result.push_str(&s[i..i + 3]);
        i += 3;
    }

    result
}
