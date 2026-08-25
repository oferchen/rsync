//! Splitting a remote transfer's operand vector into sources and a destination.
//!
//! Upstream has no "you must supply a destination" check at all. It reaches the
//! same place from the other direction: when fewer than two operands are given
//! it *infers* list-only mode, and when the source is remote it explicitly
//! records that there is no destination argument.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/options.c:2311-2312` - `if (argc < 2 && !read_batch &&
//!   !am_server) list_only |= 1;`. The inference is transport-agnostic: a local
//!   path, an SSH `host:path` source and a daemon `host::module` all take it.
//! - `rsync-3.5.0/main.c:1465-1466` - inside `start_client()`'s remote-source
//!   branch, `if (argc == 1 || **argv == ':') argc = 0; /* no dest arg */`.
//! - `rsync-3.5.0/main.c:712` - `get_local_name()` returns NULL immediately for
//!   `list_only`, which is why a destination that is never consumed is safe.

use std::ffi::OsString;

use crate::client::config::ClientConfig;
use crate::client::error::{ClientError, invalid_argument_error};

/// Splits `args` into `(sources, destination)` for a remote transfer.
///
/// With two or more operands the last is the destination, as usual. With
/// exactly one the operand is a source and `fallback_dest` stands in for the
/// destination upstream simply does not have (`main.c:1465-1466`); the caller
/// owns that value so the borrow outlives the call. `list_only` makes the
/// stand-in inert - upstream's `get_local_name()` returns NULL before ever
/// looking at it (`main.c:712`), and oc's receiver is read-only for the same
/// reason - so a one-operand transfer without `list_only` is rejected rather
/// than silently writing to it.
///
/// The one-operand case normally cannot reach here without `list_only`: the CLI
/// applies upstream's `argc < 2` inference while parsing
/// (`crates/cli/src/frontend/execution/drive/workflow/run.rs`). The conjunct
/// guards the library entry points, which can be called with a hand-built
/// [`ClientConfig`].
///
/// # Errors
///
/// Returns an invalid-argument error (exit code 1) when `args` is empty, or
/// when a single operand is given without `list_only`.
pub(crate) fn split_transfer_operands<'a>(
    args: &'a [OsString],
    config: &ClientConfig,
    fallback_dest: &'a OsString,
) -> Result<(&'a [OsString], &'a OsString), ClientError> {
    if args.is_empty() || (args.len() < 2 && !config.list_only()) {
        return Err(invalid_argument_error(
            "need at least one source and one destination",
            1,
        ));
    }

    if args.len() < 2 {
        return Ok((args, fallback_dest));
    }

    let (sources, destination) = args.split_at(args.len() - 1);
    Ok((sources, &destination[0]))
}

#[cfg(test)]
mod tests {
    use super::split_transfer_operands;
    use crate::client::config::ClientConfig;
    use std::ffi::OsString;

    fn config(list_only: bool) -> ClientConfig {
        ClientConfig::builder().list_only(list_only).build()
    }

    fn operands(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    /// Two or more operands keep the ordinary split: everything but the last is
    /// a source. `list_only` does not change that - upstream still lists the
    /// source when a destination is present (`list_only > 1` suppresses the
    /// transfer, not the operand parse).
    #[test]
    fn trailing_operand_is_the_destination() {
        let dot = OsString::from(".");
        for list_only in [false, true] {
            let args = operands(&["a", "b", "dest"]);
            let (sources, destination) =
                split_transfer_operands(&args, &config(list_only), &dot).expect("split");
            assert_eq!(sources, &args[..2]);
            assert_eq!(destination, &args[2]);
        }
    }

    /// THE FIX: a lone operand is a SOURCE, and the caller's stand-in becomes
    /// the destination. Upstream records `argc = 0` here (main.c:1465-1466);
    /// the stand-in is inert because `list_only` keeps the receiver read-only.
    #[test]
    fn lone_operand_under_list_only_is_a_source() {
        let dot = OsString::from(".");
        let args = operands(&["host:path"]);
        let (sources, destination) =
            split_transfer_operands(&args, &config(true), &dot).expect("split");
        assert_eq!(sources, &args[..]);
        assert_eq!(destination, &dot);
    }

    /// NON-VACUITY: without `list_only` the same operand vector is still
    /// rejected, so the fix cannot be mistaken for "always synthesize a
    /// destination". The CLI never produces this shape (it applies upstream's
    /// `argc < 2` inference first); a hand-built config can.
    #[test]
    fn lone_operand_without_list_only_is_rejected() {
        let dot = OsString::from(".");
        let args = operands(&["host:path"]);
        let error = split_transfer_operands(&args, &config(false), &dot)
            .expect_err("one operand without list_only must be rejected");
        assert!(
            error.to_string().contains("one source and one destination"),
            "unexpected error: {error}"
        );
    }

    /// An empty operand vector is an error under every setting - upstream's
    /// inference turns a short list into list-only, never into a no-op.
    #[test]
    fn empty_operands_are_rejected_even_under_list_only() {
        let dot = OsString::from(".");
        let args: Vec<OsString> = Vec::new();
        for list_only in [false, true] {
            split_transfer_operands(&args, &config(list_only), &dot)
                .expect_err("an empty operand vector is always an error");
        }
    }
}
