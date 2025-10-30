use super::prelude::*;


#[test]
fn module_list_request_detects_remote_url() {
    let operands = vec![OsString::from("rsync://example.com:8730/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "example.com");
    assert_eq!(request.address().port(), 8730);
}


#[test]
fn module_list_request_accepts_mixed_case_scheme() {
    let operands = vec![OsString::from("RSyNc://Example.COM/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "Example.COM");
    assert_eq!(request.address().port(), 873);
}


#[test]
fn module_list_request_honours_custom_default_port() {
    let operands = vec![OsString::from("rsync://example.com/")];
    let request = ModuleListRequest::from_operands_with_port(&operands, 10_873)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "example.com");
    assert_eq!(request.address().port(), 10_873);
}


#[test]
fn module_list_request_rejects_remote_transfer() {
    let operands = vec![OsString::from("rsync://example.com/module")];
    let request = ModuleListRequest::from_operands(&operands).expect("parse succeeds");
    assert!(request.is_none());
}


#[test]
fn module_list_request_accepts_username_in_rsync_url() {
    let operands = vec![OsString::from("rsync://user@example.com/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "example.com");
    assert_eq!(request.address().port(), 873);
    assert_eq!(request.username(), Some("user"));
}


#[test]
fn module_list_request_accepts_username_in_legacy_syntax() {
    let operands = vec![OsString::from("user@example.com::")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "example.com");
    assert_eq!(request.address().port(), 873);
    assert_eq!(request.username(), Some("user"));
}


#[test]
fn module_list_request_supports_ipv6_in_rsync_url() {
    let operands = vec![OsString::from("rsync://[2001:db8::1]/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "2001:db8::1");
    assert_eq!(request.address().port(), 873);
}


#[test]
fn module_list_request_supports_ipv6_in_legacy_syntax() {
    let operands = vec![OsString::from("[fe80::1]::")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "fe80::1");
    assert_eq!(request.address().port(), 873);
}


#[test]
fn module_list_request_decodes_percent_encoded_host() {
    let operands = vec![OsString::from("rsync://example%2Ecom/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "example.com");
    assert_eq!(request.address().port(), 873);
}


#[test]
fn module_list_request_supports_ipv6_zone_identifier() {
    let operands = vec![OsString::from("rsync://[fe80::1%25eth0]/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "fe80::1%eth0");
    assert_eq!(request.address().port(), 873);
}


#[test]
fn module_list_request_supports_raw_ipv6_zone_identifier() {
    let operands = vec![OsString::from("[fe80::1%eth0]::")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "fe80::1%eth0");
    assert_eq!(request.address().port(), 873);
}


#[test]
fn module_list_request_decodes_percent_encoded_username() {
    let operands = vec![OsString::from("user%2Bname@localhost::")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.username(), Some("user+name"));
    assert_eq!(request.address().host(), "localhost");
}


#[test]
fn module_list_request_rejects_truncated_percent_encoding_in_username() {
    let operands = vec![OsString::from("user%2@localhost::")];
    let error =
        ModuleListRequest::from_operands(&operands).expect_err("invalid encoding should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("invalid percent-encoding in daemon username")
    );
}


#[test]
fn module_list_request_defaults_to_localhost_for_shorthand() {
    let operands = vec![OsString::from("::")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "localhost");
    assert_eq!(request.address().port(), 873);
    assert!(request.username().is_none());
}


#[test]
fn module_list_request_preserves_username_with_default_host() {
    let operands = vec![OsString::from("user@::")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "localhost");
    assert_eq!(request.address().port(), 873);
    assert_eq!(request.username(), Some("user"));
}


#[test]
fn module_list_options_reports_address_mode() {
    let options = ModuleListOptions::default().with_address_mode(AddressMode::Ipv6);
    assert_eq!(options.address_mode(), AddressMode::Ipv6);

    let default_options = ModuleListOptions::default();
    assert_eq!(default_options.address_mode(), AddressMode::Default);
}


#[test]
fn module_list_options_records_bind_address() {
    let socket = "198.51.100.4:0".parse().expect("socket");
    let options = ModuleListOptions::default().with_bind_address(Some(socket));
    assert_eq!(options.bind_address(), Some(socket));

    let default_options = ModuleListOptions::default();
    assert!(default_options.bind_address().is_none());
}


#[test]
fn module_list_request_parses_ipv6_zone_identifier() {
    let operands = vec![OsString::from("rsync://fe80::1%eth0/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request present");
    assert_eq!(request.address().host(), "fe80::1%eth0");
    assert_eq!(request.address().port(), 873);

    let bracketed = vec![OsString::from("rsync://[fe80::1%25eth0]/")];
    let request = ModuleListRequest::from_operands(&bracketed)
        .expect("parse succeeds")
        .expect("request present");
    assert_eq!(request.address().host(), "fe80::1%eth0");
    assert_eq!(request.address().port(), 873);
}


#[test]
fn module_list_request_rejects_truncated_percent_encoding() {
    let operands = vec![OsString::from("rsync://example%2/")];
    let error = ModuleListRequest::from_operands(&operands)
        .expect_err("truncated percent encoding should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("invalid percent-encoding in daemon host")
    );
}


#[test]
fn module_list_request_rejects_empty_username() {
    let operands = vec![OsString::from("@example.com::")];
    let error =
        ModuleListRequest::from_operands(&operands).expect_err("empty username should be rejected");
    let rendered = error.message().to_string();
    assert!(rendered.contains("daemon username must be non-empty"));
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
}


#[test]
fn module_list_request_rejects_ipv6_module_transfer() {
    let operands = vec![OsString::from("[fe80::1]::module")];
    let request = ModuleListRequest::from_operands(&operands).expect("parse succeeds");
    assert!(request.is_none());
}


#[test]
fn module_list_request_requires_bracketed_ipv6_host() {
    let operands = vec![OsString::from("fe80::1::")];
    let error = ModuleListRequest::from_operands(&operands)
        .expect_err("unbracketed IPv6 host should be rejected");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("IPv6 daemon addresses must be enclosed in brackets")
    );
}

