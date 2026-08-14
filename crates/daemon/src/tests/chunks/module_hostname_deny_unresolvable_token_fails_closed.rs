/// An unresolvable `hosts deny` hostname token must fail CLOSED, and an
/// unresolvable `hosts allow` token must NOT.
///
/// upstream: access.c:57-63 - the forward-DNS branch of `match_hostname`
/// returns the caller's `deny` flag when `gethostbyname` returns NULL:
///
/// ```c
/// if (!(hp = gethostbyname(tok))) {
///     /* A deny-list hostname token we cannot resolve must fail CLOSED:
///      * we can't prove the peer isn't the denied host ... */
///     return deny;
/// }
/// ```
///
/// The flag is threaded per call - 0 for the allow list (access.c:284), 1 for
/// the deny list (access.c:293) - so ONE branch produces opposite answers for
/// the two lists. Sibling of CVE-2026-70452, which fixed the reverse-lookup
/// path only. oc previously returned "no match" for both, so a `hosts deny`
/// naming a host whose DNS was unavailable admitted every peer.
#[test]
fn module_hostname_deny_unresolvable_token_fails_closed() {
    clear_test_hostname_overrides();
    let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 14));
    // A resolved reverse name matching no token, so the sentinel fail-closed
    // guard stays inactive and only the forward-resolution path is under test.
    let reverse = Some("workstation.example.net");

    // THE FIX. No override for the token means the lookup FAILS, and a deny
    // token we cannot resolve must block.
    let deny_only = module_with_host_patterns(&[], &["unresolvable.example.com"]);
    assert!(
        !deny_only.permits(peer, PeerHost::new(reverse, true)),
        "an unresolvable `hosts deny` token must refuse the peer (access.c:57-63)"
    );

    // THE ASYMMETRY, and the reason `deny` is a parameter rather than a global
    // rule: the identical unresolvable token on the ALLOW side must still not
    // match. A fix that returned `true` from the failure branch unconditionally
    // would admit every peer here and pass the assertion above.
    let allow_only = module_with_host_patterns(&["unresolvable.example.com"], &[]);
    assert!(
        !allow_only.permits(peer, PeerHost::new(reverse, true)),
        "an unresolvable `hosts allow` token must still not admit the peer"
    );

    // CONTROL: a deny token that DOES resolve, to some other address, must not
    // block. Without this the whole table is satisfied by a build that refuses
    // everything, which is the failure mode a fail-closed change invites.
    clear_test_hostname_overrides();
    set_test_forward_override(
        "unresolvable.example.com",
        &[IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9))],
    );
    assert!(
        deny_only.permits(peer, PeerHost::new(reverse, true)),
        "a deny token resolving elsewhere must leave the peer permitted"
    );

    // CONTROL: a deny token resolving TO the peer still blocks, so the
    // successful-lookup arm is not collateral damage from the fix.
    set_test_forward_override("unresolvable.example.com", &[peer]);
    assert!(
        !deny_only.permits(peer, PeerHost::new(reverse, true)),
        "a deny token resolving to the peer must still block it"
    );

    clear_test_hostname_overrides();
}
