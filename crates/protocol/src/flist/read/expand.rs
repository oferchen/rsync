//! Receive-side file-list growth bound, mirroring upstream `flist_expand()`.
//!
//! Upstream stores a received list as an array of `file_struct *` that
//! `flist_expand()` grows through `realloc_array()`, and therefore through
//! `my_alloc()`, which refuses any request of `max_alloc / sizeof(pointer)`
//! elements or more and exits `RERR_MALLOC`. That refusal is what bounds how
//! many entries a hostile sender can make a receiver hold. oc keeps entries in
//! its own containers, so it replays upstream's `used` / `malloced` arithmetic
//! to refuse at exactly the same entry.

use std::io;

use crate::flist::ProcessRole;
use crate::max_alloc::{effective_max_alloc, malloc_failure};

/// upstream: rsync.h:966 `#define FLIST_START (32)`.
const FLIST_START: usize = 32;
/// upstream: rsync.h:967 `#define FLIST_START_LARGE (32 * 1024)`.
pub(super) const FLIST_START_LARGE: usize = 32 * 1024;
/// upstream: rsync.h:968 `#define FLIST_LINEAR (FLIST_START_LARGE * 512)`.
const FLIST_LINEAR: usize = FLIST_START_LARGE * 512;
/// upstream: flist.c:591-639 does its overflow arithmetic in `int`.
const INT_MAX: usize = i32::MAX as usize;

/// Why `flist_expand()` refused to grow a list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    /// `my_alloc()` refused the pointer array at `--max-alloc`.
    MaxAlloc,
    /// The element count would overflow upstream's `int` arithmetic.
    TooLarge,
}

/// The `used` / `malloced` pair of one upstream `struct file_list`.
///
/// Only the counts are kept: the entries themselves live in the caller's
/// containers. A zeroed value is a list that has not been created yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct FlistGrowth {
    used: usize,
    malloced: usize,
}

impl FlistGrowth {
    /// Whether the list has had its initial `flist_expand()`.
    pub(super) const fn is_started(self) -> bool {
        self.malloced != 0
    }

    /// Reserves room for one more entry and counts it, as
    /// `flist_expand(flist, 1)` followed by `flist->used++` does.
    pub(super) fn push(&mut self, role: ProcessRole) -> io::Result<()> {
        self.expand(1, role)?;
        self.used += 1;
        Ok(())
    }

    /// Mirrors `flist_expand(flist, extra)`.
    ///
    /// upstream: flist.c:591-639.
    pub(super) fn expand(&mut self, extra: usize, role: ProcessRole) -> io::Result<()> {
        self.grow(extra, effective_max_alloc())
            .map_err(|refusal| refusal_error(refusal, role))
    }

    fn grow(&mut self, extra: usize, max_alloc: usize) -> Result<(), Refusal> {
        if extra > INT_MAX || self.used > INT_MAX - extra {
            return Err(Refusal::TooLarge);
        }
        let wanted = self.used + extra;
        if wanted <= self.malloced {
            return Ok(());
        }
        if self.malloced < FLIST_START {
            self.malloced = FLIST_START;
        } else if self.malloced >= FLIST_LINEAR {
            if self.malloced > INT_MAX - FLIST_LINEAR {
                return Err(Refusal::TooLarge);
            }
            self.malloced += FLIST_LINEAR;
        } else if self.malloced < FLIST_START_LARGE / 16 {
            if self.malloced > INT_MAX / 4 {
                return Err(Refusal::TooLarge);
            }
            self.malloced *= 4;
        } else {
            if self.malloced > INT_MAX / 2 {
                return Err(Refusal::TooLarge);
            }
            self.malloced *= 2;
        }
        self.malloced = self.malloced.max(wanted);
        // upstream: util2.c:75 `if (num >= max_alloc/size)` for the
        // realloc_array(flist->files, struct file_struct *, malloced) at
        // flist.c:626.
        if self.malloced >= max_alloc / size_of::<*const u8>() {
            return Err(Refusal::MaxAlloc);
        }
        Ok(())
    }
}

/// Renders a refusal with upstream's exact diagnostic.
fn refusal_error(refusal: Refusal, role: ProcessRole) -> io::Error {
    match refusal {
        // upstream: util2.c:78-79 names the realloc_array() call site, which
        // for a file list is always flist_expand()'s at flist.c:626.
        Refusal::MaxAlloc => malloc_failure(format!(
            "[{role}] exceeded --max-alloc={} setting (file=flist.c, line=626)",
            effective_max_alloc()
        )),
        // upstream: flist.c:637-638.
        Refusal::TooLarge => {
            malloc_failure(format!("[{role}] file list has grown too large to expand"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE_MIB: usize = 1024 * 1024;

    /// Grows a fresh list the way recv_file_list() does and returns how many
    /// entries it accepted before refusing.
    fn accepted_entries(max_alloc: usize, cap: usize) -> (usize, Option<Refusal>) {
        let mut list = FlistGrowth::default();
        if let Err(refusal) = list.grow(FLIST_START_LARGE, max_alloc) {
            return (0, Some(refusal));
        }
        while list.used < cap {
            if let Err(refusal) = list.grow(1, max_alloc) {
                return (list.used, Some(refusal));
            }
            list.used += 1;
        }
        (list.used, None)
    }

    #[test]
    fn growth_doubles_from_the_large_start_then_goes_linear() {
        // WHY: the refusal point depends on upstream's exact growth steps; a
        // different schedule would refuse lists upstream accepts or vice versa.
        let mut list = FlistGrowth::default();
        list.grow(FLIST_START_LARGE, usize::MAX).unwrap();
        assert_eq!(list.malloced, FLIST_START_LARGE);
        list.used = FLIST_START_LARGE;
        list.grow(1, usize::MAX).unwrap();
        assert_eq!(list.malloced, 2 * FLIST_START_LARGE);
        list.malloced = FLIST_LINEAR;
        list.used = FLIST_LINEAR;
        list.grow(1, usize::MAX).unwrap();
        assert_eq!(list.malloced, 2 * FLIST_LINEAR);
    }

    #[test]
    fn small_lists_grow_by_four() {
        // upstream: flist.c:606-609 - below FLIST_START_LARGE/16 the array
        // quadruples.
        let mut list = FlistGrowth::default();
        list.grow(1, usize::MAX).unwrap();
        assert_eq!(list.malloced, FLIST_START);
        list.used = FLIST_START;
        list.grow(1, usize::MAX).unwrap();
        assert_eq!(list.malloced, 4 * FLIST_START);
    }

    #[test]
    fn one_mib_refuses_the_step_that_reaches_the_pointer_limit() {
        // WHY: at --max-alloc=1M the 131072-pointer array is the first one
        // my_alloc() refuses, so the last list upstream accepts is half that.
        let limit = ONE_MIB / size_of::<*const u8>();
        let (accepted, refusal) = accepted_entries(ONE_MIB, limit);
        assert_eq!(accepted, limit / 2);
        assert_eq!(refusal, Some(Refusal::MaxAlloc));
    }

    #[test]
    fn int_overflow_is_refused_before_max_alloc() {
        // upstream: flist.c:597-598,603-604 - past INT_MAX the list is "too
        // large to expand" whatever --max-alloc allows.
        let mut list = FlistGrowth {
            used: INT_MAX,
            malloced: INT_MAX,
        };
        assert_eq!(list.grow(1, usize::MAX), Err(Refusal::TooLarge));
        let mut list = FlistGrowth {
            used: INT_MAX - FLIST_LINEAR + 2,
            malloced: INT_MAX - FLIST_LINEAR + 2,
        };
        assert_eq!(list.grow(1, usize::MAX), Err(Refusal::TooLarge));
    }

    #[test]
    fn refusal_text_matches_upstream() {
        let err = refusal_error(Refusal::TooLarge, ProcessRole::PreForkReceiver);
        assert_eq!(
            err.to_string(),
            "[Receiver] file list has grown too large to expand"
        );
        assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
    }
}
