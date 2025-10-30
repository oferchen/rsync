use super::prelude::*;


#[test]
fn write_daemon_password_appends_newline_and_zeroizes_buffer() {
    let mut output = Vec::new();
    let mut secret = b"swordfish".to_vec();

    write_daemon_password(&mut output, &mut secret).expect("write succeeds");

    assert_eq!(output, b"swordfish\n");
    assert!(secret.iter().all(|&byte| byte == 0));
}


#[test]
fn write_daemon_password_handles_existing_newline() {
    let mut output = Vec::new();
    let mut secret = b"hunter2\n".to_vec();

    write_daemon_password(&mut output, &mut secret).expect("write succeeds");

    assert_eq!(output, b"hunter2\n");
    assert!(secret.iter().all(|&byte| byte == 0));
}

