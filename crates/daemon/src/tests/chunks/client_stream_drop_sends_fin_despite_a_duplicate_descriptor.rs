/// Dropping a test client must end its session even while another descriptor
/// for the same socket stays open.
///
/// On Unix the in-process daemon forks each session child from the test
/// process, so the child holds a duplicate of the client socket - modelled
/// here by `try_clone`. If dropping the client only closed its own descriptor,
/// no FIN would reach the session, and a session waiting for client input
/// would outlive the test until its I/O timeout.
#[test]
fn client_stream_drop_sends_fin_despite_a_duplicate_descriptor() {
    let (port, listener) = allocate_test_port();
    let client = connect_with_retries(port);
    let (mut session, _) = listener.accept().expect("accept client");
    session
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("bound the session read");
    let inherited = client.try_clone().expect("duplicate client socket");

    drop(client);

    let mut byte = [0u8; 1];
    let read = session
        .read(&mut byte)
        .expect("the session must see the client's FIN, not wait for more input");
    assert_eq!(read, 0, "a dropped client must leave the session at EOF");
    drop(inherited);
}
