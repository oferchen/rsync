#[test]
fn parse_auth_user_list_trims_and_deduplicates_case_insensitively() {
    let users =
        parse_auth_user_list(" alice,BOB, alice ,  Carol ").expect("parse non-empty user list");
    let usernames: Vec<&str> = users.iter().map(|u| u.username.as_str()).collect();
    assert_eq!(usernames, ["alice", "BOB", "Carol"]);

    let err = parse_auth_user_list(" , ,  ").expect_err("blank list rejected");
    assert_eq!(err, "must specify at least one username");
}

