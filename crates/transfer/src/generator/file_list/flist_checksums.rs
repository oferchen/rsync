//! Sender-side `--checksum` sums for the file list.
//!
//! upstream: flist.c:1815 - `always_checksum && am_sender && S_ISREG(st.st_mode)`
//! makes `make_file()` call `file_checksum()` (checksum.c:419, unseeded) and
//! store the result in `F_SUM(file)`, which `send_file_entry()` writes into the
//! flist (flist.c:1228). The receiver's quick check (generator.c:639,
//! `quick_check_ok`) compares its own `file_checksum()` of the basis against
//! this value to decide the file is unchanged.
//!
//! Upstream hashes one file at a time while listing. Here every regular file
//! of a list or sub-list is hashed in one parallel batch just before the list
//! is encoded, through the shared [`checksums::parallel`] service. Each sum is
//! the exact negotiated whole-file digest and the entries are encoded in list
//! order, so the wire bytes are unchanged.

use std::ops::Range;
use std::path::PathBuf;

use checksums::parallel::{FileJob, hash_files};

use crate::delta_apply::ChecksumVerifier;
use crate::generator::GeneratorContext;

impl GeneratorContext {
    /// Stores the `--checksum` sum on every regular file in `range` that has
    /// none yet.
    ///
    /// A file that cannot be read in full keeps no sum, so the list carries the
    /// all-zero placeholder and the receiver transfers it.
    pub(in crate::generator) fn fill_flist_checksums(&mut self, range: Range<usize>) {
        if !self.config.flags.checksum {
            return;
        }
        let targets: Vec<(usize, PathBuf)> = range
            .filter_map(|index| {
                let entry = &self.file_list[index];
                if !entry.is_file() || entry.checksum().is_some() {
                    return None;
                }
                let base = self.source_bases.get(index)?;
                Some((index, base.join(entry.path())))
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        let jobs: Vec<FileJob<'_>> = targets
            .iter()
            .map(|(index, path)| FileJob {
                path,
                len: self.file_list[*index].size(),
            })
            .collect();
        let algorithm = self.get_checksum_algorithm();
        let digests = hash_files(&jobs, || ChecksumVerifier::for_algorithm(algorithm));
        for ((index, _), digest) in targets.iter().zip(digests) {
            if let Ok(digest) = digest {
                self.file_list[*index].set_checksum(digest.as_bytes().to_vec());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! WHY this matters: without the sender populating the checksum, the flist
    //! carries an all-zero sum, the receiver's `-c` quick check can never match
    //! a content-identical basis, and every unchanged file is re-transferred.
    //! These tests pin that the sender emits the exact unseeded digest the
    //! receiver recomputes, for every file of a batch, in list order.
    use crate::config::ServerConfig;
    use crate::delta_apply::ChecksumVerifier;
    use crate::generator::GeneratorContext;
    use crate::handshake::HandshakeResult;
    use crate::role::ServerRole;
    use protocol::ProtocolVersion;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn generator(checksum: bool) -> GeneratorContext {
        let handshake = HandshakeResult {
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        };
        let mut config = ServerConfig {
            role: ServerRole::Generator,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: vec![OsString::from(".")],
            ..Default::default()
        };
        config.flags.checksum = checksum;
        config.flags.numeric_ids = crate::NumericIds::Explicit;
        GeneratorContext::new_for_test(&handshake, config)
    }

    /// The exact unseeded hash the receiver recomputes in its quick check.
    fn expected_sum(ctx: &GeneratorContext, data: &[u8]) -> Vec<u8> {
        let mut verifier = ChecksumVerifier::for_algorithm(ctx.get_checksum_algorithm());
        verifier.update(data);
        let mut digest = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        let len = verifier.finalize_into(&mut digest);
        digest[..len].to_vec()
    }

    fn push(ctx: &mut GeneratorContext, dir: &Path, name: &str) {
        let path = dir.join(name);
        let meta = std::fs::symlink_metadata(&path).unwrap();
        let entry = ctx
            .create_entry(
                &path,
                PathBuf::from(name),
                &fast_io::EntryMetadata::from(meta),
                2,
            )
            .unwrap();
        ctx.push_file_item(entry, path);
    }

    /// Every regular file in the batch, including an empty one, carries its
    /// own digest; a directory carries none.
    #[test]
    fn every_regular_file_of_the_batch_gets_its_own_sum() {
        let tmp = TempDir::new().unwrap();
        let files: [(&str, &[u8]); 3] = [
            ("a.bin", b"the quick brown fox"),
            ("b.bin", b"jumps over the lazy dog"),
            ("empty", b""),
        ];
        for (name, data) in files {
            std::fs::write(tmp.path().join(name), data).unwrap();
        }
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        let mut ctx = generator(true);
        for name in ["a.bin", "b.bin", "empty", "sub"] {
            push(&mut ctx, tmp.path(), name);
        }
        ctx.fill_flist_checksums(0..4);
        for (index, (_, data)) in files.iter().enumerate() {
            assert_eq!(
                ctx.file_list[index].checksum().map(<[u8]>::to_vec),
                Some(expected_sum(&ctx, data)),
            );
        }
        assert_eq!(ctx.file_list[3].checksum(), None);
    }

    /// Only the requested range is hashed, so a sub-list never fills entries
    /// that belong to a later one.
    #[test]
    fn only_the_requested_range_is_filled() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("first"), b"one").unwrap();
        std::fs::write(tmp.path().join("second"), b"two").unwrap();
        let mut ctx = generator(true);
        push(&mut ctx, tmp.path(), "first");
        push(&mut ctx, tmp.path(), "second");
        ctx.fill_flist_checksums(1..2);
        assert_eq!(ctx.file_list[0].checksum(), None);
        assert_eq!(
            ctx.file_list[1].checksum().map(<[u8]>::to_vec),
            Some(expected_sum(&ctx, b"two"))
        );
    }

    /// Without `--checksum` the list carries no per-file sum
    /// (upstream gates on `always_checksum`).
    #[test]
    fn without_checksum_flag_no_sum_is_stored() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("payload.bin"), b"data").unwrap();
        let mut ctx = generator(false);
        push(&mut ctx, tmp.path(), "payload.bin");
        ctx.fill_flist_checksums(0..1);
        assert_eq!(ctx.file_list[0].checksum(), None);
    }

    /// A file removed after listing keeps no sum, so it is transferred rather
    /// than wrongly matched.
    #[test]
    fn unreadable_file_keeps_no_sum() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("gone"), b"data").unwrap();
        let mut ctx = generator(true);
        push(&mut ctx, tmp.path(), "gone");
        std::fs::remove_file(tmp.path().join("gone")).unwrap();
        ctx.fill_flist_checksums(0..1);
        assert_eq!(ctx.file_list[0].checksum(), None);
    }
}
