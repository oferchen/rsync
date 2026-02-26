#[test]
fn help_text_documents_detach_flags() {
    let help = render_help(ProgramName::OcRsyncd);
    assert!(
        help.contains("--detach"),
        "help text should document the --detach flag"
    );
    assert!(
        help.contains("--no-detach"),
        "help text should document the --no-detach flag"
    );
}
