use super::prelude::*;


#[test]
fn builder_forces_event_collection() {
    let config = ClientConfig::builder()
        .transfer_args([OsString::from("src"), OsString::from("dst")])
        .force_event_collection(true)
        .build();

    assert!(config.force_event_collection());
    assert!(config.collect_events());
}

