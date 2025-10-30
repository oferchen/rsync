use super::prelude::*;


#[test]
fn builder_records_bind_address() {
    let bind = BindAddress::new(
        OsString::from("127.0.0.1"),
        "127.0.0.1:0".parse().expect("socket"),
    );
    let config = ClientConfig::builder()
        .bind_address(Some(bind.clone()))
        .build();

    let recorded = config.bind_address().expect("bind address present");
    assert_eq!(recorded.raw(), bind.raw());
    assert_eq!(recorded.socket(), bind.socket());
}


#[test]
fn builder_records_modify_window() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .modify_window(Some(5))
        .build();

    assert_eq!(config.modify_window(), Some(5));
    assert_eq!(config.modify_window_duration(), Duration::from_secs(5));
    assert_eq!(
        ClientConfig::default().modify_window_duration(),
        Duration::ZERO
    );
}

