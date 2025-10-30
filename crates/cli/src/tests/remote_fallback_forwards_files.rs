use super::common::*;
use super::*;

#[cfg(unix)]
#[test]
fn remote_fallback_forwards_files_from_entries() {
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _rsh_guard = clear_rsync_rsh();
    let temp = tempdir().expect("tempdir");
    let list_path = temp.path().join("file-list.txt");
    std::fs::write(&list_path, b"alpha\nbeta\n").expect("write list");

    let script_path = temp.path().join("fallback.sh");
    let args_path = temp.path().join("args.txt");
    let files_copy_path = temp.path().join("files.bin");
    let dest_path = temp.path().join("dest");

    let script = r#"#!/bin/sh
printf "%s\n" "$@" > "$ARGS_FILE"
files_from=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--files-from" ]; then
files_from="$2"
break
  fi
  shift
done
if [ -n "$files_from" ]; then
  cat "$files_from" > "$FILES_COPY"
fi
exit 0
"#;
    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _args_guard = EnvGuard::set("ARGS_FILE", args_path.as_os_str());
    let _files_guard = EnvGuard::set("FILES_COPY", files_copy_path.as_os_str());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from(format!("--files-from={}", list_path.display())),
        OsString::from("remote::module"),
        dest_path.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let dest_display = dest_path.display().to_string();
    let recorded = std::fs::read_to_string(&args_path).expect("read args file");
    assert!(recorded.contains("--files-from"));
    assert!(recorded.contains("remote::module"));
    assert!(recorded.contains(&dest_display));

    let copied = std::fs::read(&files_copy_path).expect("read copied file list");
    assert_eq!(copied, b"alpha\nbeta\n");
}
