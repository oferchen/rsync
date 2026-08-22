//! Merging deferred diagnostics back into the rendered event stream.
//!
//! upstream: log.c:272-373 `rwrite()` writes each message the instant it is
//! produced, so upstream never needs an ordering key - the output order *is*
//! the production order. oc buffers the per-file event stream and renders it
//! post-hoc, which splits one output order into two buffers. This module puts
//! them back together, keyed on the production order both sides carry
//! (`logging::Sequence`).

use std::collections::VecDeque;
use std::io::{self, Write};

use logging::{Sequence, Stamped};

/// Diagnostics held back for rendering at the position they were produced.
///
/// The queue holds *already-routed* messages: the caller has run each event
/// through `logging::message_stream` and kept only those bound for the stream
/// this renderer writes to. Nothing here decides a stream, which keeps
/// `message_stream` the single owner of that rule.
///
/// This is a collaborator passed alongside the writer rather than a wrapper
/// around it. A plain `Write` cannot express "a new event is about to be
/// rendered, with this key", and a `Write` subtrait would need a blanket impl
/// for plain writers to keep existing callers compiling - which would overlap
/// the merging type's own impl and be rejected by coherence.
#[derive(Debug, Default)]
pub(crate) struct PendingDiagnostics {
    queue: VecDeque<Stamped<String>>,
}

impl PendingDiagnostics {
    /// Nothing to interleave.
    ///
    /// Every method is then a no-op, so a caller with no diagnostic buffer
    /// behind its event stream - the log-file renderer, and every test that
    /// exercises an emitter directly - pays nothing for the seam.
    pub(crate) const fn empty() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }

    /// Takes the messages to interleave, ordered by production key.
    ///
    /// The input is sorted here rather than assumed sorted: the buffer is
    /// drained from one thread-local queue today, but the key exists precisely
    /// so that messages from several producers can be merged, and a merge that
    /// silently depends on input order would break the first time one is added.
    pub(crate) fn new(mut messages: Vec<Stamped<String>>) -> Self {
        messages.sort_by_key(Stamped::sequence);
        Self {
            queue: messages.into(),
        }
    }

    /// Renders every diagnostic produced before the event about to be written.
    ///
    /// `key` is `None` for an event that carries no production key - every
    /// remote-transfer event, which is built from the wire rather than from a
    /// recorded local action. Such an event has no position to order against,
    /// so **nothing is flushed**: treating it as position zero would push every
    /// held diagnostic ahead of output it may well have followed.
    pub(crate) fn begin_event<W: Write + ?Sized>(
        &mut self,
        key: Option<Sequence>,
        out: &mut W,
    ) -> io::Result<()> {
        let Some(key) = key else {
            return Ok(());
        };
        while let Some(held) = self.queue.front()
            && held.sequence() < key
        {
            writeln!(out, "{}", held.value())?;
            self.queue.pop_front();
        }
        Ok(())
    }

    /// Renders the diagnostics that outlived the event stream.
    ///
    /// Everything produced after the last rendered event belongs here, at the
    /// end of the per-file block - which is where upstream would have written
    /// it, ahead of the `match_report()` totals and the summary trailer.
    pub(crate) fn finish<W: Write + ?Sized>(&mut self, out: &mut W) -> io::Result<()> {
        for held in self.queue.drain(..) {
            writeln!(out, "{}", held.value())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_diagnostic_produced_first_renders_before_the_event() {
        // WHY: this is the whole point of the funnel. Upstream writes the
        // diagnostic at production time, so it precedes the per-file line that
        // was produced after it; oc must reproduce that from the key alone.
        let first = Sequence::stamp();
        let second = Sequence::stamp();
        let mut pending =
            PendingDiagnostics::new(vec![Stamped::with_sequence(first, "skipping".to_owned())]);

        let mut out = Vec::new();
        pending.begin_event(Some(second), &mut out).unwrap();

        assert_eq!(String::from_utf8(out).unwrap(), "skipping\n");
    }

    #[test]
    fn a_diagnostic_produced_later_is_held_back() {
        // WHY: the merge must be an ordering, not a flush-everything-first. A
        // diagnostic produced after the event stays behind it.
        let first = Sequence::stamp();
        let second = Sequence::stamp();
        let mut pending =
            PendingDiagnostics::new(vec![Stamped::with_sequence(second, "later".to_owned())]);

        let mut out = Vec::new();
        pending.begin_event(Some(first), &mut out).unwrap();
        assert!(out.is_empty(), "held diagnostic leaked ahead of its event");

        pending.finish(&mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "later\n");
    }

    #[test]
    fn an_unkeyed_event_flushes_nothing() {
        // WHY: a remote-transfer event has no production key. Treating `None`
        // as position zero would dump every held diagnostic ahead of the whole
        // event stream - the exact mis-ordering the funnel exists to remove.
        let mut pending = PendingDiagnostics::new(vec![Stamped::with_sequence(
            Sequence::stamp(),
            "held".to_owned(),
        )]);

        let mut out = Vec::new();
        pending.begin_event(None, &mut out).unwrap();

        assert!(out.is_empty(), "unkeyed event flushed a held diagnostic");
    }

    #[test]
    fn messages_render_in_production_order_not_input_order() {
        // WHY: `new` sorts rather than trusting its caller, so a buffer merged
        // from more than one producer still renders in production order.
        let first = Sequence::stamp();
        let second = Sequence::stamp();
        let third = Sequence::stamp();
        let mut pending = PendingDiagnostics::new(vec![
            Stamped::with_sequence(third, "c".to_owned()),
            Stamped::with_sequence(first, "a".to_owned()),
            Stamped::with_sequence(second, "b".to_owned()),
        ]);

        let mut out = Vec::new();
        pending.finish(&mut out).unwrap();

        assert_eq!(String::from_utf8(out).unwrap(), "a\nb\nc\n");
    }

    #[test]
    fn an_empty_collaborator_writes_nothing() {
        let mut pending = PendingDiagnostics::empty();

        let mut out = Vec::new();
        pending
            .begin_event(Some(Sequence::stamp()), &mut out)
            .unwrap();
        pending.finish(&mut out).unwrap();

        assert!(out.is_empty());
    }
}
