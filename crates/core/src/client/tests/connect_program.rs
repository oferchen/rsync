use super::prelude::*;


#[test]
fn connect_program_token_expansion_matches_upstream_rules() {
    let template = OsString::from("netcat %H %P %%");
    let config = ConnectProgramConfig::new(template, None).expect("config");
    let rendered = config
        .format_command("daemon.example", 10873)
        .expect("rendered command");

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(rendered.as_bytes(), b"netcat daemon.example 10873 %");
    }

    #[cfg(not(unix))]
    {
        assert_eq!(rendered, OsString::from("netcat daemon.example 10873 %"));
    }
}

