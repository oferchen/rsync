// Tests for skip-compress: files matching the skip-compress list bypass
// compression even when --compress is enabled.

/// When compression is enabled and the file has a suffix in the default
/// skip-compress list (e.g. `.gz`), the compressor must not be engaged.
/// The summary should reflect that no compressed bytes were produced for
/// that file.
#[test]
fn skip_compress_default_list_skips_gz_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("archive.tar.gz");
    let destination = temp.path().join("dest.tar.gz");
    let content = vec![0xAB; 8 * 1024];
    fs::write(&source, &content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().compress(true),
        )
        .expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), content);
    assert_eq!(summary.files_copied(), 1);
    // The .gz suffix is in the default skip list, so compression should
    // not have been applied.  compressed_bytes stays 0 and
    // compression_used remains false.
    assert!(!summary.compression_used());
    assert_eq!(summary.compressed_bytes(), 0);
}

/// When compression is enabled and the file has a suffix in the default
/// skip-compress list (e.g. `.jpg`), the compressor is bypassed.
#[test]
fn skip_compress_default_list_skips_jpg_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("photo.jpg");
    let destination = temp.path().join("dest.jpg");
    let content = vec![0xFF; 4 * 1024];
    fs::write(&source, &content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().compress(true),
        )
        .expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), content);
    assert!(!summary.compression_used());
    assert_eq!(summary.compressed_bytes(), 0);
}

/// When compression is enabled and the file does NOT have a suffix in the
/// skip-compress list, compression should be applied normally.
#[test]
fn skip_compress_allows_compression_for_txt_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("notes.txt");
    let destination = temp.path().join("dest.txt");
    // Use repetitive content so zlib actually compresses it.
    let content = vec![b'A'; 16 * 1024];
    fs::write(&source, &content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().compress(true),
        )
        .expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), content);
    assert!(summary.compression_used());
    assert!(summary.compressed_bytes() > 0);
    // Repetitive content compresses well.
    assert!(summary.compressed_bytes() < summary.bytes_copied());
}

/// When compression is disabled, even files that are not in the skip list
/// should not produce compressed bytes.
#[test]
fn skip_compress_no_compression_when_disabled() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("notes.txt");
    let destination = temp.path().join("dest.txt");
    let content = b"some text";
    fs::write(&source, content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().compress(false),
        )
        .expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), content.as_slice());
    assert!(!summary.compression_used());
    assert_eq!(summary.compressed_bytes(), 0);
}

/// A custom skip-compress list via the builder should override the default
/// list. Only the custom suffixes should skip compression.
#[test]
fn skip_compress_custom_list_via_builder() {
    use crate::local_copy::SkipCompressList;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source dir");
    let dest_dir = temp.path().join("dest");

    // Create a .xyz file (custom suffix) and a .txt file.
    let custom_content = vec![0xCD; 8 * 1024];
    let text_content = vec![b'B'; 16 * 1024];
    fs::write(source_dir.join("data.xyz"), &custom_content).expect("write .xyz");
    fs::write(source_dir.join("data.txt"), &text_content).expect("write .txt");

    let mut source_operand = source_dir.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_dir.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Custom list: only skip .xyz
    let custom_list = SkipCompressList::parse("xyz").expect("parse custom list");
    let options = LocalCopyOptions::builder()
        .compress(true)
        .skip_compress(custom_list)
        .build()
        .expect("valid options");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(
        fs::read(dest_dir.join("data.xyz")).expect("read .xyz"),
        custom_content
    );
    assert_eq!(
        fs::read(dest_dir.join("data.txt")).expect("read .txt"),
        text_content
    );

    // Compression was used (for the .txt file at least).
    assert!(summary.compression_used());
    assert!(summary.compressed_bytes() > 0);
}

/// An empty skip-compress list allows compression for all files, even those
/// that the default list would skip (like .gz).
#[test]
fn skip_compress_empty_list_compresses_everything() {
    use crate::local_copy::SkipCompressList;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("archive.gz");
    let destination = temp.path().join("dest.gz");
    // Repetitive content so compression still has an effect even if the
    // content is named .gz.
    let content = vec![b'Z'; 16 * 1024];
    fs::write(&source, &content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let empty_list = SkipCompressList::parse("").expect("parse empty list");
    let options = LocalCopyOptions::default()
        .compress(true)
        .with_skip_compress(empty_list);

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), content);
    // With an empty skip list, even .gz files get compressed.
    assert!(summary.compression_used());
    assert!(summary.compressed_bytes() > 0);
}

/// Case-insensitive matching: .MP4 (uppercase) should still be skipped by
/// the default list which contains "mp4".
#[test]
fn skip_compress_case_insensitive_matching() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("video.MP4");
    let destination = temp.path().join("dest.MP4");
    let content = vec![0xBE; 8 * 1024];
    fs::write(&source, &content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().compress(true),
        )
        .expect("copy succeeds");

    assert_eq!(fs::read(&destination).expect("read dest"), content);
    assert!(!summary.compression_used());
    assert_eq!(summary.compressed_bytes(), 0);
}

/// A directory copy with mixed file types: some skipped, some compressed.
/// Verifies the pipeline handles per-file decisions correctly across a
/// multi-file transfer.
#[test]
fn skip_compress_mixed_directory_transfer() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source dir");
    let dest_dir = temp.path().join("dest");

    // Create files of different types.
    let gz_content = vec![0x1F; 4 * 1024]; // .gz should be skipped
    let png_content = vec![0x89; 4 * 1024]; // .png should be skipped
    let txt_content = vec![b'X'; 16 * 1024]; // .txt should be compressed
    let rs_content = vec![b'Y'; 16 * 1024]; // .rs should be compressed

    fs::write(source_dir.join("archive.gz"), &gz_content).expect("write .gz");
    fs::write(source_dir.join("image.png"), &png_content).expect("write .png");
    fs::write(source_dir.join("readme.txt"), &txt_content).expect("write .txt");
    fs::write(source_dir.join("main.rs"), &rs_content).expect("write .rs");

    let mut source_operand = source_dir.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_dir.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().compress(true),
        )
        .expect("copy succeeds");

    // All four files should be present in the destination.
    assert_eq!(fs::read(dest_dir.join("archive.gz")).expect("read .gz"), gz_content);
    assert_eq!(fs::read(dest_dir.join("image.png")).expect("read .png"), png_content);
    assert_eq!(fs::read(dest_dir.join("readme.txt")).expect("read .txt"), txt_content);
    assert_eq!(fs::read(dest_dir.join("main.rs")).expect("read .rs"), rs_content);

    // At least some compression was used (for .txt and .rs files).
    assert!(summary.compression_used());
    assert!(summary.compressed_bytes() > 0);

    // The total literal bytes should cover all four files.
    let total_literal = (gz_content.len() + png_content.len() + txt_content.len() + rs_content.len()) as u64;
    assert_eq!(summary.bytes_copied(), total_literal);
}

/// Verifies that the default SkipCompressList matches all the commonly
/// compressed formats that upstream rsync ships.
#[test]
fn skip_compress_default_list_covers_common_formats() {
    use crate::local_copy::SkipCompressList;

    let list = SkipCompressList::default();

    // Archive formats
    assert!(list.matches_path(Path::new("file.gz")));
    assert!(list.matches_path(Path::new("file.bz2")));
    assert!(list.matches_path(Path::new("file.xz")));
    assert!(list.matches_path(Path::new("file.zip")));
    assert!(list.matches_path(Path::new("file.7z")));
    assert!(list.matches_path(Path::new("file.zst")));
    assert!(list.matches_path(Path::new("file.lz4")));
    assert!(list.matches_path(Path::new("file.rar")));
    assert!(list.matches_path(Path::new("file.lzma")));

    // Image formats
    assert!(list.matches_path(Path::new("file.jpg")));
    assert!(list.matches_path(Path::new("file.jpeg")));
    assert!(list.matches_path(Path::new("file.png")));
    assert!(list.matches_path(Path::new("file.webp")));

    // Audio formats
    assert!(list.matches_path(Path::new("file.mp3")));
    assert!(list.matches_path(Path::new("file.aac")));
    assert!(list.matches_path(Path::new("file.flac")));
    assert!(list.matches_path(Path::new("file.ogg")));
    assert!(list.matches_path(Path::new("file.opus")));

    // Video formats
    assert!(list.matches_path(Path::new("file.mp4")));
    assert!(list.matches_path(Path::new("file.mkv")));
    assert!(list.matches_path(Path::new("file.avi")));
    assert!(list.matches_path(Path::new("file.webm")));
    assert!(list.matches_path(Path::new("file.mov")));

    // Package formats
    assert!(list.matches_path(Path::new("file.deb")));
    assert!(list.matches_path(Path::new("file.rpm")));
    assert!(list.matches_path(Path::new("file.jar")));
    assert!(list.matches_path(Path::new("file.apk")));

    // Formats that should NOT be in the skip list
    assert!(!list.matches_path(Path::new("file.txt")));
    assert!(!list.matches_path(Path::new("file.rs")));
    assert!(!list.matches_path(Path::new("file.c")));
    assert!(!list.matches_path(Path::new("file.html")));
    assert!(!list.matches_path(Path::new("file.xml")));
    assert!(!list.matches_path(Path::new("file.json")));
    assert!(!list.matches_path(Path::new("file.csv")));
    assert!(!list.matches_path(Path::new("file.log")));
}

/// Verifies that the `should_skip_compress` method on `LocalCopyOptions`
/// correctly delegates to the configured skip-compress list.
#[test]
fn skip_compress_options_should_skip_compress_method() {
    use crate::local_copy::SkipCompressList;

    // Default options use the default skip list.
    let opts = LocalCopyOptions::default();
    assert!(opts.should_skip_compress(Path::new("archive.gz")));
    assert!(opts.should_skip_compress(Path::new("photo.PNG")));
    assert!(!opts.should_skip_compress(Path::new("notes.txt")));

    // Custom list.
    let custom = SkipCompressList::parse("dat/bin").expect("parse");
    let opts = LocalCopyOptions::default().with_skip_compress(custom);
    assert!(opts.should_skip_compress(Path::new("data.dat")));
    assert!(opts.should_skip_compress(Path::new("program.bin")));
    assert!(!opts.should_skip_compress(Path::new("archive.gz")));
    assert!(!opts.should_skip_compress(Path::new("notes.txt")));

    // Empty list skips nothing.
    let empty = SkipCompressList::parse("").expect("parse empty");
    let opts = LocalCopyOptions::default().with_skip_compress(empty);
    assert!(!opts.should_skip_compress(Path::new("archive.gz")));
    assert!(!opts.should_skip_compress(Path::new("photo.png")));
}

/// Files without an extension should never match the skip-compress list.
#[test]
fn skip_compress_no_extension_never_matches() {
    use crate::local_copy::SkipCompressList;

    let list = SkipCompressList::default();
    assert!(!list.matches_path(Path::new("Makefile")));
    assert!(!list.matches_path(Path::new("LICENSE")));
    assert!(!list.matches_path(Path::new("README")));
    assert!(!list.matches_path(Path::new(".gitignore")));
}

/// The builder's `skip_compress` method should correctly pass the list
/// through to the built options.
#[test]
fn skip_compress_builder_passes_list_through() {
    use crate::local_copy::SkipCompressList;

    let list = SkipCompressList::parse("custom/ext").expect("parse");
    let options = LocalCopyOptions::builder()
        .compress(true)
        .skip_compress(list)
        .build()
        .expect("valid options");

    assert!(options.should_skip_compress(Path::new("file.custom")));
    assert!(options.should_skip_compress(Path::new("file.ext")));
    assert!(!options.should_skip_compress(Path::new("file.gz")));
}
