//! The generator's keepalive checks while it walks the file list.
//!
//! upstream: generate_files() starts `next_loopchk` at 0 (generator.c:2802)
//! and, after each entry's recv_generator(), pokes
//! `maybe_send_keepalive(time(NULL), MSK_ALLOW_FLUSH)` once the entry's index
//! reaches it, then adds `loopchk_limit = allowed_lull * 5` (generator.c:2829,
//! :2904-2913). The poke after the first entry always finds the lull passed,
//! since `last_io_out` is still 0 (io.c:113): it flushes what that entry
//! buffered, or sends an empty `MSG_DATA` when the entry buffered nothing.
//!
//! This receiver decides entries in passes and writes their records later, in
//! file-list order, so the checks run where each record is written: before a
//! record, the checks of the entries since the last record, which wrote
//! nothing; after it, the check of the record's own entry. Entries are counted
//! by flat index.

use std::io;

use super::ReceiverContext;
use crate::writer::MsgInfoSender;

/// The two keepalive counters of upstream's generator.
#[derive(Clone, Copy, Debug, Default)]
pub(in crate::receiver) struct LoopChk {
    /// `next_loopchk`: the next flat index whose check pokes.
    next: usize,
    /// touch_up_dirs()'s static `counter` (generator.c:2625).
    touch_up: usize,
}

impl LoopChk {
    /// Runs the checks of the entries before `end` not yet checked.
    ///
    /// Each entry is checked once and `next` moves past an entry only when it
    /// pokes there, so `next` stays above every checked entry: the pokes due
    /// before `end` are the ones at `next`, `next + limit`, and so on.
    fn check_before<W: MsgInfoSender + ?Sized>(
        &mut self,
        end: usize,
        writer: &mut W,
    ) -> io::Result<()> {
        let Some(limit) = loopchk_limit(writer) else {
            return Ok(());
        };
        while self.next < end {
            writer.maybe_send_keepalive()?;
            self.next += limit;
        }
        Ok(())
    }

    /// Counts one entry of the directory touch-up walk and pokes once the
    /// count reaches the limit (generator.c:2654-2659).
    fn touch_up_tick<W: MsgInfoSender + ?Sized>(&mut self, writer: &mut W) -> io::Result<()> {
        let Some(limit) = loopchk_limit(writer) else {
            return Ok(());
        };
        self.touch_up += 1;
        if self.touch_up >= limit {
            self.touch_up = 0;
            writer.maybe_send_keepalive_once_written()?;
        }
        Ok(())
    }
}

/// `loopchk_limit = allowed_lull * 5` (generator.c:2829), or `None` without
/// `--timeout`.
fn loopchk_limit<W: MsgInfoSender + ?Sized>(writer: &W) -> Option<usize> {
    writer.keepalive_lull().map(|lull| {
        usize::try_from(lull.as_secs())
            .unwrap_or(usize::MAX)
            .saturating_mul(5)
            .max(1)
    })
}

impl ReceiverContext {
    /// Runs the generator's keepalive checks of the entries before `end`.
    ///
    /// Called with a record's flat index before the record is written, and
    /// with one past it after.
    pub(in crate::receiver) fn keepalive_checks_before<W: MsgInfoSender + ?Sized>(
        &self,
        end: usize,
        writer: &mut W,
    ) -> io::Result<()> {
        let mut chk = self.loopchk.get();
        let result = chk.check_before(end, writer);
        self.loopchk.set(chk);
        result
    }

    /// Counts one entry of the directory touch-up walk.
    pub(in crate::receiver) fn touch_up_keepalive_tick<W: MsgInfoSender + ?Sized>(
        &self,
        writer: &mut W,
    ) -> io::Result<()> {
        let mut chk = self.loopchk.get();
        let result = chk.touch_up_tick(writer);
        self.loopchk.set(chk);
        result
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Counts the pokes a check makes.
    struct Pokes {
        lull: Option<Duration>,
        flushing: usize,
        once_written: usize,
    }

    impl Pokes {
        fn new(lull: Option<Duration>) -> Self {
            Self {
                lull,
                flushing: 0,
                once_written: 0,
            }
        }
    }

    impl MsgInfoSender for Pokes {
        fn maybe_send_keepalive(&mut self) -> io::Result<bool> {
            self.flushing += 1;
            Ok(true)
        }

        fn maybe_send_keepalive_once_written(&mut self) -> io::Result<bool> {
            self.once_written += 1;
            Ok(true)
        }

        fn keepalive_lull(&self) -> Option<Duration> {
            self.lull
        }
    }

    /// `--timeout=2` gives an allowed lull of 1 s and so a limit of 5.
    fn one_second() -> Pokes {
        Pokes::new(Some(Duration::from_secs(1)))
    }

    #[test]
    fn the_first_entrys_record_is_checked_after_it_is_written() {
        let mut chk = LoopChk::default();
        let mut w = one_second();
        chk.check_before(0, &mut w).unwrap();
        assert_eq!(w.flushing, 0, "nothing is checked before the first record");
        chk.check_before(1, &mut w).unwrap();
        assert_eq!(w.flushing, 1, "entry 0 pokes once its record is buffered");
    }

    #[test]
    fn entries_without_records_are_checked_before_the_next_record() {
        let mut chk = LoopChk::default();
        let mut w = one_second();
        chk.check_before(3, &mut w).unwrap();
        assert_eq!(
            w.flushing, 1,
            "entry 0 wrote nothing and pokes before entry 3's record"
        );
        chk.check_before(4, &mut w).unwrap();
        assert_eq!(
            w.flushing, 1,
            "entries 1-3 fall short of the next check at 5"
        );
    }

    #[test]
    fn checks_fall_every_limit_entries_and_carry_across_calls() {
        let mut chk = LoopChk::default();
        let mut w = one_second();
        for idx in 0..12 {
            chk.check_before(idx, &mut w).unwrap();
            chk.check_before(idx + 1, &mut w).unwrap();
        }
        assert_eq!(w.flushing, 3, "entries 0, 5 and 10 poke");
        chk.check_before(16, &mut w).unwrap();
        assert_eq!(w.flushing, 4, "entry 15 pokes from a later call");
    }

    #[test]
    fn a_long_run_without_records_pokes_once_per_limit() {
        let mut chk = LoopChk::default();
        let mut w = one_second();
        chk.check_before(23, &mut w).unwrap();
        assert_eq!(w.flushing, 5, "entries 0, 5, 10, 15 and 20 each poke");
    }

    #[test]
    fn without_timeout_nothing_is_checked() {
        let mut chk = LoopChk::default();
        let mut w = Pokes::new(None);
        chk.check_before(100, &mut w).unwrap();
        for _ in 0..100 {
            chk.touch_up_tick(&mut w).unwrap();
        }
        assert_eq!((w.flushing, w.once_written), (0, 0));
    }

    #[test]
    fn the_touch_up_walk_pokes_every_limit_entries_once_written() {
        let mut chk = LoopChk::default();
        let mut w = one_second();
        for _ in 0..11 {
            chk.touch_up_tick(&mut w).unwrap();
        }
        assert_eq!(
            w.once_written, 2,
            "the counter reaches 5 twice in 11 entries"
        );
        assert_eq!(
            w.flushing, 0,
            "the walk never makes a connection's first poke"
        );
    }
}
