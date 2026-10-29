//! Stream flags bitmap for batch file headers.
//!
//! The flags encode which rsync options were active during batch creation.
//! Bit positions, names and protocol-version gating all come from one table,
//! [`STREAM_FLAGS`], mirroring upstream's `batch.c:59-95` `flag_ptr[]` and
//! `flag_name[]` arrays.

use std::io::{self, Read, Write};

use super::wire::{read_i32, write_i32};
use crate::error::{BatchError, BatchResult};

/// One data-stream-affecting option recorded in the batch header.
struct StreamFlag {
    /// The `flag_name[]` text embedded in the reconcile notice.
    name: &'static str,
    /// Lowest protocol whose `check_batch_flags()` visits this bit.
    min_protocol: i32,
    /// The [`BatchFlags`] field carrying this bit.
    field: fn(&mut BatchFlags) -> &mut bool,
}

impl StreamFlag {
    fn get(&self, mut flags: BatchFlags) -> bool {
        *(self.field)(&mut flags)
    }
}

/// The batch-restorable options in bit order: entry N is bit N.
///
/// upstream: batch.c:59-95 `flag_ptr[]` / `flag_name[]`. `check_batch_flags()`
/// truncates the array below protocol 29 at bit 7 and below 30 at bit 9
/// (batch.c:125-128), which `min_protocol` records.
const STREAM_FLAGS: [StreamFlag; 15] = [
    StreamFlag {
        name: "--recurse (-r)",
        min_protocol: 0,
        field: |f| &mut f.recurse,
    },
    StreamFlag {
        name: "--owner (-o)",
        min_protocol: 0,
        field: |f| &mut f.preserve_uid,
    },
    StreamFlag {
        name: "--group (-g)",
        min_protocol: 0,
        field: |f| &mut f.preserve_gid,
    },
    StreamFlag {
        name: "--links (-l)",
        min_protocol: 0,
        field: |f| &mut f.preserve_links,
    },
    StreamFlag {
        name: "--devices (-D)",
        min_protocol: 0,
        field: |f| &mut f.preserve_devices,
    },
    StreamFlag {
        name: "--hard-links (-H)",
        min_protocol: 0,
        field: |f| &mut f.preserve_hard_links,
    },
    StreamFlag {
        name: "--checksum (-c)",
        min_protocol: 0,
        field: |f| &mut f.always_checksum,
    },
    StreamFlag {
        name: "--dirs (-d)",
        min_protocol: 29,
        field: |f| &mut f.xfer_dirs,
    },
    StreamFlag {
        name: "--compress (-z)",
        min_protocol: 29,
        field: |f| &mut f.do_compression,
    },
    StreamFlag {
        name: "--iconv",
        min_protocol: 30,
        field: |f| &mut f.iconv,
    },
    StreamFlag {
        name: "--acls (-A)",
        min_protocol: 30,
        field: |f| &mut f.preserve_acls,
    },
    StreamFlag {
        name: "--xattrs (-X)",
        min_protocol: 30,
        field: |f| &mut f.preserve_xattrs,
    },
    StreamFlag {
        name: "--inplace",
        min_protocol: 30,
        field: |f| &mut f.inplace,
    },
    StreamFlag {
        name: "--append",
        min_protocol: 30,
        field: |f| &mut f.append,
    },
    StreamFlag {
        name: "--append-verify",
        min_protocol: 30,
        field: |f| &mut f.append_verify,
    },
];

/// Bit index of the `--iconv` flag - a mismatch here is fatal upstream.
const ICONV_BIT: usize = 9;

/// The stream flags `check_batch_flags()` visits at `protocol_version`, with
/// their bit index.
fn visited_flags(protocol_version: i32) -> impl Iterator<Item = (usize, &'static StreamFlag)> {
    STREAM_FLAGS
        .iter()
        .enumerate()
        .filter(move |(_, flag)| flag.min_protocol <= protocol_version)
}

/// The invocation's options after reconciling them with a batch header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciledFlags {
    /// The options the replay runs with.
    pub flags: BatchFlags,
    /// One `rprintf(FINFO, ...)` notice per option the batch changed, in bit
    /// order. Upstream prints them under `--info=misc` (`INFO_GTE(MISC, 1)`).
    pub notices: Vec<String>,
}

/// Reconcile the currently-active options against a batch file's recorded
/// stream flags.
///
/// `recorded` is the stream-flags bitmap read from the batch header; `active`
/// is the flag state derived from the current `--read-batch` invocation. Each
/// option `check_batch_flags()` visits at `protocol_version` is forced to the
/// batch's value, with a notice when it changes; every other option keeps the
/// invocation's value. An `--iconv` mismatch is fatal and returns
/// [`BatchError::FlagMismatch`].
///
/// Two adjustments follow the loop, as upstream makes them:
///
/// - below protocol 29, where the header has no `--dirs` bit, `--dirs`
///   follows `-r`: on with it, and off without it unless the invocation gave
///   `-d` without `-r` (batch.c:151-156);
/// - the append mode changes only when the batch sets `--append` or
///   `--append-verify`; a batch with neither leaves the invocation's mode in
///   place even after the "Clearing" notice (batch.c:158-161).
///
/// # Upstream Reference
///
/// - `batch.c:121-162`: `check_batch_flags()`.
pub fn check_batch_flags(
    recorded: BatchFlags,
    active: BatchFlags,
    protocol_version: i32,
) -> BatchResult<ReconciledFlags> {
    let mut flags = active;
    let mut notices = Vec::new();

    for (bit, flag) in visited_flags(protocol_version) {
        let set = flag.get(recorded);
        let current = (flag.field)(&mut flags);
        if *current == set {
            continue;
        }
        if bit == ICONV_BIT {
            // upstream: batch.c:137-142 - an --iconv mismatch is fatal.
            return Err(BatchError::FlagMismatch(format!(
                "{} specify the --iconv option to use this batch file.",
                if set { "Please" } else { "Do not" }
            )));
        }
        // upstream: batch.c:143-148 - INFO(MISC) reconcile notice.
        notices.push(format!(
            "{}ing the {} option to match the batchfile.",
            if set { "Sett" } else { "Clear" },
            flag.name
        ));
        *current = set;
    }

    // upstream: batch.c:151-156 - `if (recurse) xfer_dirs |= 1; else if
    // (xfer_dirs < 2) xfer_dirs = 0;`. parse_arguments() leaves xfer_dirs at 2
    // only for `-d` without `-r` (options.c:2323-2329).
    if protocol_version < 29 {
        flags.xfer_dirs = flags.recurse || (active.xfer_dirs && !active.recurse);
    }

    // upstream: batch.c:158-161 - append_mode is assigned only from a set bit.
    if !flags.append && !flags.append_verify {
        flags.append = active.append;
        flags.append_verify = active.append_verify;
    }

    Ok(ReconciledFlags { flags, notices })
}

/// Stream flags bitmap that affects data stream format.
///
/// These flags must match between write and read to ensure correct
/// interpretation of the batch file. Each field is one `STREAM_FLAGS` entry,
/// which fixes its bit position and protocol gating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BatchFlags {
    /// Bit 0: --recurse (-r) - upstream: batch.c:60 `&recurse`
    pub recurse: bool,
    /// Bit 1: --owner (-o) - upstream: batch.c:61 `&preserve_uid`
    pub preserve_uid: bool,
    /// Bit 2: --group (-g) - upstream: batch.c:62 `&preserve_gid`
    pub preserve_gid: bool,
    /// Bit 3: --links (-l) - upstream: batch.c:63 `&preserve_links`
    pub preserve_links: bool,
    /// Bit 4: --devices (-D) - upstream: batch.c:64 `&preserve_devices`
    pub preserve_devices: bool,
    /// Bit 5: --hard-links (-H) - upstream: batch.c:65 `&preserve_hard_links`
    pub preserve_hard_links: bool,
    /// Bit 6: --checksum (-c) - upstream: batch.c:66 `&always_checksum`
    pub always_checksum: bool,
    /// Bit 7: --dirs (-d) [protocol >= 29] - upstream: batch.c:67 `&xfer_dirs`
    pub xfer_dirs: bool,
    /// Bit 8: --compress (-z) [protocol >= 29] - upstream: batch.c:68 `&do_compression`
    ///
    /// When true, the batch body carries compressed token data and the reader
    /// must decompress it. The codec is always zlib: upstream
    /// `compat.c:414 getenv_nstr()` pins the compression list to "zlib" while
    /// `write_batch` is set, and `compat.c:195-196 parse_compress_choice()`
    /// resolves a bare `do_compression` to CPRES_ZLIB on `--read-batch`.
    pub do_compression: bool,
    /// Bit 9: --iconv [protocol >= 30] - upstream: batch.c:69 `&tweaked_iconv`
    pub iconv: bool,
    /// Bit 10: --acls (-A) [protocol >= 30] - upstream: batch.c:70 `&preserve_acls`
    pub preserve_acls: bool,
    /// Bit 11: --xattrs (-X) [protocol >= 30] - upstream: batch.c:71 `&preserve_xattrs`
    pub preserve_xattrs: bool,
    /// Bit 12: --inplace [protocol >= 30] - upstream: batch.c:72 `&inplace`
    pub inplace: bool,
    /// Bit 13: --append [protocol >= 30] - upstream: batch.c:73 `&tweaked_append`
    pub append: bool,
    /// Bit 14: --append-verify [protocol >= 30] - upstream: batch.c:74 `&tweaked_append_verify`
    pub append_verify: bool,
}

impl BatchFlags {
    /// Decode the bits `check_batch_flags()` visits at `protocol_version`;
    /// the rest stay clear.
    pub fn from_bitmap(bitmap: i32, protocol_version: i32) -> Self {
        let mut flags = Self::default();
        for (bit, flag) in visited_flags(protocol_version) {
            *(flag.field)(&mut flags) = bitmap & (1 << bit) != 0;
        }
        flags
    }

    /// Encode the bits `check_batch_flags()` visits at `protocol_version`.
    pub fn to_bitmap(&self, protocol_version: i32) -> i32 {
        visited_flags(protocol_version)
            .filter(|(_, flag)| flag.get(*self))
            .fold(0, |bitmap, (bit, _)| bitmap | 1 << bit)
    }

    /// Write the stream-flags bitmap to a writer.
    ///
    /// The full 15-bit bitmap is always written, independent of the negotiated
    /// protocol version, mirroring upstream `batch.c:97-114
    /// write_stream_flags()` (which walks the whole `flag_ptr[]` array).
    /// Upstream truncates by protocol only on the read side in
    /// `batch.c:125-128 check_batch_flags()`, so a proto < 30 batch created
    /// with `-z`/`-d` (proto 28) or `--inplace` (proto 29) must still record
    /// those header bits for the byte layout to match upstream.
    /// `protocol_version` is accepted for call-site symmetry with the rest of
    /// the header writer but has no effect on the emitted bytes.
    pub fn write_to_versioned<W: Write>(
        &self,
        writer: &mut W,
        protocol_version: i32,
    ) -> io::Result<()> {
        let _ = protocol_version;
        write_i32(writer, self.to_bitmap(i32::MAX))
    }

    /// Read the raw bitmap from a reader.
    ///
    /// Returns the raw `i32` bitmap without interpreting protocol-gated bits.
    /// The caller must pass this to [`BatchFlags::from_bitmap`] with the
    /// correct protocol version (read from the header after the bitmap).
    pub fn read_raw<R: Read>(reader: &mut R) -> io::Result<i32> {
        read_i32(reader)
    }
}

#[cfg(test)]
mod check_flags_tests {
    use super::{BatchFlags, check_batch_flags};
    use crate::error::BatchError;

    #[test]
    fn matching_flags_produce_no_notices() {
        // WHY: when the invocation already matches the batchfile there is
        // nothing to reconcile, so upstream stays silent (batch.c:135 skips
        // the body when *flag_ptr[i] == set).
        let flags = BatchFlags {
            recurse: true,
            preserve_uid: true,
            ..BatchFlags::default()
        };
        assert_eq!(
            check_batch_flags(flags, flags, 31).unwrap().notices,
            Vec::<String>::new()
        );
    }

    #[test]
    fn missing_active_flag_emits_setting_notice() {
        // WHY: the batch enabled -r but the caller did not; upstream forces
        // the option on and says so (batch.c:143-148, set=1 -> "Sett").
        let recorded = BatchFlags {
            recurse: true,
            ..BatchFlags::default()
        };
        let active = BatchFlags::default();
        assert_eq!(
            check_batch_flags(recorded, active, 31).unwrap().notices,
            vec!["Setting the --recurse (-r) option to match the batchfile.".to_owned()]
        );
    }

    #[test]
    fn extra_active_flag_emits_clearing_notice() {
        // WHY: the caller passed -c but the batch was made without it; upstream
        // clears the option to match (batch.c:143-148, set=0 -> "Clear").
        let recorded = BatchFlags::default();
        let active = BatchFlags {
            always_checksum: true,
            ..BatchFlags::default()
        };
        assert_eq!(
            check_batch_flags(recorded, active, 31).unwrap().notices,
            vec!["Clearing the --checksum (-c) option to match the batchfile.".to_owned()]
        );
    }

    #[test]
    fn iconv_required_by_batch_is_fatal() {
        // WHY: batch.c:137-142 makes an --iconv mismatch fatal, not a notice;
        // a batch recorded with --iconv cannot be replayed without it.
        let recorded = BatchFlags {
            iconv: true,
            ..BatchFlags::default()
        };
        let active = BatchFlags::default();
        let err = check_batch_flags(recorded, active, 31).unwrap_err();
        assert!(matches!(err, BatchError::FlagMismatch(_)));
        assert_eq!(
            err.to_string(),
            "Please specify the --iconv option to use this batch file."
        );
    }

    #[test]
    fn iconv_forbidden_by_batch_is_fatal() {
        // WHY: the reverse mismatch - caller passed --iconv, batch has none -
        // is equally fatal with the "Do not" wording (batch.c:139-140).
        let recorded = BatchFlags::default();
        let active = BatchFlags {
            iconv: true,
            ..BatchFlags::default()
        };
        let err = check_batch_flags(recorded, active, 31).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Do not specify the --iconv option to use this batch file."
        );
    }

    #[test]
    fn iconv_bit_is_ignored_below_protocol_30() {
        // WHY: batch.c:127-128 nulls flag_ptr[9] below protocol 30, so the
        // iconv bit must not be checked and must never abort a proto-29 replay.
        let recorded = BatchFlags {
            iconv: true,
            ..BatchFlags::default()
        };
        let active = BatchFlags::default();
        assert_eq!(
            check_batch_flags(recorded, active, 29).unwrap().notices,
            Vec::<String>::new()
        );
    }

    #[test]
    fn proto28_ignores_dirs_and_compress_bits() {
        // WHY: batch.c:125-126 nulls flag_ptr[7] below protocol 29, so -d/-z
        // (bits 7-8) and every later bit are outside the reconcile loop.
        let recorded = BatchFlags {
            xfer_dirs: true,
            do_compression: true,
            ..BatchFlags::default()
        };
        let active = BatchFlags::default();
        assert_eq!(
            check_batch_flags(recorded, active, 28).unwrap().notices,
            Vec::<String>::new()
        );
    }

    #[test]
    fn notices_are_ordered_by_bit_index() {
        // WHY: upstream walks flag_ptr[] in order, so notices arrive lowest
        // bit first; downstream output fidelity depends on that ordering.
        let recorded = BatchFlags {
            preserve_links: true,
            ..BatchFlags::default()
        };
        let active = BatchFlags {
            preserve_uid: true,
            ..BatchFlags::default()
        };
        assert_eq!(
            check_batch_flags(recorded, active, 31).unwrap().notices,
            vec![
                "Clearing the --owner (-o) option to match the batchfile.".to_owned(),
                "Setting the --links (-l) option to match the batchfile.".to_owned(),
            ]
        );
    }

    #[test]
    fn options_beyond_the_protocol_cutoff_keep_the_invocation_value() {
        // WHY: batch.c:125-128 stops the loop at bit 9 below protocol 30, so
        // a proto-29 replay with --inplace keeps it even though the header
        // bit is clear; forcing it off would replay via temp files instead.
        let active = BatchFlags {
            inplace: true,
            preserve_acls: true,
            ..BatchFlags::default()
        };
        let reconciled = check_batch_flags(BatchFlags::default(), active, 29).unwrap();
        assert_eq!(reconciled.flags, active);
        assert!(reconciled.notices.is_empty());
    }

    #[test]
    fn proto28_derives_dirs_from_recurse() {
        // WHY: batch.c:151-156 - a proto-28 header has no -d bit, so -d
        // follows the recorded -r, except an explicit -d without -r.
        let recurse = BatchFlags {
            recurse: true,
            ..BatchFlags::default()
        };
        let reconciled = check_batch_flags(recurse, BatchFlags::default(), 28).unwrap();
        assert!(reconciled.flags.xfer_dirs, "-r implies -d");

        let dirs_and_recurse = BatchFlags {
            recurse: true,
            xfer_dirs: true,
            ..BatchFlags::default()
        };
        let reconciled = check_batch_flags(BatchFlags::default(), dirs_and_recurse, 28).unwrap();
        assert!(
            !reconciled.flags.xfer_dirs,
            "-r -d is xfer_dirs 1, cleared with -r"
        );

        let dirs_only = BatchFlags {
            xfer_dirs: true,
            ..BatchFlags::default()
        };
        let reconciled = check_batch_flags(BatchFlags::default(), dirs_only, 28).unwrap();
        assert!(reconciled.flags.xfer_dirs, "-d alone is xfer_dirs 2, kept");
    }

    #[test]
    fn append_mode_changes_only_for_a_set_bit() {
        // WHY: batch.c:158-161 assigns append_mode only from a set tweaked
        // bit; a batch without either keeps the invocation's --append after
        // printing the "Clearing" notice.
        let active = BatchFlags {
            append: true,
            ..BatchFlags::default()
        };
        let reconciled = check_batch_flags(BatchFlags::default(), active, 31).unwrap();
        assert!(reconciled.flags.append);
        assert_eq!(
            reconciled.notices,
            vec!["Clearing the --append option to match the batchfile.".to_owned()]
        );

        let recorded = BatchFlags {
            append_verify: true,
            ..BatchFlags::default()
        };
        let reconciled = check_batch_flags(recorded, active, 31).unwrap();
        assert!(!reconciled.flags.append && reconciled.flags.append_verify);
    }

    #[test]
    fn bitmap_round_trip_follows_the_table() {
        // WHY: one table drives encode and decode; bit N must land on the
        // field flag_ptr[N] names and the header always carries all 15 bits.
        let all = BatchFlags::from_bitmap(0x7fff, 31);
        assert_eq!(all.to_bitmap(31), 0x7fff);
        assert_eq!(BatchFlags::from_bitmap(0x7fff, 29).to_bitmap(31), 0x01ff);
        assert_eq!(BatchFlags::from_bitmap(0x7fff, 28).to_bitmap(31), 0x007f);
        let inplace = BatchFlags {
            inplace: true,
            ..BatchFlags::default()
        };
        let mut header = Vec::new();
        inplace.write_to_versioned(&mut header, 28).unwrap();
        assert_eq!(header, (1i32 << 12).to_le_bytes());
    }
}
