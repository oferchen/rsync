//! The two option sets of a local copy.
//!
//! upstream: pipe.c:109-148 local_child() forks the receiver, which re-runs
//! parse_arguments() over the `-M` values on top of the inherited options. The
//! sender keeps the client's own, so every `-M` option changes the receiver
//! half alone.

use std::sync::Arc;

use super::LocalCopyOptions;

impl LocalCopyOptions {
    /// Gives the sender half its own option set; `self` stays the receiver's.
    #[must_use]
    pub fn with_local_sender(mut self, sender: LocalCopyOptions) -> Self {
        self.sender = Some(Arc::new(sender));
        self
    }

    /// The option set the sender half reads: its own when `-M` gave the
    /// receiver a different one, otherwise `self`.
    #[must_use]
    pub fn sender_half(&self) -> &LocalCopyOptions {
        self.sender.as_deref().unwrap_or(self)
    }

    /// `--fake-super` as each half reads it.
    #[cfg(all(any(unix, windows), feature = "acl"))]
    #[must_use]
    pub(crate) fn fake_super_sides(&self) -> crate::local_copy::FakeSuperSides {
        crate::local_copy::FakeSuperSides {
            sender: self.sender_half().fake_super_enabled(),
            receiver: self.fake_super_enabled(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_local_sender_both_halves_read_one_set() {
        let options = LocalCopyOptions::new().fake_super(true);
        assert!(std::ptr::eq(options.sender_half(), &options));
    }

    #[test]
    fn a_local_sender_keeps_its_own_set() {
        let options = LocalCopyOptions::new()
            .fake_super(true)
            .with_local_sender(LocalCopyOptions::new());
        assert!(options.fake_super_enabled());
        assert!(!options.sender_half().fake_super_enabled());
    }
}
