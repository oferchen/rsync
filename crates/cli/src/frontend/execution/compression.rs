//! Parsing for the compression-related flags: `--compress-level`,
//! `--compress-choice`, `--compress-threads`, and `--bwlimit`.

use std::ffi::OsStr;
use std::num::NonZeroU8;

use compress::algorithm::{CompressionAlgorithm, CompressionAlgorithmParseError};
use compress::zlib::CompressionLevel;
use core::{
    bandwidth::BandwidthParseError,
    client::{BandwidthLimit, CompressionSetting},
    message::{Message, Role},
    rsync_error,
};

/// A resolved `--compress-level` value after clamping into the codec's range.
pub(crate) enum CompressLevelArg {
    /// The level resolved to "no compression" for the selected codec.
    Disable,
    /// A concrete compression level to apply.
    Level(CompressionLevel),
}

/// Outcome of parsing `--compress-choice=NAME`.
///
/// upstream: options.c:2031-2034 `if (compress_choice && strcasecmp(...,
/// "auto") != 0) parse_compress_choice(0); else compress_choice = NULL;` -
/// the literal `auto` is nulled out so normal codec negotiation runs, distinct
/// from `none` (which disables compression) and an explicit codec.
#[derive(Debug)]
pub(crate) enum CompressChoice {
    /// `--compress-choice=none` disables compression.
    Disabled,
    /// `--compress-choice=auto` nulls the choice; negotiation proceeds as if
    /// `--compress-choice` had not been supplied.
    Auto,
    /// An explicit codec selection.
    Codec(CompressionAlgorithm),
}

enum CompressionLevelParseError {
    Empty { original: String },
    Invalid { original: String, trimmed: String },
}

impl CompressionLevelParseError {
    fn into_flag_message(self) -> Message {
        match self {
            Self::Empty { original } | Self::Invalid { original, .. } => {
                rsync_error!(1, "--compress-level={} is invalid", original).with_role(Role::Client)
            }
        }
    }

    fn into_argument_message(self) -> Message {
        match self {
            Self::Empty { .. } => {
                rsync_error!(1, "compression level value must not be empty").with_role(Role::Client)
            }
            Self::Invalid { trimmed, .. } => rsync_error!(
                1,
                format!(
                    "invalid compression level '{trimmed}': compression level must be an integer"
                )
            )
            .with_role(Role::Client),
        }
    }
}

/// Parses the raw numeric `--compress-level` value.
///
/// Only genuinely malformed input (empty or non-numeric) is rejected, matching
/// upstream: `--compress-level` is a `POPT_ARG_INT`, so any integer is accepted
/// and only later clamped into the codec's range by
/// `token.c:init_compression_level()`.
fn parse_raw_level(argument: &OsStr) -> Result<i32, CompressionLevelParseError> {
    let original = argument.to_string_lossy().into_owned();
    let trimmed = original.trim().to_owned();

    if trimmed.is_empty() {
        return Err(CompressionLevelParseError::Empty { original });
    }

    trimmed
        .parse::<i32>()
        .map_err(|_| CompressionLevelParseError::Invalid { original, trimmed })
}

/// Clamps a raw level into `codec`'s range, mirroring
/// `token.c:init_compression_level()`. Never rejects an out-of-range value.
fn clamp_level(codec: CompressionAlgorithm, raw: i32) -> CompressLevelArg {
    match codec.clamp_level(raw) {
        Some(level) => CompressLevelArg::Level(level),
        None => CompressLevelArg::Disable,
    }
}

/// Parses `--compress-level=N` and clamps it into `codec`'s range.
///
/// Any integer is accepted; an out-of-range value is clamped rather than
/// rejected. Empty or non-numeric input yields a `--compress-level` error.
pub(crate) fn parse_compress_level(
    argument: &OsStr,
    codec: CompressionAlgorithm,
) -> Result<CompressLevelArg, Message> {
    parse_raw_level(argument)
        .map(|raw| clamp_level(codec, raw))
        .map_err(CompressionLevelParseError::into_flag_message)
}

/// Parses `--compress-choice=NAME` into a `CompressChoice`.
///
/// `auto` yields `CompressChoice::Auto`, `none` yields
/// `CompressChoice::Disabled`, and any other value is parsed as a codec name.
/// An empty or unknown name is rejected with exit code 4 (`RERR_UNSUPPORTED`).
pub(crate) fn parse_compress_choice(argument: &OsStr) -> Result<CompressChoice, Message> {
    let original = argument.to_string_lossy().into_owned();
    let trimmed = original.trim();

    if trimmed.is_empty() {
        // upstream: compat.c:190 - an unparsable compress name returns
        // RERR_UNSUPPORTED (errcode.h:28), not RERR_SYNTAX.
        return Err(
            rsync_error!(4, "--compress-choice={} is invalid", original).with_role(Role::Client)
        );
    }

    // upstream: options.c:2031 - only the literal `auto` (case-insensitive) is
    // nulled; anything else, including `auto,auto`, is passed to
    // parse_compress_choice and rejected if unknown.
    if trimmed.eq_ignore_ascii_case("auto") {
        return Ok(CompressChoice::Auto);
    }

    if trimmed.eq_ignore_ascii_case("none") {
        return Ok(CompressChoice::Disabled);
    }

    match trimmed.parse::<CompressionAlgorithm>() {
        Ok(algorithm) => Ok(CompressChoice::Codec(algorithm)),
        Err(err) => Err(render_compress_choice_error(err, trimmed)),
    }
}

fn render_compress_choice_error(err: CompressionAlgorithmParseError, trimmed: &str) -> Message {
    let display = if trimmed.is_empty() {
        err.input()
    } else {
        trimmed
    };
    #[allow(unused_mut)] // REASON: mutated when lz4 or zstd features are enabled
    let mut supported = vec!["zlib", "zlibx"];
    #[cfg(feature = "lz4")]
    {
        supported.push("lz4");
    }
    #[cfg(feature = "zstd")]
    {
        supported.push("zstd");
    }
    let supported_list = supported.join(", ");
    let rendered = format!(
        "invalid compression algorithm '{display}': supported values include {supported_list}"
    );
    // upstream: compat.c:190 - an unknown compress name returns
    // RERR_UNSUPPORTED (errcode.h:28), not RERR_SYNTAX.
    rsync_error!(4, rendered).with_role(Role::Client)
}

/// Parses `--bwlimit=RATE[:BURST]` into an optional bandwidth limit.
///
/// Returns `Ok(None)` when the value disables the limit. Invalid, too-small,
/// and too-large values are rejected with a `--bwlimit` error.
pub(crate) fn parse_bandwidth_limit(argument: &OsStr) -> Result<Option<BandwidthLimit>, Message> {
    let text = argument.to_string_lossy();
    match BandwidthLimit::parse(&text) {
        Ok(Some(limit)) => Ok(Some(limit)),
        Ok(None) => Ok(None),
        Err(BandwidthParseError::Invalid) => {
            Err(rsync_error!(1, "--bwlimit={} is invalid", text).with_role(Role::Client))
        }
        Err(BandwidthParseError::TooSmall) => Err(rsync_error!(
            1,
            "--bwlimit={} is too small (min: 512 or 0 for unlimited)",
            text
        )
        .with_role(Role::Client)),
        Err(BandwidthParseError::TooLarge) => {
            Err(rsync_error!(1, "--bwlimit={} is too large", text).with_role(Role::Client))
        }
    }
}

/// Upper bound for `--compress-threads`. Mirrors zstd's documented worker cap
/// and matches what upstream rsync 3.4.2 accepts before clamping. Upstream
/// silently clamps negative values to 0; we reject them so users get a clear
/// diagnostic instead.
const COMPRESS_THREADS_MAX: i32 = 64;

/// Parses `--compress-threads=N` into an optional worker count.
///
/// Returns `Ok(None)` for `0` (delegates the choice to zstd, matching
/// `do_compression_threads = 0` in upstream `options.c:90`). Returns
/// `Ok(Some(n))` for positive integers up to [`COMPRESS_THREADS_MAX`].
/// Rejects negative values, non-numeric input, and out-of-range positive
/// integers with a user-facing error message.
///
/// # Upstream Reference
///
/// - `options.c:772-773` - `{"compress-threads", 0, POPT_ARG_INT, &do_compression_threads, 0, 0, 0 }`.
/// - `options.c:2034-2035` - upstream clamps negative values to 0.
pub(crate) fn parse_compress_threads(argument: &OsStr) -> Result<Option<NonZeroU8>, Message> {
    let original = argument.to_string_lossy().into_owned();
    let trimmed = original.trim();

    if trimmed.is_empty() {
        return Err(
            rsync_error!(1, "--compress-threads={} is invalid", original).with_role(Role::Client),
        );
    }

    match trimmed.parse::<i32>() {
        Ok(0) => Ok(None),
        Ok(value @ 1..=COMPRESS_THREADS_MAX) => {
            let byte = u8::try_from(value).expect("range 1..=64 fits in u8");
            Ok(Some(
                NonZeroU8::new(byte).expect("range guarantees non-zero"),
            ))
        }
        Ok(_) => Err(rsync_error!(
            1,
            format!(
                "--compress-threads={} must be between 0 and {}",
                trimmed, COMPRESS_THREADS_MAX
            )
        )
        .with_role(Role::Client)),
        Err(_) => Err(
            rsync_error!(1, "--compress-threads={} is invalid", original).with_role(Role::Client),
        ),
    }
}

/// Parses `--compress-level` directly into a `CompressionSetting`.
///
/// Like `parse_compress_level` but folds the clamped result into an
/// enabled/disabled compression setting, with argument-style error messages.
pub(crate) fn parse_compress_level_argument(
    value: &OsStr,
    codec: CompressionAlgorithm,
) -> Result<CompressionSetting, Message> {
    match parse_raw_level(value).map(|raw| clamp_level(codec, raw)) {
        Ok(CompressLevelArg::Disable) => Ok(CompressionSetting::disabled()),
        Ok(CompressLevelArg::Level(level)) => Ok(CompressionSetting::level(level)),
        Err(error) => Err(error.into_argument_message()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn compress_level_zero_is_codec_dependent() {
        // upstream: token.c:56-105 init_compression_level() - `--compress-level=0`
        // is codec-dependent, NOT unconditionally disabled: for zlib the
        // off_level is Z_NO_COMPRESSION (0), so level 0 turns compression off
        // (CPRES_NONE); for zstd off_level is CLVL_NOT_SPECIFIED and a literal 0
        // maps to the default level, so compression stays on. The clamp is
        // applied against the resolved codec, mirroring upstream exactly.
        assert!(matches!(
            parse_compress_level(OsStr::new("0"), CompressionAlgorithm::Zlib),
            Ok(CompressLevelArg::Disable)
        ));
        #[cfg(feature = "zstd")]
        assert!(matches!(
            parse_compress_level(OsStr::new("0"), CompressionAlgorithm::Zstd),
            Ok(CompressLevelArg::Level(_))
        ));
    }

    #[test]
    fn parse_compress_choice_none_disables_compression() {
        let parsed = parse_compress_choice(OsStr::new("none"));
        assert!(matches!(parsed, Ok(CompressChoice::Disabled)));
    }

    #[test]
    fn parse_compress_choice_auto_is_nulled() {
        // upstream: options.c:2031-2034 - the literal `auto` (case-insensitive)
        // is nulled so normal codec negotiation runs; it is neither a disable
        // nor an explicit codec.
        assert!(matches!(
            parse_compress_choice(OsStr::new("auto")),
            Ok(CompressChoice::Auto)
        ));
        assert!(matches!(
            parse_compress_choice(OsStr::new("AUTO")),
            Ok(CompressChoice::Auto)
        ));
    }

    #[test]
    fn parse_compress_choice_auto_auto_is_rejected() {
        // upstream: options.c:2031 only special-cases the exact token `auto`;
        // `auto,auto` falls through to parse_compress_choice and is rejected
        // as an unknown compress name (RERR_UNSUPPORTED, exit 4).
        let error = parse_compress_choice(OsStr::new("auto,auto")).expect_err("auto,auto rejected");
        assert_eq!(error.code(), Some(4));
    }

    #[test]
    fn parse_compress_choice_accepts_zlib_aliases() {
        let parsed = parse_compress_choice(OsStr::new("zlib"));
        assert!(matches!(
            parsed,
            Ok(CompressChoice::Codec(CompressionAlgorithm::Zlib))
        ));

        let alias = parse_compress_choice(OsStr::new(" zlibx "));
        assert!(matches!(
            alias,
            Ok(CompressChoice::Codec(CompressionAlgorithm::Zlib))
        ));
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn parse_compress_choice_accepts_zstd() {
        let parsed = parse_compress_choice(OsStr::new("zstd"));
        assert!(matches!(
            parsed,
            Ok(CompressChoice::Codec(CompressionAlgorithm::Zstd))
        ));
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn parse_compress_choice_accepts_lz4() {
        let parsed = parse_compress_choice(OsStr::new("lz4"));
        assert!(matches!(
            parsed,
            Ok(CompressChoice::Codec(CompressionAlgorithm::Lz4))
        ));
    }

    #[test]
    fn parse_compress_choice_rejects_unknown_algorithm() {
        let error =
            parse_compress_choice(OsStr::new("brotli")).expect_err("brotli should be rejected");
        let message = error.to_string();
        assert!(message.contains("invalid compression algorithm"));
        assert!(message.contains("brotli"));
    }

    #[test]
    fn parse_compress_threads_accepts_positive_value() {
        let parsed = parse_compress_threads(OsStr::new("4")).expect("4 should parse");
        assert_eq!(parsed.map(NonZeroU8::get), Some(4));
    }

    #[test]
    fn parse_compress_threads_zero_means_zstd_default() {
        let parsed = parse_compress_threads(OsStr::new("0")).expect("0 should parse");
        assert!(parsed.is_none());
    }

    #[test]
    fn parse_compress_threads_rejects_negative() {
        let error =
            parse_compress_threads(OsStr::new("-1")).expect_err("negative should be rejected");
        let rendered = error.to_string();
        assert!(rendered.contains("--compress-threads=-1 must be between 0 and 64"));
    }

    #[test]
    fn parse_compress_threads_rejects_non_numeric() {
        let error =
            parse_compress_threads(OsStr::new("abc")).expect_err("non-numeric should be rejected");
        let rendered = error.to_string();
        assert!(rendered.contains("--compress-threads=abc is invalid"));
    }

    #[test]
    fn parse_compress_threads_rejects_above_cap() {
        let error = parse_compress_threads(OsStr::new("99")).expect_err("99 should exceed the cap");
        let rendered = error.to_string();
        assert!(rendered.contains("must be between 0 and 64"));
    }
}
