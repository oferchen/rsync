use super::*;

#[test]
fn extract_matrix_entry_parses_enabled_flag() {
    let contents = r#"
      - name: linux-x86_64
        enabled: true
        target: x86_64-unknown-linux-gnu
        build_command: build
        build_daemon: true
        uses_zig: false
        needs_cross_gcc: false
        generate_sbom: true
      - name: windows-x86
        enabled: false
        target: i686-pc-windows-gnu
        build_command: zigbuild
        build_daemon: false
        uses_zig: true
        needs_cross_gcc: false
        generate_sbom: false
    "#;

    let linux = extract_matrix_entry(contents, "linux-x86_64").expect("linux entry");
    assert_eq!(linux.enabled, Some(true));
    assert_eq!(linux.target.as_deref(), Some("x86_64-unknown-linux-gnu"));
    assert_eq!(linux.build_command.as_deref(), Some("build"));
    assert_eq!(linux.build_daemon, Some(true));
    assert_eq!(linux.uses_zig, Some(false));
    assert_eq!(linux.needs_cross_gcc, Some(false));
    assert_eq!(linux.generate_sbom, Some(true));

    let windows = extract_matrix_entry(contents, "windows-x86").expect("windows entry");
    assert_eq!(windows.enabled, Some(false));
    assert_eq!(windows.target.as_deref(), Some("i686-pc-windows-gnu"));
    assert_eq!(windows.build_command.as_deref(), Some("zigbuild"));
    assert_eq!(windows.build_daemon, Some(false));
    assert_eq!(windows.uses_zig, Some(true));
    assert_eq!(windows.needs_cross_gcc, Some(false));
    assert_eq!(windows.generate_sbom, Some(false));
}
