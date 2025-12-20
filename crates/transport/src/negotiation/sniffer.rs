use std::io::{self, Read};

use logging::debug_log;
use protocol::{LEGACY_DAEMON_PREFIX_LEN, NegotiationPrologue, NegotiationPrologueSniffer};

use super::NegotiatedStream;

/// Sniffs the negotiation prologue from the provided reader.
///
/// The function mirrors upstream rsync's handshake detection logic: it peeks at
/// the first byte to distinguish binary negotiations from legacy ASCII
/// `@RSYNCD:` exchanges, buffering the observed data so the caller can replay it
/// when parsing the daemon greeting or continuing the binary protocol.
///
/// # Errors
///
/// Returns [`io::ErrorKind::UnexpectedEof`] if the stream ends before a
/// negotiation style can be determined or propagates any underlying I/O error
/// reported by the reader.
#[must_use = "the returned stream holds the buffered negotiation state and must be consumed"]
pub fn sniff_negotiation_stream<R: Read>(reader: R) -> io::Result<NegotiatedStream<R>> {
    let mut sniffer = NegotiationPrologueSniffer::new();
    sniff_negotiation_stream_with_sniffer(reader, &mut sniffer)
}

/// Sniffs the negotiation prologue using a caller supplied sniffer instance.
///
/// The helper mirrors [`sniff_negotiation_stream`] but reuses the provided
/// [`NegotiationPrologueSniffer`], avoiding temporary allocations when a
/// higher layer already maintains a pool of reusable sniffers. The sniffer is
/// reset to guarantee stale state from previous sessions is discarded before
/// the new transport is observed.
#[must_use = "the returned stream holds the buffered negotiation state and must be consumed"]
pub fn sniff_negotiation_stream_with_sniffer<R: Read>(
    mut reader: R,
    sniffer: &mut NegotiationPrologueSniffer,
) -> io::Result<NegotiatedStream<R>> {
    sniffer.reset();

    let decision = sniffer.read_from(&mut reader)?;
    debug_assert_ne!(decision, NegotiationPrologue::NeedMoreData);

    let sniffed_prefix_len = sniffer.sniffed_prefix_len();
    let buffered = sniffer.take_buffered();

    debug_log!(
        Connect,
        2,
        "sniffed negotiation prologue: {:?} ({} bytes buffered)",
        decision,
        buffered.len()
    );

    debug_assert!(sniffed_prefix_len <= LEGACY_DAEMON_PREFIX_LEN);
    debug_assert!(sniffed_prefix_len <= buffered.len());

    Ok(NegotiatedStream::from_raw_components(
        reader,
        decision,
        sniffed_prefix_len,
        0,
        buffered,
    ))
}
