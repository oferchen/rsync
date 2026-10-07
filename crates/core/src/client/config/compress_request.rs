use compress::zlib::CompressionLevel;
use protocol::CompressionAlgorithm;

/// `do_compression` as upstream leaves it once option parsing ends.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum RequestedCodec {
    /// `CPRES_NONE`: compression stays off.
    #[default]
    Off,
    /// `CPRES_AUTO`: on, codec left to negotiation (`--compress-level` alone).
    Auto,
    /// A codec fixed by `-z` or by `compress_choice`.
    Fixed(CompressionAlgorithm),
}

/// The compression request the client forwards to the remote server.
///
/// Mirrors upstream's `do_compression`, `compress_choice` and
/// `do_compression_level` once `options.c:2131-2140` ran, before any
/// negotiation. `server_options()` derives the compact `z` and the long-form
/// compression arguments from exactly this state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompressRequest {
    codec: RequestedCodec,
    choice: Option<String>,
    level: Option<i32>,
}

impl CompressRequest {
    /// Resolves the request from the parsed options.
    ///
    /// `z_given` reports a `-z` that no later `--no-compress` cancelled.
    /// `choice` is the effective `compress_choice`: the last of
    /// `--old-compress`, `--new-compress` and `--compress-choice`, `zlibx` for
    /// a repeated `-z`, and never `auto`. `level` is the raw, unclamped
    /// `--compress-level`.
    ///
    /// Returns `None` when `choice` names no known codec.
    #[must_use]
    pub fn resolve(z_given: bool, choice: Option<&str>, level: Option<i32>) -> Option<Self> {
        // upstream: compat.c:182-212 parse_compress_choice(0) - the lookup is
        // case-insensitive and a `none` choice is renamed to "none".
        let (codec, choice) = match choice {
            Some(name) => match CompressionAlgorithm::parse(&name.to_ascii_lowercase()).ok()? {
                CompressionAlgorithm::None => (RequestedCodec::Off, Some("none".to_owned())),
                algorithm => (RequestedCodec::Fixed(algorithm), Some(name.to_owned())),
            },
            None if z_given => (RequestedCodec::Fixed(CompressionAlgorithm::Zlib), None),
            None => (RequestedCodec::Off, None),
        };
        // upstream: options.c:2138-2140 - a level alone turns compression on.
        let codec = match codec {
            RequestedCodec::Off if level.is_some() => RequestedCodec::Auto,
            codec => codec,
        };
        Some(Self {
            codec,
            choice,
            level,
        })
    }

    /// Reports whether the compact server flag string carries `z`.
    ///
    /// upstream: options.c:2898 - only for `do_compression == CPRES_ZLIB`.
    #[must_use]
    pub fn packs_z(&self) -> bool {
        self.codec == RequestedCodec::Fixed(CompressionAlgorithm::Zlib)
    }

    /// Returns the forwarded `--compress-level=N` argument, if any.
    ///
    /// upstream: options.c:2931-2934 - sent unclamped whenever compression is
    /// on, `CPRES_AUTO` included, and a level was given.
    #[must_use]
    pub fn level_arg(&self) -> Option<String> {
        self.level()
            .map(|level| format!("--compress-level={level}"))
    }

    /// Returns the raw `do_compression_level` both peers apply, if forwarded.
    ///
    /// upstream: token.c:55 init_compression_level() resolves this same value
    /// on each side after negotiation, so the local half must carry it too.
    #[must_use]
    pub fn level(&self) -> Option<i32> {
        match self.codec {
            RequestedCodec::Off => None,
            _ => self.level,
        }
    }

    /// Returns the forwarded codec-selection argument, if any.
    ///
    /// upstream: options.c:2994-2999 - zlibx travels as `--new-compress`, an
    /// explicit zlib as `--old-compress`, anything else (`none` included) as
    /// the verbatim `--compress-choice=NAME`.
    #[must_use]
    pub fn choice_arg(&self) -> Option<String> {
        let choice = self.choice.as_deref()?;
        Some(match self.codec {
            RequestedCodec::Fixed(CompressionAlgorithm::ZlibX) => "--new-compress".to_owned(),
            RequestedCodec::Fixed(CompressionAlgorithm::Zlib) => "--old-compress".to_owned(),
            _ => format!("--compress-choice={choice}"),
        })
    }

    /// Returns the codec an explicit `compress_choice` pins on both peers.
    ///
    /// upstream: compat.c:543 - a set `compress_choice` skips the vstring
    /// negotiation, and a `none` choice stays set (compat.c:209-211) even when
    /// a level re-enables `do_compression`. `None` when no choice was given.
    #[must_use]
    pub fn pinned_codec(&self) -> Option<CompressionAlgorithm> {
        self.choice.as_ref()?;
        Some(match self.codec {
            RequestedCodec::Fixed(algorithm) => algorithm,
            RequestedCodec::Off | RequestedCodec::Auto => CompressionAlgorithm::None,
        })
    }
}

/// Returns the signed `do_compression_level` a [`CompressionLevel`] stands for.
pub(super) fn level_value(level: CompressionLevel) -> i32 {
    match level {
        CompressionLevel::None => 0,
        CompressionLevel::Fast => 1,
        CompressionLevel::Default => 6,
        CompressionLevel::Best => 9,
        CompressionLevel::Precise(n) => i32::from(n.get()),
        CompressionLevel::PreciseSigned(v) => v,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One command line: its parsed inputs and the server argv rsync 3.5.1
    /// sends for it (compact `z`, long-form args in upstream order).
    struct Row {
        cmdline: &'static str,
        z: bool,
        choice: Option<&'static str>,
        level: Option<i32>,
        packs_z: bool,
        args: &'static [&'static str],
    }

    const fn row(
        cmdline: &'static str,
        z: bool,
        choice: Option<&'static str>,
        level: Option<i32>,
        packs_z: bool,
        args: &'static [&'static str],
    ) -> Row {
        Row {
            cmdline,
            z,
            choice,
            level,
            packs_z,
            args,
        }
    }

    // Expected columns recorded from rsync 3.5.1 through a --rsh that logs
    // its argv; a wrong peer codec silently changes the token stream format.
    const GOLDEN: &[Row] = &[
        row("-z", true, None, None, true, &[]),
        row("-zz", true, Some("zlibx"), None, false, &["--new-compress"]),
        row(
            "--old-compress",
            false,
            Some("zlib"),
            None,
            true,
            &["--old-compress"],
        ),
        row(
            "--new-compress",
            false,
            Some("zlibx"),
            None,
            false,
            &["--new-compress"],
        ),
        row(
            "--zc=zlibx",
            false,
            Some("zlibx"),
            None,
            false,
            &["--new-compress"],
        ),
        row(
            "--zc=ZLIBX",
            false,
            Some("ZLIBX"),
            None,
            false,
            &["--new-compress"],
        ),
        row(
            "--zc=Zlib",
            false,
            Some("Zlib"),
            None,
            true,
            &["--old-compress"],
        ),
        row(
            "--zc=zstd",
            false,
            Some("zstd"),
            None,
            false,
            &["--compress-choice=zstd"],
        ),
        row(
            "--zc=Zstd",
            false,
            Some("Zstd"),
            None,
            false,
            &["--compress-choice=Zstd"],
        ),
        row(
            "--zc=lz4",
            false,
            Some("lz4"),
            None,
            false,
            &["--compress-choice=lz4"],
        ),
        row(
            "--zc=none",
            false,
            Some("none"),
            None,
            false,
            &["--compress-choice=none"],
        ),
        row(
            "--zc=NONE",
            false,
            Some("NONE"),
            None,
            false,
            &["--compress-choice=none"],
        ),
        row("--zc=auto", false, None, None, false, &[]),
        row("-z --zc=auto", true, None, None, true, &[]),
        row(
            "--compress-level=3",
            false,
            None,
            Some(3),
            false,
            &["--compress-level=3"],
        ),
        row(
            "--compress-level=0",
            false,
            None,
            Some(0),
            false,
            &["--compress-level=0"],
        ),
        row(
            "-z --compress-level=3",
            true,
            None,
            Some(3),
            true,
            &["--compress-level=3"],
        ),
        row(
            "--zc=none --compress-level=3",
            false,
            Some("none"),
            Some(3),
            false,
            &["--compress-level=3", "--compress-choice=none"],
        ),
        row(
            "--zc=zstd --compress-level=0",
            false,
            Some("zstd"),
            Some(0),
            false,
            &["--compress-level=0", "--compress-choice=zstd"],
        ),
        row(
            "-zz --compress-level=5",
            true,
            Some("zlibx"),
            Some(5),
            false,
            &["--compress-level=5", "--new-compress"],
        ),
        row(
            "-zz --old-compress",
            true,
            Some("zlib"),
            None,
            true,
            &["--old-compress"],
        ),
        row(
            "--no-compress --compress-level=2",
            false,
            None,
            Some(2),
            false,
            &["--compress-level=2"],
        ),
        row("-z --no-compress", false, None, None, false, &[]),
    ];

    #[test]
    fn server_args_match_upstream_for_every_option_combination() {
        for row in GOLDEN {
            let request = CompressRequest::resolve(row.z, row.choice, row.level)
                .unwrap_or_else(|| panic!("{}: known codec", row.cmdline));
            let args: Vec<String> = request
                .level_arg()
                .into_iter()
                .chain(request.choice_arg())
                .collect();
            assert_eq!(request.packs_z(), row.packs_z, "{}: compact z", row.cmdline);
            assert_eq!(args, row.args, "{}: long-form args", row.cmdline);
        }
    }

    #[test]
    fn zlibx_is_pinned_distinct_from_zlib() {
        let zlibx = CompressRequest::resolve(true, Some("zlibx"), None).expect("known codec");
        assert_eq!(zlibx.pinned_codec(), Some(CompressionAlgorithm::ZlibX));
        let plain = CompressRequest::resolve(true, None, Some(6)).expect("known codec");
        assert_eq!(plain.pinned_codec(), None, "-z negotiates its codec");
        // A level must not reopen the vstring exchange the `none` choice skips.
        let none = CompressRequest::resolve(false, Some("none"), Some(3)).expect("known codec");
        assert_eq!(none.pinned_codec(), Some(CompressionAlgorithm::None));
        let off = CompressRequest::resolve(false, None, None).expect("known codec");
        assert_eq!(off.pinned_codec(), None, "no choice pins no codec");
    }

    #[test]
    fn unknown_choice_is_rejected() {
        assert!(CompressRequest::resolve(false, Some("bogus"), None).is_none());
    }
}
