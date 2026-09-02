//! The error half of a uid/gid lookup.
//!
//! A lookup has two back ends - the host user database and, when a daemon
//! module configures `name converter`, an operator-supplied subprocess - and
//! upstream treats their failures differently. `getpwnam()` returning nothing
//! (for any reason) leaves the sender's numeric id in place (uidlist.c:160-163),
//! whereas a name-converter transport failure ends the session outright
//! (clientserver.c:1326, :1333 `exit_cleanup()`). Collapsing both into
//! `Ok(None)` is what lets a dead converter read as "this name has no id",
//! which is the fail-open shape [`LookupError`] exists to prevent.

use std::error::Error;
use std::fmt;
use std::io;

/// A uid/gid lookup that yields `Ok(None)` when the back end answered "no such
/// name/id" and [`LookupError`] when it could not answer at all.
pub type LookupResult<T> = Result<Option<T>, LookupError>;

/// Why a uid/gid lookup could not be answered.
///
/// The variant is the discriminator a caller needs to decide between upstream's
/// two failure policies, so it must survive any conversion; [`From<LookupError>
/// for io::Error`](#impl-From<LookupError>-for-Error) keeps the value itself as
/// the error's payload rather than flattening it to a message.
#[derive(Debug)]
pub enum LookupError {
    /// The host user database call failed.
    ///
    /// upstream: uidlist.c:160-163 `user_to_uid()` - a `getpwnam()` that yields
    /// no entry is "no id"; the caller keeps the sender's numeric id. Upstream
    /// cannot distinguish "absent" from "the NSS backend errored" and neither
    /// arm ends the transfer, so this variant is tolerated the same way.
    Database(io::Error),

    /// The daemon name converter could not answer.
    ///
    /// upstream: clientserver.c:1324-1327 (request too large,
    /// `RERR_UNSUPPORTED`) and :1329-1334 (write to the converter failed,
    /// `RERR_SOCKETIO`) both call `exit_cleanup()`, and a converter that has
    /// closed its answer pipe (:1336-1337) dies on the next request's write. The
    /// session ends; the lookup never degrades into "no such name".
    Converter(io::Error),
}

impl LookupError {
    /// Reports whether the name converter, rather than the host database, is
    /// the back end that failed.
    #[must_use]
    pub fn is_converter_failure(&self) -> bool {
        matches!(self, Self::Converter(_))
    }
}

impl fmt::Display for LookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(err) => write!(f, "user database lookup failed: {err}"),
            Self::Converter(err) => write!(f, "name converter failed: {err}"),
        }
    }
}

impl Error for LookupError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Database(err) | Self::Converter(err) => Some(err),
        }
    }
}

impl From<LookupError> for io::Error {
    fn from(err: LookupError) -> Self {
        let kind = match &err {
            LookupError::Database(inner) | LookupError::Converter(inner) => inner.kind(),
        };
        io::Error::new(kind, err)
    }
}

/// Applies upstream's asymmetry between the two lookup back ends: a host
/// database failure is "no id", a converter failure is fatal.
///
/// This is the conversion every id-list and file-list call site needs, and
/// having one of it is what keeps the fail-closed rule from being re-decided
/// (and re-lost) per call site.
///
/// upstream: uidlist.c:160-163 vs clientserver.c:1326/:1333.
pub fn no_id_unless_converter_failed<T>(result: LookupResult<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(value),
        Err(LookupError::Database(_)) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_database_failure_reads_as_no_id() {
        let err = LookupError::Database(io::Error::from(io::ErrorKind::PermissionDenied));
        assert!(!err.is_converter_failure());
        assert_eq!(
            no_id_unless_converter_failed::<u32>(Err(err)).expect("tolerated"),
            None
        );
    }

    #[test]
    fn a_converter_failure_is_fatal_and_stays_recognisable() {
        let err = LookupError::Converter(io::Error::from(io::ErrorKind::BrokenPipe));
        assert!(err.is_converter_failure());
        let fatal = no_id_unless_converter_failed::<u32>(Err(err))
            .expect_err("a dead converter must not read as `no such name`");
        assert_eq!(fatal.kind(), io::ErrorKind::BrokenPipe);
        // The discriminator has to survive the trip through io::Error, or a
        // caller downstream of the conversion is back to one undifferentiated
        // failure.
        let payload = fatal
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<LookupError>())
            .expect("the LookupError itself is the payload");
        assert!(payload.is_converter_failure());
    }
}
