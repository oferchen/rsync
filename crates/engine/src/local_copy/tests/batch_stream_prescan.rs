// A streamed `--write-batch` lists directories ahead of the walk the way
// rsync's sender builds sub-lists ahead of file data (sender.c:523-555,
// MIN_FILECNT_LOOKAHEAD). How far ahead it lists must change nothing in the
// batch, must keep what it holds bounded, and must use the same filter stack
// the walk would have used on entering each directory.

mod batch_stream_prescan {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::batch::{BatchConfig, BatchFlags, BatchMode, BatchWriter};
    use crate::local_copy::batch_order::last_peak_held_bytes;
    use crate::local_copy::{PrescanPolicy, last_peak_listed_entries, set_prescan_policy};
    use protocol::CompatibilityFlags;

    const THRESHOLD: usize = 1 << 20;

    const COMPAT_FLAGS: CompatibilityFlags = CompatibilityFlags::INC_RECURSE
        .union(CompatibilityFlags::SAFE_FILE_LIST)
        .union(CompatibilityFlags::AVOID_XATTR_OPTIMIZATION)
        .union(CompatibilityFlags::CHECKSUM_SEED_FIX)
        .union(CompatibilityFlags::INPLACE_PARTIAL_DIR)
        .union(CompatibilityFlags::VARINT_FLIST_FLAGS);

    struct Recorded {
        batch: Vec<u8>,
        peak_held_bytes: usize,
        peak_listed_entries: usize,
    }

    fn noise(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    /// Copies `source/` into a fresh destination writing a streamed batch,
    /// listing ahead as `policy` says.
    fn record(source: &Path, policy: PrescanPolicy, filters: Option<FilterProgram>) -> Recorded {
        let temp = tempdir().expect("tempdir");
        let batch_path = temp.path().join("batch");
        let config =
            BatchConfig::new(BatchMode::Write, batch_path.to_string_lossy().into_owned(), 32)
                .with_compat_flags(COMPAT_FLAGS.bits() as i32)
                .with_checksum_seed(1);
        let mut writer = BatchWriter::new(config).expect("batch writer");
        writer
            .write_header(BatchFlags {
                recurse: true,
                ..Default::default()
            })
            .expect("batch header");
        let writer = Arc::new(Mutex::new(writer));
        let options = LocalCopyOptions::default()
            .recursive(true)
            .with_filter_program(filters)
            .batch_writer(Some(Arc::clone(&writer)));
        let mut src = source.as_os_str().to_owned();
        src.push("/");
        let operands = vec![src, temp.path().join("dst").into_os_string()];
        set_prescan_policy(policy);
        let run = LocalCopyPlan::from_operands(&operands)
            .expect("plan")
            .execute_with_options(LocalCopyExecution::Apply, options);
        set_prescan_policy(PrescanPolicy::Lookahead);
        run.expect("batch-writing copy");
        Arc::try_unwrap(writer)
            .expect("writer uniquely owned")
            .into_inner()
            .expect("writer mutex")
            .finalize()
            .expect("finalize batch");
        Recorded {
            batch: fs::read(&batch_path).expect("read batch"),
            peak_held_bytes: last_peak_held_bytes(),
            peak_listed_entries: last_peak_listed_entries(),
        }
    }

    fn dir_merge(name: &str) -> FilterProgram {
        FilterProgram::new([FilterProgramEntry::DirMerge(DirMergeRule::new(
            name,
            DirMergeOptions::default(),
        ))])
        .expect("filter program")
    }

    /// More than `MIN_FILECNT_LOOKAHEAD` entries over nested directories, with
    /// per-directory merge files whose rules apply only below them.
    fn build_filtered_tree(root: &Path) {
        fs::write(root.join(".rsync-filter"), "- *.skip\n").expect("root filter");
        for d in 0..30 {
            let dir = root.join(format!("d{d:02}"));
            fs::create_dir_all(dir.join("inner/deeper")).expect("mkdir");
            for f in 0..35 {
                fs::write(dir.join(format!("f{f:02}")), format!("{d} {f}\n")).expect("write");
            }
            fs::write(dir.join("x.skip"), b"skip\n").expect("write");
            fs::write(dir.join("inner/a.tmp"), b"tmp\n").expect("write");
            fs::write(dir.join("inner/deeper/b.tmp"), b"tmp\n").expect("write");
            fs::write(dir.join("inner/deeper/c"), b"c\n").expect("write");
        }
        fs::write(root.join("d07/inner/.rsync-filter"), "- *.tmp\n").expect("inner filter");
    }

    #[test]
    fn batch_bytes_do_not_depend_on_how_far_the_walk_lists_ahead() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("src");
        fs::create_dir(&source).expect("mkdir");
        build_filtered_tree(&source);

        let never = record(&source, PrescanPolicy::Never, Some(dir_merge(".rsync-filter")));
        let lookahead = record(&source, PrescanPolicy::Lookahead, Some(dir_merge(".rsync-filter")));
        let everything = record(&source, PrescanPolicy::Everything, Some(dir_merge(".rsync-filter")));
        assert_eq!(lookahead.batch, never.batch, "listing ahead changes no byte");
        assert_eq!(everything.batch, never.batch, "listing everything ahead changes no byte");
        // The merge files were applied: excluded names never reach the batch,
        // while the same names outside the filtered directory do.
        let has = |needle: &[u8]| everything.batch.windows(needle.len()).any(|w| w == needle);
        assert!(!has(b"x.skip"), "the root merge file excludes *.skip everywhere");
        assert!(has(b"a.tmp"), "*.tmp is listed where no merge file excludes it");
        assert!(
            everything.peak_listed_entries > lookahead.peak_listed_entries,
            "the policies really list differently: {} vs {}",
            everything.peak_listed_entries,
            lookahead.peak_listed_entries
        );
    }

    #[test]
    fn a_merge_file_applies_only_below_its_directory_when_listed_ahead() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("src");
        fs::create_dir(&source).expect("mkdir");
        build_filtered_tree(&source);
        let everything = record(&source, PrescanPolicy::Everything, Some(dir_merge(".rsync-filter")));
        let never = record(&source, PrescanPolicy::Never, Some(dir_merge(".rsync-filter")));
        assert_eq!(everything.batch, never.batch);
        // d07/inner excludes *.tmp below itself only: 29 of each name stay.
        let count = |needle: &[u8]| everything.batch.windows(needle.len()).filter(|w| *w == needle).count();
        assert_eq!(count(b"a.tmp"), 29);
        assert_eq!(count(b"b.tmp"), 29);
    }

    /// Files after subdirectories at three levels, names whose byte order and
    /// `f_name_cmp` order disagree, and a large file listed before every
    /// subdirectory of the root (the `src/sub/z` shape).
    fn build_order_tree(root: &Path) -> usize {
        let small = 600 * 1024;
        let files: &[(&str, usize)] = &[
            ("a-b", small),
            ("a.txt", small),
            ("big", 3 << 20),
            ("zz", small),
            ("a/a", small),
            ("a/a.txt", small),
            ("a/z", small),
            ("a/m/x", small),
            ("a/m/y", small),
            ("a/m/deep/f", small),
            ("b/q", small),
            ("b/r", small),
            ("b/a-b/s", small),
            ("sub/z", 10),
        ];
        for (i, (name, len)) in files.iter().enumerate() {
            let path = root.join(name);
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            fs::write(&path, noise(*len, i as u64 + 7)).expect("write");
        }
        small
    }

    #[test]
    fn held_record_bytes_stay_within_one_record_past_the_threshold() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("src");
        fs::create_dir(&source).expect("mkdir");
        let largest_held_record = build_order_tree(&source);
        // A record under the threshold, plus the one being built up to the
        // threshold and one more literal chunk.
        let bound = largest_held_record + THRESHOLD + 64 * 1024;

        let streamed = record(&source, PrescanPolicy::Lookahead, None);
        assert!(
            streamed.peak_held_bytes <= bound,
            "held {} bytes, bound {bound}",
            streamed.peak_held_bytes
        );
        // Without listing ahead, the large file waits whole for the root's
        // sub-lists: the fixture can tell the two apart.
        let unlisted = record(&source, PrescanPolicy::Never, None);
        assert!(
            unlisted.peak_held_bytes > bound,
            "without listing ahead {} bytes are held",
            unlisted.peak_held_bytes
        );
        assert_eq!(streamed.batch, unlisted.batch);
    }

    #[test]
    fn listing_ahead_holds_no_more_than_the_lookahead() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("src");
        fs::create_dir(&source).expect("mkdir");
        fs::write(source.join("big"), noise(2 << 20, 3)).expect("write");
        let per_dir = 100;
        for d in 0..40 {
            let dir = source.join(format!("d{d:02}"));
            fs::create_dir(&dir).expect("mkdir");
            for f in 0..per_dir {
                fs::write(dir.join(format!("f{f:03}")), format!("{d} {f}")).expect("write");
            }
        }
        let lookahead = record(&source, PrescanPolicy::Lookahead, None);
        // upstream: rsync.h:152 MIN_FILECNT_LOOKAHEAD - the sender stops once
        // that many entries are listed past the request, so it holds at most
        // that plus the directory that crossed it and the root's own list.
        let cap = protocol::flist::MIN_FILECNT_LOOKAHEAD + per_dir + 41;
        assert!(
            lookahead.peak_listed_entries <= cap,
            "held {} listed entries, cap {cap}",
            lookahead.peak_listed_entries
        );
        let everything = record(&source, PrescanPolicy::Everything, None);
        assert!(everything.peak_listed_entries > cap);
        assert_eq!(lookahead.batch, everything.batch);
    }
}
