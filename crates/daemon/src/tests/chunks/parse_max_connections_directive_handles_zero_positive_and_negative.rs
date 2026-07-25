#[test]
fn parse_max_connections_directive_handles_zero_positive_and_negative() {
    // upstream: `max connections` is a P_INTEGER directive read via atoi()
    // (loadparm.c:431-433). atoi maps an empty, whitespace-only or non-numeric
    // value to 0, and connection.c:claim_connection:27 returns success for 0
    // without taking a lock - that is the unlimited case.
    assert_eq!(
        parse_max_connections_directive(""),
        Some(MaxConnections::Unlimited)
    );
    assert_eq!(
        parse_max_connections_directive("  "),
        Some(MaxConnections::Unlimited)
    );
    assert_eq!(
        parse_max_connections_directive("0"),
        Some(MaxConnections::Unlimited)
    );
    assert_eq!(
        parse_max_connections_directive("invalid"),
        Some(MaxConnections::Unlimited)
    );

    // A positive value bounds the slot scan at connection.c:33.
    let expected = NonZeroU32::new(25).expect("non-zero");
    assert_eq!(
        parse_max_connections_directive("25"),
        Some(MaxConnections::Limited(expected))
    );

    // A negative value can never satisfy `i < max_connections`, so the slot
    // scan is empty and every connection is refused. rsyncd.conf.5: "A negative
    // value disables the module". The sign must survive parsing because
    // clientserver.c:746-757 echoes the configured number verbatim.
    assert_eq!(
        parse_max_connections_directive("-1"),
        Some(MaxConnections::Disabled(-1))
    );

    // atoi leniency applies to the disabling form too.
    assert_eq!(
        parse_max_connections_directive("-7 trailing"),
        Some(MaxConnections::Disabled(-7))
    );
}
