
#[test]
fn plan_from_operands_requires_destination() {
    let operands = vec![OsString::from("only-source")];
    let error = LocalCopyPlan::from_operands(&operands).expect_err("missing destination");
    assert!(matches!(
        error.kind(),
        LocalCopyErrorKind::MissingSourceOperands
    ));
}

#[test]
fn plan_rejects_empty_operands() {
    let operands = vec![OsString::new(), OsString::from("dest")];
    let error = LocalCopyPlan::from_operands(&operands).expect_err("empty source");
    assert!(matches!(
        error.kind(),
        LocalCopyErrorKind::InvalidArgument(LocalCopyArgumentError::EmptySourceOperand)
    ));
}

#[test]
fn plan_rejects_empty_destination() {
    let operands = vec![OsString::from("src"), OsString::new()];
    let error = LocalCopyPlan::from_operands(&operands).expect_err("empty destination");
    assert!(matches!(
        error.kind(),
        LocalCopyErrorKind::InvalidArgument(LocalCopyArgumentError::EmptyDestinationOperand)
    ));
}

#[test]
fn plan_rejects_remote_module_source() {
    let operands = vec![OsString::from("host::module"), OsString::from("dest")];
    let error = LocalCopyPlan::from_operands(&operands).expect_err("remote module");
    assert!(matches!(
        error.kind(),
        LocalCopyErrorKind::InvalidArgument(LocalCopyArgumentError::RemoteOperandUnsupported)
    ));
}

#[test]
fn plan_rejects_remote_shell_source() {
    let operands = vec![OsString::from("host:/path"), OsString::from("dest")];
    let error = LocalCopyPlan::from_operands(&operands).expect_err("remote shell source");
    assert!(matches!(
        error.kind(),
        LocalCopyErrorKind::InvalidArgument(LocalCopyArgumentError::RemoteOperandUnsupported)
    ));
}

#[test]
fn plan_rejects_remote_destination() {
    let operands = vec![OsString::from("src"), OsString::from("rsync://host/module")];
    let error = LocalCopyPlan::from_operands(&operands).expect_err("remote destination");
    assert!(matches!(
        error.kind(),
        LocalCopyErrorKind::InvalidArgument(LocalCopyArgumentError::RemoteOperandUnsupported)
    ));
}

#[test]
fn plan_accepts_windows_drive_style_paths() {
    let operands = vec![OsString::from("C:\\source"), OsString::from("C:\\dest")];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan accepts drive paths");
    assert_eq!(plan.sources().len(), 1);
}

#[test]
fn plan_detects_trailing_separator() {
    let operands = vec![OsString::from("dir/"), OsString::from("dest")];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    assert!(plan.sources()[0].copy_contents());
}

#[test]
fn execute_creates_directory_for_trailing_destination_separator() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    fs::write(&source, b"payload").expect("write source");

    let dest_dir = temp.path().join("dest");
    let mut destination_operand = dest_dir.clone().into_os_string();
    destination_operand.push(std::path::MAIN_SEPARATOR_STR);

    let operands = vec![source.clone().into_os_string(), destination_operand];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().hard_links(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let copied = dest_dir.join(source.file_name().expect("source name"));
    assert_eq!(fs::read(copied).expect("read copied"), b"payload");
    assert_eq!(summary.files_copied(), 1);
}

#[test]
fn execute_copies_single_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"example").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().hard_links(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    assert_eq!(fs::read(destination).expect("read dest"), b"example");
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_matched(), 0);
}

struct SleepyHandler {
    slept: bool,
    delay: Duration,
}

impl LocalCopyRecordHandler for SleepyHandler {
    fn handle(&mut self, _record: LocalCopyRecord) {
        if !self.slept {
            self.slept = true;
            thread::sleep(self.delay);
        }
    }
}

#[test]
fn local_copy_timeout_enforced() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");
    fs::write(&source, vec![0u8; 1024]).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let mut handler = SleepyHandler {
        slept: false,
        delay: Duration::from_millis(50),
    };

    let options = LocalCopyOptions::default().with_timeout(Some(Duration::from_millis(5)));
    let error = plan
        .execute_with_options_and_handler(LocalCopyExecution::Apply, options, Some(&mut handler))
        .expect_err("timeout should fail copy");

    assert!(matches!(error.kind(), LocalCopyErrorKind::Timeout { .. }));
    assert!(
        !destination.exists(),
        "destination should not be created on timeout"
    );
}

#[test]
fn local_copy_stop_at_enforced() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");
    fs::write(&source, vec![0u8; 1024]).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let stop_at = std::time::SystemTime::now()
        .checked_sub(Duration::from_secs(1))
        .expect("past deadline");
    let options = LocalCopyOptions::default().with_stop_at(Some(stop_at));
    let error = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect_err("stop-at should fail copy");

    assert!(matches!(error.kind(), LocalCopyErrorKind::StopAtReached { .. }));
    assert!(!destination.exists(), "destination should not exist on stop-at");
}
