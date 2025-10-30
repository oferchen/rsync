use super::common::*;
use super::*;

#[test]
fn stats_human_readable_formats_totals() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("file.bin");
    std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

    let dest_default = tmp.path().join("default");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        source.clone().into_os_string(),
        dest_default.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stats output utf8");
    assert!(rendered.contains("Total file size: 1,536 bytes"));
    assert!(rendered.contains("Total bytes sent: 1,536"));

    let dest_human = tmp.path().join("human");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("--human-readable"),
        source.into_os_string(),
        dest_human.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stats output utf8");
    assert!(rendered.contains("Total file size: 1.54K bytes"));
    assert!(rendered.contains("Total bytes sent: 1.54K"));
}

#[test]
fn stats_human_readable_combined_formats_totals() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("file.bin");
    std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

    let dest_combined = tmp.path().join("combined");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("--human-readable=2"),
        source.into_os_string(),
        dest_combined.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stats output utf8");
    assert!(rendered.contains("Total file size: 1.54K (1,536) bytes"));
    assert!(rendered.contains("Total bytes sent: 1.54K (1,536)"));
}

#[test]
fn stats_transfer_renders_summary_block() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("stats.txt");
    let destination = tmp.path().join("stats.out");
    let payload = b"statistics";
    std::fs::write(&source, payload).expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("stats output is UTF-8");
    let expected_size = payload.len();
    assert!(rendered.contains("Number of files: 1 (reg: 1)"));
    assert!(rendered.contains("Number of created files: 1 (reg: 1)"));
    assert!(rendered.contains("Number of regular files transferred: 1"));
    assert!(!rendered.contains("Number of regular files matched"));
    assert!(!rendered.contains("Number of hard links"));
    assert!(rendered.contains(&format!("Total file size: {expected_size} bytes")));
    assert!(rendered.contains(&format!("Literal data: {expected_size} bytes")));
    assert!(rendered.contains("Matched data: 0 bytes"));
    assert!(rendered.contains("File list size: 0"));
    assert!(rendered.contains("File list generation time:"));
    assert!(rendered.contains("File list transfer time:"));
    assert!(rendered.contains(&format!("Total bytes sent: {expected_size}")));
    assert!(rendered.contains(&format!("Total bytes received: {expected_size}")));
    assert!(rendered.contains("\n\nsent"));
    assert!(rendered.contains("total size is"));
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        payload
    );
}
