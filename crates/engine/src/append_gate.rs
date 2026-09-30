//! The `--append` size gate shared by the local-copy executor and the network
//! receiver.
//!
//! upstream: generator.c:2261 recv_generator() - once the quick check has
//! failed, `if (append_mode > 0 && sx.st.st_size >= F_LENGTH(file)) goto
//! cleanup;` skips the file outright: no data is requested and no attribute is
//! applied. The test is reached only for a destination that survived the
//! non-regular obstacle removal at generator.c:2149, i.e. a regular file.
//! A phase-2 redo negates `append_mode` (generator.c:2658), so the gate never
//! applies to a redo pass.

/// Reports whether `--append` skips a file whose existing destination is
/// `dest_len` bytes long and whose source is `source_len` bytes long.
///
/// The destination is left untouched when it is already at least as long as
/// the source, including the equal-length case and an empty source over an
/// empty destination.
#[must_use]
pub const fn append_skips(dest_len: u64, source_len: u64) -> bool {
    dest_len >= source_len
}

#[cfg(test)]
mod tests {
    use super::append_skips;

    /// upstream: generator.c:2261 uses `>=`, so an equal-length destination is
    /// skipped rather than re-sent from its end.
    #[test]
    fn equal_length_destination_is_skipped() {
        assert!(append_skips(4, 4));
        assert!(append_skips(0, 0));
    }

    #[test]
    fn longer_destination_is_skipped() {
        assert!(append_skips(8, 2));
    }

    #[test]
    fn shorter_destination_is_appended() {
        assert!(!append_skips(4, 8));
        assert!(!append_skips(0, 1));
    }
}
