use super::prelude::*;


#[test]
fn parse_proxy_spec_accepts_http_scheme() {
    let proxy =
        parse_proxy_spec("http://user:secret@proxy.example:8080").expect("http proxy parses");
    assert_eq!(proxy.host, "proxy.example");
    assert_eq!(proxy.port, 8080);
    assert_eq!(
        proxy.authorization_header(),
        Some(String::from("dXNlcjpzZWNyZXQ="))
    );
}


#[test]
fn parse_proxy_spec_decodes_percent_encoded_credentials() {
    let proxy = parse_proxy_spec("http://user%3Aname:p%40ss%25word@proxy.example:1080")
        .expect("percent-encoded proxy parses");
    assert_eq!(proxy.host, "proxy.example");
    assert_eq!(proxy.port, 1080);
    assert_eq!(
        proxy.authorization_header(),
        Some(String::from("dXNlcjpuYW1lOnBAc3Mld29yZA=="))
    );
}


#[test]
fn parse_proxy_spec_accepts_https_scheme() {
    let proxy = parse_proxy_spec("https://proxy.example:3128").expect("https proxy parses");
    assert_eq!(proxy.host, "proxy.example");
    assert_eq!(proxy.port, 3128);
    assert!(proxy.authorization_header().is_none());
}


#[test]
fn parse_proxy_spec_rejects_unknown_scheme() {
    let error = match parse_proxy_spec("socks5://proxy:1080") {
        Ok(_) => panic!("invalid proxy scheme should be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("RSYNC_PROXY scheme must be http:// or https://")
    );
}


#[test]
fn parse_proxy_spec_rejects_path_component() {
    let error = match parse_proxy_spec("http://proxy.example:3128/path") {
        Ok(_) => panic!("proxy specification with path should be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("RSYNC_PROXY must not include a path component")
    );
}


#[test]
fn parse_proxy_spec_rejects_invalid_percent_encoding_in_credentials() {
    let error = match parse_proxy_spec("user%zz:secret@proxy.example:8080") {
        Ok(_) => panic!("invalid percent-encoding should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("RSYNC_PROXY username contains invalid percent-encoding")
    );

    let error = match parse_proxy_spec("user:secret%@proxy.example:8080") {
        Ok(_) => panic!("truncated percent-encoding should be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("RSYNC_PROXY password contains truncated percent-encoding")
    );
}

