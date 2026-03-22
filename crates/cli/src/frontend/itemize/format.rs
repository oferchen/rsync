//! Formatting logic for the 11-character rsync itemize string.
//!
//! Implements the `YXcstpoguax` output format matching upstream `log.c`.
//! Handles new-file (`+`), missing-data (`?`), attribute flags, and the
//! trailing-dot-to-space collapse for unchanged entries.

use super::change::ItemizeChange;

/// Formats an itemized change as an 11-character rsync string.
///
/// This is the core formatting function that produces strings like:
/// - `>f+++++++++` - new file received
/// - `.f         ` - unchanged file (all-dots collapsed to spaces)
/// - `>fcst......` - file with checksum, size, and time changes
///
/// # Examples
///
/// ```
/// use cli::{ItemizeChange, UpdateType, FileType, format_itemize};
///
/// let change = ItemizeChange::new()
///     .with_update_type(UpdateType::Received)
///     .with_file_type(FileType::Directory)
///     .with_new_file(true);
///
/// assert_eq!(format_itemize(&change), ">d+++++++++");
/// ```
#[must_use]
pub fn format_itemize(change: &ItemizeChange) -> String {
    let mut result = String::with_capacity(11);

    let update_char = change.update_type.as_char();

    // Position 0: Update type
    result.push(update_char);

    // Position 1: File type
    result.push(change.file_type.as_char());

    // Positions 2-10: Attributes
    // upstream: log.c:730-734 - ITEM_IS_NEW fills with '+', ITEM_MISSING_DATA fills with '?'
    if change.new_file {
        result.push_str("+++++++++");
    } else if change.missing_data {
        result.push_str("?????????");
    } else {
        // Position 2: Checksum
        result.push(if change.checksum_changed { 'c' } else { '.' });

        // Position 3: Size
        // upstream: log.c:705-707 - symlinks never report size changes
        let is_symlink = matches!(change.file_type, super::types::FileType::Symlink);
        result.push(if !is_symlink && change.size_changed {
            's'
        } else {
            '.'
        });

        // Position 4: Time (T takes precedence over t)
        result.push(if change.time_set_to_transfer {
            'T'
        } else if change.time_changed {
            't'
        } else {
            '.'
        });

        // Position 5: Permissions
        result.push(if change.perms_changed { 'p' } else { '.' });

        // Position 6: Owner
        result.push(if change.owner_changed { 'o' } else { '.' });

        // Position 7: Group
        result.push(if change.group_changed { 'g' } else { '.' });

        // Position 8: Access/Create time (both, atime only, ctime only, or neither)
        result.push(match (change.atime_changed, change.ctime_changed) {
            (true, true) => 'b',
            (true, false) => 'u',
            (false, true) => 'n',
            (false, false) => '.',
        });

        // Position 9: ACL
        result.push(if change.acl_changed { 'a' } else { '.' });

        // Position 10: Extended attributes
        result.push(if change.xattr_changed { 'x' } else { '.' });

        // upstream: log.c:735-744 - when update type is '.', 'h', or 'c' and all
        // rendered attribute positions (2-10) are dots, collapse them to spaces.
        // We check the rendered chars, not the raw flags, because symlink size
        // suppression may produce all-dot attributes even when size_changed is set.
        if matches!(update_char, '.' | 'h' | 'c')
            && result.as_bytes()[2..].iter().all(|&b| b == b'.')
        {
            result.replace_range(2.., "         ");
        }
    }

    result
}
