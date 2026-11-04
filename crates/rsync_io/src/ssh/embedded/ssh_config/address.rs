//! Numeric host addresses, as `ssh` recognises and rewrites them.
//!
//! Upstream asks `getaddrinfo(AI_NUMERICHOST)` whether a host is an address
//! and prints it back through `getnameinfo(NI_NUMERICHOST)`
//! (openssh/ssh.c:298-380). With glibc that accepts the `inet_aton`
//! shorthands, so `127.1` becomes `127.0.0.1`, and prints IPv6 in
//! `inet_ntop` form. Both halves are ported here.

use std::net::{Ipv4Addr, Ipv6Addr};

use crate::ssh::config_options::AddressFamily;

/// `is_addr_fast` (openssh/ssh.c:298-302): a scope or port separator, or
/// nothing but digits and dots.
pub(super) fn is_addr_fast(name: &str) -> bool {
    name.contains(['%', ':']) || name.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

/// `is_addr` (openssh/ssh.c:304-329): the fast test, or a single numeric
/// address for the configured family.
pub(super) fn is_addr(name: &str, family: Option<AddressFamily>) -> bool {
    is_addr_fast(name) || canonical_address(name, family).is_some()
}

/// The form `resolve_addr` gives a numeric host (openssh/ssh.c:337-380), or
/// `None` when `name` is not a numeric address of the configured family.
/// A scoped IPv6 address keeps its scope as written.
pub(super) fn canonical_address(name: &str, family: Option<AddressFamily>) -> Option<String> {
    let v4 = !matches!(family, Some(AddressFamily::Inet6));
    let v6 = !matches!(family, Some(AddressFamily::Inet));
    if v4 && let Some(addr) = inet_aton(name) {
        return Some(addr.to_string());
    }
    if v6 {
        if let Some((addr, scope)) = name.split_once('%') {
            return addr
                .parse::<Ipv6Addr>()
                .ok()
                .filter(|_| !scope.is_empty())
                .map(|addr| format!("{}%{scope}", inet_ntop6(addr)));
        }
        return name.parse::<Ipv6Addr>().ok().map(inet_ntop6);
    }
    None
}

/// glibc's `inet_aton` as `getaddrinfo` applies it, exactly: one to four
/// parts in C notation (decimal, `0` octal, `0x` hex), the last filling the
/// remaining bytes, and nothing after them.
fn inet_aton(s: &str) -> Option<Ipv4Addr> {
    let parts = s.split('.').map(c_number).collect::<Option<Vec<u32>>>()?;
    let (&last, head) = parts.split_last()?;
    if head.len() > 3 || head.iter().any(|&part| part > 0xff) {
        return None;
    }
    if u64::from(last) >> (8 * (4 - head.len())) != 0 {
        return None;
    }
    let addr = head
        .iter()
        .enumerate()
        .fold(last, |acc, (i, &part)| acc | (part << (24 - 8 * i)));
    Some(Ipv4Addr::from(addr))
}

/// One `inet_aton` part: `strtoul(part, &end, 0)` that must consume it all.
fn c_number(part: &str) -> Option<u32> {
    let (digits, radix) =
        if let Some(hex) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
            (hex, 16)
        } else if part.len() > 1 && part.starts_with('0') {
            (&part[1..], 8)
        } else {
            (part, 10)
        };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    u32::from_str_radix(digits, radix).ok()
}

/// glibc's `inet_ntop6`: the longest run of two or more zero words (the
/// first on a tie) becomes `::`, words print as lower-case hex, and an
/// IPv4-compatible or -mapped address ends in dotted quad.
fn inet_ntop6(addr: Ipv6Addr) -> String {
    let words = addr.segments();
    let (mut best_base, mut best_len) = (None, 0);
    let mut i = 0;
    while i < words.len() {
        if words[i] == 0 {
            let start = i;
            while i < words.len() && words[i] == 0 {
                i += 1;
            }
            if i - start > best_len {
                (best_base, best_len) = (Some(start), i - start);
            }
        } else {
            i += 1;
        }
    }
    if best_len < 2 {
        best_base = None;
    }
    let mut out = String::new();
    for (i, word) in words.iter().enumerate() {
        if let Some(base) = best_base
            && (base..base + best_len).contains(&i)
        {
            if i == base {
                out.push(':');
            }
            continue;
        }
        if i != 0 {
            out.push(':');
        }
        if i == 6
            && best_base == Some(0)
            && (best_len == 6 || (best_len == 5 && words[5] == 0xffff))
        {
            let [.., a, b, c, d] = addr.octets();
            out.push_str(&Ipv4Addr::new(a, b, c, d).to_string());
            break;
        }
        out.push_str(&format!("{word:x}"));
    }
    if best_base.is_some_and(|base| base + best_len == words.len()) {
        out.push(':');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each row measured on the 10.0p1 `ssh -G <host>` `hostname` line.
    #[test]
    fn numeric_hosts_take_resolve_addrs_form() {
        for (host, canonical) in [
            ("127.1", Some("127.0.0.1")),
            ("0x7f.1", Some("127.0.0.1")),
            ("017.0.0.1", Some("15.0.0.1")),
            ("10.300", Some("10.0.1.44")),
            ("1", Some("0.0.0.1")),
            ("1.2.3", Some("1.2.0.3")),
            ("00", Some("0.0.0.0")),
            ("4294967295", Some("255.255.255.255")),
            ("0x1.0x2.0x3.0x4", Some("1.2.3.4")),
            ("0:0::1", Some("::1")),
            ("::1.2.3.4", Some("::1.2.3.4")),
            ("0:0:0:0:0:ffff:102:304", Some("::ffff:1.2.3.4")),
            ("1:0:0:1:0:0:0:1", Some("1:0:0:1::1")),
            ("2001:db8:0:0:1:0:0:1", Some("2001:db8::1:0:0:1")),
            ("FE80::A", Some("fe80::a")),
            ("fe80::1%lo", Some("fe80::1%lo")),
            ("0x", None),
            ("08", None),
            ("1.2.3.256", None),
            ("4294967296", None),
            ("1..2", None),
            ("1.2.3.", None),
            ("1.2.3.4.5", None),
            ("abc", None),
        ] {
            assert_eq!(
                canonical_address(host, None).as_deref(),
                canonical,
                "{host}"
            );
        }
    }

    /// `AddressFamily` narrows the `getaddrinfo` hints, so an address of the
    /// other family is not rewritten (it still counts as one, by its shape).
    #[test]
    fn address_family_narrows_the_numeric_parse() {
        assert_eq!(canonical_address("0:0::1", Some(AddressFamily::Inet)), None);
        assert_eq!(canonical_address("127.1", Some(AddressFamily::Inet6)), None);
        assert!(is_addr("127.1", Some(AddressFamily::Inet6)));
        assert!(!is_addr("abc", None));
    }
}
