//! Name converter trait and thread-local storage.
//!
//! Provides the `NameConverterCallbacks` abstraction for daemon environments
//! where NSS lookups are unavailable (e.g., inside a chroot). A converter
//! installed via `set_name_converter` intercepts all UID/GID name resolution
//! calls on the current thread.
//!
//! upstream: uidlist.c:110-193 - the name converter subprocess replaces
//! getpwuid/getpwnam/getgrgid/getgrnam calls.

use super::error::LookupError;
use std::cell::RefCell;
use std::io;

/// What one name-converter query produced.
///
/// Upstream's `namecvt_call()` returns a bare `BOOL` (clientserver.c:1311) only
/// because two of its outcomes never return at all: an over-long request
/// (:1324-1327) and a failed write (:1329-1334) each call `exit_cleanup()`. The
/// two that do return - a refused token (:1317-1319) and an answer the callers
/// cannot use (:1345-1354) - are the only ones `False` ever means. Reproducing
/// that as a single `Option` in Rust would merge a converter that has *died*
/// with a name it simply does not know, and "does not know" is the arm callers
/// are entitled to shrug off.
#[derive(Debug)]
pub enum ConverterOutcome<T> {
    /// The converter answered, and the answer is usable.
    ///
    /// upstream: clientserver.c:1355-1359 - `*id_p`/`*name_p` is set and the
    /// call returns `True`.
    Resolved(T),

    /// The converter answered, and its answer names nothing: an empty line, or
    /// (name to id) a line that is not a plain decimal id that fits.
    ///
    /// upstream: clientserver.c:1345-1354 - `False` with the caller's id left
    /// untouched. CVE-2026-53798 is precisely this arm being read as `atol("")
    /// == 0` and mapping an unknown name onto root.
    Unknown,

    /// The request was never written, because the name cannot be framed as one
    /// request line. The converter itself is untouched and still usable.
    ///
    /// upstream: clientserver.c:1317-1319 `namecvt_safe_token()` - the token is
    /// reported and the call returns `False` without writing anything
    /// (CVE-2026-53788).
    Refused,

    /// The query could not be completed. The session must not continue as
    /// though the name were merely unknown.
    ///
    /// upstream: clientserver.c:1324-1327 (`RERR_UNSUPPORTED`) and :1329-1334
    /// (`RERR_SOCKETIO`) exit the process; a converter that has closed its
    /// answer pipe (:1336-1337) exits on the next request's write.
    Failed(io::Error),
}

impl<T> ConverterOutcome<T> {
    /// Rewrites the answered value, turning one the caller cannot use into
    /// [`ConverterOutcome::Unknown`].
    ///
    /// upstream: clientserver.c:1347-1354 - a malformed id answer is `False`,
    /// the same result an empty answer produces; it is not a transport failure.
    pub fn map_answer<U>(self, map: impl FnOnce(T) -> Option<U>) -> ConverterOutcome<U> {
        match self {
            Self::Resolved(value) => {
                map(value).map_or(ConverterOutcome::Unknown, ConverterOutcome::Resolved)
            }
            Self::Unknown => ConverterOutcome::Unknown,
            Self::Refused => ConverterOutcome::Refused,
            Self::Failed(err) => ConverterOutcome::Failed(err),
        }
    }

    /// Collapses the outcome into the lookup API's result.
    ///
    /// The two arms upstream returns `False` for become `Ok(None)` ("no such
    /// name"); the arm upstream exits on becomes an error, never `Ok(None)`.
    pub(super) fn into_lookup(self) -> Result<Option<T>, LookupError> {
        match self {
            Self::Resolved(value) => Ok(Some(value)),
            Self::Unknown | Self::Refused => Ok(None),
            Self::Failed(err) => Err(LookupError::Converter(err)),
        }
    }
}

impl<T> From<Option<T>> for ConverterOutcome<T> {
    /// Lifts an answer from a back end that has no failure mode of its own -
    /// one that can only say "here it is" or "no such account".
    fn from(answer: Option<T>) -> Self {
        answer.map_or(Self::Unknown, Self::Resolved)
    }
}

/// External name-to-ID and ID-to-name conversion.
///
/// Used by the daemon's `name converter` parameter to provide uid/gid mapping
/// in chroot environments where NSS lookups are unavailable.
///
/// upstream: uidlist.c:110-193
pub trait NameConverterCallbacks: Send {
    /// Converts a numeric UID to a username.
    fn uid_to_name(&mut self, uid: u32) -> ConverterOutcome<String>;
    /// Converts a numeric GID to a group name.
    fn gid_to_name(&mut self, gid: u32) -> ConverterOutcome<String>;
    /// Converts a username to a numeric UID.
    fn name_to_uid(&mut self, name: &str) -> ConverterOutcome<u32>;
    /// Converts a group name to a numeric GID.
    fn name_to_gid(&mut self, name: &str) -> ConverterOutcome<u32>;
}

/// Runs `query` against the installed name converter, if there is one.
///
/// `Some(outcome)` means a converter is installed and `outcome` is its verdict -
/// including the arms that say "I do not know this name" and "I could not
/// answer at all". `None` means no converter is installed, and only then may
/// the caller consult the host user database.
///
/// upstream: uidlist.c:114-121, :131-138, :154-163, :180-189 - every lookup is
/// `if (namecvt_pid) { namecvt_call(...) } else { getpw*()/getgr*() }`. The
/// converter exists to isolate a session from the host database: a chrooted
/// daemon module resolves names through the operator's script, not through
/// `/etc/passwd`. Falling back to the host on "unknown" would defeat the reason
/// the directive is configured, so the two arms are mutually exclusive and this
/// helper is the single place that says so.
pub(super) fn with_name_converter<T>(
    query: impl FnOnce(&mut dyn NameConverterCallbacks) -> ConverterOutcome<T>,
) -> Option<ConverterOutcome<T>> {
    NAME_CONVERTER_SLOT.with(|slot| slot.borrow_mut().as_mut().map(|nc| query(&mut **nc)))
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

/// Reports whether a name converter is installed on the current thread.
///
/// The process-wide name memo bypasses caching when this returns `true`,
/// because converter results are thread-local and must not be shared across
/// threads.
pub(super) fn has_name_converter() -> bool {
    NAME_CONVERTER_SLOT.with(|slot| slot.borrow().is_some())
}
