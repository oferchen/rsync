#[test]
fn connection_status_messages_describe_active_sessions() {
    assert_eq!(format_connection_status(0), "Idle; waiting for connections");
    assert_eq!(format_connection_status(1), "Serving 1 connection");
    assert_eq!(format_connection_status(3), "Serving 3 connections");
}

