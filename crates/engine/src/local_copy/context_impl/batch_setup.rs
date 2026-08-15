// Derivation of the per-session batch-recording and compression components
// from `LocalCopyOptions`. Each function owns exactly one component so that
// `CopyContext::new` stays an assembly step rather than a derivation.

/// Creates the scratch buffer that holds per-file delta data until the flist
/// end marker has been written.
///
/// Returns `None` when `--write-batch` is not in effect.
///
/// The flist entries go straight to the batch writer during the walk, but the
/// file data (NDX + iflags + sum_head + tokens + checksum) must follow the
/// flist end marker in the batch stream, so it is buffered until then.
fn build_batch_delta_buffer(options: &LocalCopyOptions) -> Option<std::io::Cursor<Vec<u8>>> {
    options
        .get_batch_writer()
        .map(|_| std::io::Cursor::new(Vec::new()))
}

/// Creates the NDX codec for the batch stream, matching the protocol version
/// recorded in the batch header.
///
/// Returns `None` when `--write-batch` is not in effect.
fn build_batch_ndx_codec(options: &LocalCopyOptions) -> Option<protocol::codec::NdxCodecEnum> {
    options.get_batch_writer().map(|batch_writer_arc| {
        let guard = batch_writer_arc.lock().expect("batch writer mutex poisoned");
        let proto_version = guard.config().protocol_version;
        drop(guard);
        protocol::codec::NdxCodecEnum::new(proto_version as u8)
    })
}

/// Creates the file-list writer that encodes flist entries into the batch
/// stream.
///
/// Returns `None` when `--write-batch` is not in effect.
///
/// Every per-entry field this writer emits must match what the reader expects.
/// Unlike upstream - whose batch file is a byte tee of one real stream
/// (`io.c:1962-1963`, armed at `io.c:2528-2529`), so a writer/reader
/// disagreement is unspellable - the local `--write-batch` path re-encodes the
/// flist with a second encoder. The preserve flags therefore come from the
/// stream flags this same writer already recorded in the header, because the
/// header is what the reader configures itself from
/// (`batch/src/reader/flist.rs`); deriving them from the live options instead
/// lets the two sides drift into an undecodable batch.
fn build_batch_flist_writer(
    options: &LocalCopyOptions,
) -> Option<protocol::flist::FileListWriter> {
    options.get_batch_writer().map(|batch_writer_arc| {
        let guard = batch_writer_arc.lock().expect("batch writer mutex poisoned");
        let proto_version = guard.config().protocol_version;
        let compat_flags_val = guard.config().compat_flags;
        let checksum_seed = guard.config().checksum_seed;
        let stream_flags = *guard.stream_flags();
        drop(guard);

        let protocol = protocol::ProtocolVersion::try_from(proto_version as u8)
            .unwrap_or(protocol::ProtocolVersion::NEWEST);
        // upstream: io.c:start_write_batch() - compat_flags are written to the batch
        // header. The flist writer must use the same compat_flags to ensure the wire
        // encoding (varint flags, safe file list) matches what the reader will expect
        // when decoding the batch body.
        let writer = if let Some(cf) = compat_flags_val {
            let compat = protocol::CompatibilityFlags::from_bits(cf as u32);
            protocol::flist::FileListWriter::with_compat_flags(protocol, compat)
        } else {
            protocol::flist::FileListWriter::new(protocol)
        };

        let writer = writer
            .with_preserve_uid(stream_flags.preserve_uid)
            .with_preserve_gid(stream_flags.preserve_gid)
            .with_preserve_links(stream_flags.preserve_links)
            .with_preserve_devices(stream_flags.preserve_devices)
            // upstream: batch.c flag_ptr[] bit 4 is `preserve_devices` and
            // covers --specials too (upstream `-D` = both), so the reader
            // derives specials from that one bit. Deriving it here from
            // `options.specials_enabled()` instead would desync a
            // `--specials --no-devices` batch.
            .with_preserve_specials(stream_flags.preserve_devices)
            .with_preserve_hard_links(stream_flags.preserve_hard_links)
            .with_preserve_acls(stream_flags.preserve_acls)
            .with_preserve_xattrs(stream_flags.preserve_xattrs)
            // --atimes / --crtimes have no flag_ptr[] bit; the reader takes
            // them from the replay invocation instead (reader/flist.rs), so
            // the writer keeps using the live options here.
            .with_preserve_atimes(options.preserve_atimes())
            .with_preserve_crtimes(options.preserve_crtimes())
            // The batch id-list trailer (write_batch_id_lists) emits bare
            // terminators without names, so owner names must ride inline in
            // every entry regardless of inc_recurse. Keep XMIT_*_NAME_FOLLOWS
            // always enabled for the batch flist writer.
            .with_name_follows(true);

        // upstream: flist.c:162 - under always_checksum every regular-file
        // entry carries a trailing digest. The reader re-derives the length
        // from the protocol version (batch/src/reader/flist.rs
        // default_flist_csum_len), so the two must agree or the reader
        // walks off the end of each entry.
        if stream_flags.always_checksum {
            writer
                .with_always_checksum(batch::reader::default_flist_csum_len(proto_version))
                .with_checksum_seed(checksum_seed)
        } else {
            writer
        }
    })
}

/// Creates the compressed-token encoder for the batch stream.
///
/// Returns `None` unless both `--write-batch` and compression are in effect.
///
/// upstream: batch.c:68 - stream-flag bit 8 records `do_compression`, and the
/// batch file is a tee of the wire stream, so a batch written under `-z`
/// carries `token.c:send_deflated_token()` framing. The codec is always zlib:
/// `compat.c:414 getenv_nstr()` forces the compression list to "zlib" whenever
/// `write_batch` is set ("When writing a batch file, we always negotiate an
/// old-style choice"), which is also what `compat.c:194-195
/// parse_compress_choice()` assumes on `--read-batch`.
fn build_batch_token_encoder(
    options: &LocalCopyOptions,
) -> Option<protocol::wire::CompressedTokenEncoder> {
    options.get_batch_writer().and_then(|batch_writer_arc| {
        if !options.compress_enabled() {
            return None;
        }
        let proto_version = batch_writer_arc
            .lock()
            .expect("batch writer mutex poisoned")
            .config()
            .protocol_version;
        Some(protocol::wire::CompressedTokenEncoder::new(
            options.compression_level(),
            proto_version as u32,
        ))
    })
}

/// Creates the adaptive compression-level controller seeded from the
/// configured level.
///
/// Returns `None` when compression or adaptive levelling is off, or when the
/// negotiated algorithm has no controller.
fn build_adaptive_level_controller(
    options: &LocalCopyOptions,
) -> Option<compress::strategy::adaptive_level::AdaptiveLevelController> {
    if !(options.compress_enabled() && options.adaptive_compress_enabled()) {
        return None;
    }
    let level_i32 = match options.compression_level() {
        CompressionLevel::None => 0,
        CompressionLevel::Fast => 1,
        CompressionLevel::Default => 6,
        CompressionLevel::Best => 9,
        CompressionLevel::Precise(v) => i32::from(v.get()),
        // upstream: token.c:73 - preserve zstd's negative "fast" levels.
        CompressionLevel::PreciseSigned(v) => v,
    };
    match options.compression_algorithm() {
        CompressionAlgorithm::Zlib => Some(
            compress::strategy::adaptive_level::AdaptiveLevelController::for_zlib(level_i32),
        ),
        #[cfg(feature = "zstd")]
        CompressionAlgorithm::Zstd => Some(
            compress::strategy::adaptive_level::AdaptiveLevelController::for_zstd(level_i32),
        ),
        _ => None,
    }
}
