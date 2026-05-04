//! Name converter trait and thread-local storage.
//!
//! Provides the `NameConverterCallbacks` abstraction for daemon environments
//! where NSS lookups are unavailable (e.g., inside a chroot). A converter
//! installed via `set_name_converter` intercepts all UID/GID name resolution
//! calls on the current thread.
//!
//! upstream: uidlist.c:110-193 - the name converter subprocess replaces
//! getpwuid/getpwnam/getgrgid/getgrnam calls.

use std::cell::RefCell;

/// External name-to-ID and ID-to-name conversion.
///
/// Used by the daemon's `name converter` parameter to provide uid/gid mapping
/// in chroot environments where NSS lookups are unavailable.
///
/// upstream: uidlist.c:110-193
pub trait NameConverterCallbacks: Send {
    /// Converts a numeric UID to a username.
    fn uid_to_name(&mut self, uid: u32) -> Option<String>;
    /// Converts a numeric GID to a group name.
    fn gid_to_name(&mut self, gid: u32) -> Option<String>;
    /// Converts a username to a numeric UID.
    fn name_to_uid(&mut self, name: &str) -> Option<u32>;
    /// Converts a group name to a numeric GID.
    fn name_to_gid(&mut self, name: &str) -> Option<u32>;
}

thread_local! {
    pub(super) static NAME_CONVERTER_SLOT: RefCell<Option<Box<dyn NameConverterCallbacks>>> =
        const { RefCell::new(None) };
}

/// Installs a name converter for the current thread.
///
/// When set, the four lookup functions (`lookup_user_name`, `lookup_user_by_name`,
/// `lookup_group_name`, `lookup_group_by_name`) delegate to this converter
/// instead of performing NSS queries.
pub fn set_name_converter(converter: Box<dyn NameConverterCallbacks>) {
    NAME_CONVERTER_SLOT.with(|slot| {
        *slot.borrow_mut() = Some(converter);
    });
}

/// Removes the name converter for the current thread, restoring NSS lookups.
pub fn clear_name_converter() {
    NAME_CONVERTER_SLOT.with(|slot| {
        *slot.borrow_mut() = None;
    });
}
