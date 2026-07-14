//! Stream flags bitmap for batch file headers.
//!
//! The flags encode which rsync options were active during batch creation.
//! Bit positions and protocol-version gating match upstream rsync's
//! `batch.c:write_stream_flags()`.

use std::io::{self, Read, Write};

use super::wire::{read_i32, write_i32};
use crate::error::{BatchError, BatchResult};

/// Human-readable option names in stream-flag bit order.
///
/// Mirrors upstream `batch.c:78-95 flag_name[]` exactly. Index N names the
/// option carried by bit N of the stream-flags bitmap and is embedded verbatim
/// in the reconcile notices emitted by [`check_batch_flags`].
const FLAG_NAMES: [&str; 15] = [
    "--recurse (-r)",
    "--owner (-o)",
    "--group (-g)",
    "--links (-l)",
    "--devices (-D)",
    "--hard-links (-H)",
    "--checksum (-c)",
    "--dirs (-d)",
    "--compress (-z)",
    "--iconv",
    "--acls (-A)",
    "--xattrs (-X)",
    "--inplace",
    "--append",
    "--append-verify",
];

/// Bit index of the `--iconv` flag - a mismatch here is fatal upstream.
const ICONV_BIT: usize = 9;

/// Project a [`BatchFlags`] onto the 15-entry bit vector used by
/// [`check_batch_flags`], in the same order as upstream `batch.c:59-76
/// flag_ptr[]`.
fn flag_bits(flags: &BatchFlags) -> [bool; 15] {
    [
        flags.recurse,
        flags.preserve_uid,
        flags.preserve_gid,
        flags.preserve_links,
        flags.preserve_devices,
        flags.preserve_hard_links,
        flags.always_checksum,
        flags.xfer_dirs,
        flags.do_compression,
        flags.iconv,
        flags.preserve_acls,
        flags.preserve_xattrs,
        flags.inplace,
        flags.append,
        flags.append_verify,
    ]
}

/// Reconcile the currently-active options against a batch file's recorded
/// stream flags.
///
/// `recorded` is the stream-flags bitmap read from the batch header; `active`
/// is the flag state derived from the current `--read-batch` invocation. For
/// every data-stream-affecting flag that differs, upstream forces the active
/// option to match the batch and mentions the change at `--info=misc` level.
/// The returned vector holds those notices in bit order, each formatted exactly
/// as upstream's `rprintf(FINFO, ...)`; the caller decides whether to print
/// them based on verbosity. An `--iconv` mismatch is instead fatal and returns
/// [`BatchError::FlagMismatch`].
///
/// Protocol version gates which flags participate, matching how upstream
/// truncates `flag_ptr[]`: bits 7-8 require protocol >= 29 and bits 9-14
/// require protocol >= 30.
///
/// # Upstream Reference
///
/// - `batch.c:120-161`: `check_batch_flags()`.
pub fn check_batch_flags(
    recorded: BatchFlags,
    active: BatchFlags,
    protocol_version: i32,
) -> BatchResult<Vec<String>> {
    // upstream: batch.c:124-127 - flag_ptr[] is truncated by protocol version
    // (flag_ptr[7]=NULL below 29, flag_ptr[9]=NULL below 30), so the reconcile
    // loop only visits the bits the peer actually negotiated.
    let bit_count = if protocol_version < 29 {
        7
    } else if protocol_version < 30 {
        9
    } else {
        15
    };

    let recorded_bits = flag_bits(&recorded);
    let active_bits = flag_bits(&active);
    let mut messages = Vec::new();

    for i in 0..bit_count {
        let set = recorded_bits[i];
        if active_bits[i] == set {
            continue;
        }
        if i == ICONV_BIT {
            // upstream: batch.c:137-142 - an --iconv mismatch is fatal.
            return Err(BatchError::FlagMismatch(format!(
                "{} specify the --iconv option to use this batch file.",
                if set { "Please" } else { "Do not" }
            )));
        }
        // upstream: batch.c:143-148 - INFO(MISC) reconcile notice.
        messages.push(format!(
            "{}ing the {} option to match the batchfile.",
            if set { "Sett" } else { "Clear" },
            FLAG_NAMES[i]
        ));
    }

    Ok(messages)
}

/// Stream flags bitmap that affects data stream format.
///
/// These flags must match between write and read to ensure correct
/// interpretation of the batch file. The bit positions and protocol-version
/// gating match upstream rsync's `batch.c:59-76 flag_ptr[]` array exactly.
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
    /// When true, the batch body contains compressed token data and the
    /// reader must decompress using CPRES_ZLIB (upstream compat.c:194-195).
    /// oc-rsync always writes `false` here because it records uncompressed
    /// data, avoiding the upstream limitation where the batch format does
    /// not record the actual compression algorithm.
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
    /// Create a new flags structure from a bitmap.
    #[allow(clippy::field_reassign_with_default)]
    pub fn from_bitmap(bitmap: i32, protocol_version: i32) -> Self {
        let mut flags = Self::default();
        flags.recurse = (bitmap & (1 << 0)) != 0;
        flags.preserve_uid = (bitmap & (1 << 1)) != 0;
        flags.preserve_gid = (bitmap & (1 << 2)) != 0;
        flags.preserve_links = (bitmap & (1 << 3)) != 0;
        flags.preserve_devices = (bitmap & (1 << 4)) != 0;
        flags.preserve_hard_links = (bitmap & (1 << 5)) != 0;
        flags.always_checksum = (bitmap & (1 << 6)) != 0;

        if protocol_version >= 29 {
            flags.xfer_dirs = (bitmap & (1 << 7)) != 0;
            flags.do_compression = (bitmap & (1 << 8)) != 0;
        }

        if protocol_version >= 30 {
            flags.iconv = (bitmap & (1 << 9)) != 0;
            flags.preserve_acls = (bitmap & (1 << 10)) != 0;
            flags.preserve_xattrs = (bitmap & (1 << 11)) != 0;
            flags.inplace = (bitmap & (1 << 12)) != 0;
            flags.append = (bitmap & (1 << 13)) != 0;
            flags.append_verify = (bitmap & (1 << 14)) != 0;
        }

        flags
    }

    /// Convert flags to a bitmap.
    pub const fn to_bitmap(&self, protocol_version: i32) -> i32 {
        let mut bitmap = 0i32;

        if self.recurse {
            bitmap |= 1 << 0;
        }
        if self.preserve_uid {
            bitmap |= 1 << 1;
        }
        if self.preserve_gid {
            bitmap |= 1 << 2;
        }
        if self.preserve_links {
            bitmap |= 1 << 3;
        }
        if self.preserve_devices {
            bitmap |= 1 << 4;
        }
        if self.preserve_hard_links {
            bitmap |= 1 << 5;
        }
        if self.always_checksum {
            bitmap |= 1 << 6;
        }

        if protocol_version >= 29 {
            if self.xfer_dirs {
                bitmap |= 1 << 7;
            }
            if self.do_compression {
                bitmap |= 1 << 8;
            }
        }

        if protocol_version >= 30 {
            if self.iconv {
                bitmap |= 1 << 9;
            }
            if self.preserve_acls {
                bitmap |= 1 << 10;
            }
            if self.preserve_xattrs {
                bitmap |= 1 << 11;
            }
            if self.inplace {
                bitmap |= 1 << 12;
            }
            if self.append {
                bitmap |= 1 << 13;
            }
            if self.append_verify {
                bitmap |= 1 << 14;
            }
        }

        bitmap
    }

    /// Write flags to a writer, masking bits by protocol version.
    ///
    /// Only bits valid for the given protocol version are written.
    /// Upstream `batch.c:write_stream_flags()` uses the negotiated
    /// `protocol_version` to decide which bits to set.
    pub fn write_to_versioned<W: Write>(
        &self,
        writer: &mut W,
        protocol_version: i32,
    ) -> io::Result<()> {
        write_i32(writer, self.to_bitmap(protocol_version))
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
            check_batch_flags(flags, flags, 31).unwrap(),
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
            check_batch_flags(recorded, active, 31).unwrap(),
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
            check_batch_flags(recorded, active, 31).unwrap(),
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
        // WHY: batch.c:126-127 nulls flag_ptr[9] below protocol 30, so the
        // iconv bit must not be checked and must never abort a proto-29 replay.
        let recorded = BatchFlags {
            iconv: true,
            ..BatchFlags::default()
        };
        let active = BatchFlags::default();
        assert_eq!(
            check_batch_flags(recorded, active, 29).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn proto28_ignores_dirs_and_compress_bits() {
        // WHY: batch.c:124-125 nulls flag_ptr[7] below protocol 29, so -d/-z
        // (bits 7-8) and every later bit are outside the reconcile loop.
        let recorded = BatchFlags {
            xfer_dirs: true,
            do_compression: true,
            ..BatchFlags::default()
        };
        let active = BatchFlags::default();
        assert_eq!(
            check_batch_flags(recorded, active, 28).unwrap(),
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
            check_batch_flags(recorded, active, 31).unwrap(),
            vec![
                "Clearing the --owner (-o) option to match the batchfile.".to_owned(),
                "Setting the --links (-l) option to match the batchfile.".to_owned(),
            ]
        );
    }
}
