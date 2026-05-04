//! Socket option parsing and application for rsync daemon connections.
//!
//! Translates user-facing option strings (e.g. `SO_KEEPALIVE,TCP_NODELAY`)
//! into platform-specific `setsockopt` calls on a `TcpStream`.
// upstream: socket.c:set_socket_options()

mod apply;
mod consts;
mod errors;
mod lookup;
mod types;

pub(crate) use apply::apply_socket_options;

#[cfg(test)]
mod tests {
    use super::consts;
    #[cfg(not(target_family = "windows"))]
    use super::consts::{IPTOS_LOWDELAY, IPTOS_THROUGHPUT};
    use super::lookup::{intern_name, lookup_socket_option, parse_socket_option_value};
    use super::types::SocketOptionKind;

    #[test]
    fn parse_socket_option_value_parses_positive_integers() {
        assert_eq!(parse_socket_option_value("123"), 123);
        assert_eq!(parse_socket_option_value("65536"), 65536);
        assert_eq!(parse_socket_option_value("1"), 1);
        assert_eq!(parse_socket_option_value("0"), 0);
    }

    #[test]
    fn parse_socket_option_value_parses_negative_integers() {
        assert_eq!(parse_socket_option_value("-1"), -1);
        assert_eq!(parse_socket_option_value("-100"), -100);
        assert_eq!(parse_socket_option_value("-65536"), -65536);
    }

    #[test]
    fn parse_socket_option_value_handles_plus_sign() {
        assert_eq!(parse_socket_option_value("+123"), 123);
        assert_eq!(parse_socket_option_value("+0"), 0);
    }

    #[test]
    fn parse_socket_option_value_strips_leading_whitespace() {
        assert_eq!(parse_socket_option_value("  123"), 123);
        assert_eq!(parse_socket_option_value("\t456"), 456);
    }

    #[test]
    fn parse_socket_option_value_handles_trailing_garbage() {
        assert_eq!(parse_socket_option_value("123abc"), 123);
        assert_eq!(parse_socket_option_value("456 extra"), 456);
    }

    #[test]
    fn parse_socket_option_value_returns_zero_for_empty() {
        assert_eq!(parse_socket_option_value(""), 0);
        assert_eq!(parse_socket_option_value("   "), 0);
    }

    #[test]
    fn parse_socket_option_value_returns_zero_for_invalid() {
        assert_eq!(parse_socket_option_value("abc"), 0);
        assert_eq!(parse_socket_option_value("-"), 0);
        assert_eq!(parse_socket_option_value("+"), 0);
        assert_eq!(parse_socket_option_value("-abc"), 0);
    }

    #[test]
    fn parse_socket_option_value_clamps_overflow() {
        assert_eq!(parse_socket_option_value("9999999999999"), i32::MAX);
        assert_eq!(parse_socket_option_value("-9999999999999"), i32::MIN);
    }

    #[test]
    fn lookup_socket_option_finds_so_keepalive() {
        let result = lookup_socket_option("SO_KEEPALIVE");
        assert!(result.is_some());
        match result.unwrap() {
            SocketOptionKind::Bool { level, option } => {
                assert_eq!(level, consts::SOL_SOCKET);
                assert_eq!(option, consts::SO_KEEPALIVE);
            }
            _ => panic!("expected Bool variant"),
        }
    }

    #[test]
    fn lookup_socket_option_finds_so_sndbuf() {
        let result = lookup_socket_option("SO_SNDBUF");
        assert!(result.is_some());
        match result.unwrap() {
            SocketOptionKind::Int { level, option } => {
                assert_eq!(level, consts::SOL_SOCKET);
                assert_eq!(option, consts::SO_SNDBUF);
            }
            _ => panic!("expected Int variant"),
        }
    }

    #[test]
    fn lookup_socket_option_finds_tcp_nodelay() {
        let result = lookup_socket_option("TCP_NODELAY");
        assert!(result.is_some());
        match result.unwrap() {
            SocketOptionKind::Bool { level, option } => {
                assert_eq!(level, consts::IPPROTO_TCP);
                assert_eq!(option, consts::TCP_NODELAY);
            }
            _ => panic!("expected Bool variant"),
        }
    }

    #[test]
    fn lookup_socket_option_returns_none_for_unknown() {
        assert!(lookup_socket_option("UNKNOWN_OPTION").is_none());
        assert!(lookup_socket_option("SO_INVALID").is_none());
        assert!(lookup_socket_option("").is_none());
    }

    #[cfg(not(target_family = "windows"))]
    #[test]
    fn lookup_socket_option_finds_iptos_lowdelay() {
        let result = lookup_socket_option("IPTOS_LOWDELAY");
        assert!(result.is_some());
        match result.unwrap() {
            SocketOptionKind::On {
                level,
                option,
                value,
            } => {
                assert_eq!(level, libc::IPPROTO_IP);
                assert_eq!(option, libc::IP_TOS);
                assert_eq!(value, IPTOS_LOWDELAY);
            }
            _ => panic!("expected On variant"),
        }
    }

    #[cfg(not(target_family = "windows"))]
    #[test]
    fn lookup_socket_option_finds_iptos_throughput() {
        let result = lookup_socket_option("IPTOS_THROUGHPUT");
        assert!(result.is_some());
        match result.unwrap() {
            SocketOptionKind::On {
                level,
                option,
                value,
            } => {
                assert_eq!(level, libc::IPPROTO_IP);
                assert_eq!(option, libc::IP_TOS);
                assert_eq!(value, IPTOS_THROUGHPUT);
            }
            _ => panic!("expected On variant"),
        }
    }

    #[test]
    fn intern_name_returns_static_str_for_known_options() {
        assert_eq!(intern_name("SO_KEEPALIVE"), "SO_KEEPALIVE");
        assert_eq!(intern_name("SO_REUSEADDR"), "SO_REUSEADDR");
        assert_eq!(intern_name("SO_BROADCAST"), "SO_BROADCAST");
        assert_eq!(intern_name("SO_SNDBUF"), "SO_SNDBUF");
        assert_eq!(intern_name("SO_RCVBUF"), "SO_RCVBUF");
        assert_eq!(intern_name("SO_SNDLOWAT"), "SO_SNDLOWAT");
        assert_eq!(intern_name("SO_RCVLOWAT"), "SO_RCVLOWAT");
        assert_eq!(intern_name("SO_SNDTIMEO"), "SO_SNDTIMEO");
        assert_eq!(intern_name("SO_RCVTIMEO"), "SO_RCVTIMEO");
        assert_eq!(intern_name("TCP_NODELAY"), "TCP_NODELAY");
        assert_eq!(intern_name("IPTOS_LOWDELAY"), "IPTOS_LOWDELAY");
        assert_eq!(intern_name("IPTOS_THROUGHPUT"), "IPTOS_THROUGHPUT");
    }

    #[test]
    fn lookup_socket_option_finds_so_reuseaddr() {
        let result = lookup_socket_option("SO_REUSEADDR");
        assert!(result.is_some());
        match result.unwrap() {
            SocketOptionKind::Bool { level, option } => {
                assert_eq!(level, consts::SOL_SOCKET);
                assert_eq!(option, consts::SO_REUSEADDR);
            }
            _ => panic!("expected Bool variant"),
        }
    }

    #[test]
    fn lookup_socket_option_finds_so_rcvbuf() {
        let result = lookup_socket_option("SO_RCVBUF");
        assert!(result.is_some());
        match result.unwrap() {
            SocketOptionKind::Int { level, option } => {
                assert_eq!(level, consts::SOL_SOCKET);
                assert_eq!(option, consts::SO_RCVBUF);
            }
            _ => panic!("expected Int variant"),
        }
    }

    #[test]
    fn lookup_socket_option_finds_so_sndtimeo() {
        let result = lookup_socket_option("SO_SNDTIMEO");
        assert!(result.is_some());
        match result.unwrap() {
            SocketOptionKind::Int { level, option } => {
                assert_eq!(level, consts::SOL_SOCKET);
                assert_eq!(option, consts::SO_SNDTIMEO);
            }
            _ => panic!("expected Int variant"),
        }
    }

    #[test]
    fn lookup_socket_option_finds_so_rcvtimeo() {
        let result = lookup_socket_option("SO_RCVTIMEO");
        assert!(result.is_some());
        match result.unwrap() {
            SocketOptionKind::Int { level, option } => {
                assert_eq!(level, consts::SOL_SOCKET);
                assert_eq!(option, consts::SO_RCVTIMEO);
            }
            _ => panic!("expected Int variant"),
        }
    }

    #[cfg(any(target_family = "unix", target_os = "windows"))]
    #[test]
    fn lookup_socket_option_finds_so_broadcast() {
        let result = lookup_socket_option("SO_BROADCAST");
        assert!(result.is_some());
        match result.unwrap() {
            SocketOptionKind::Bool { level, option } => {
                assert_eq!(level, consts::SOL_SOCKET);
                assert_eq!(option, consts::SO_BROADCAST);
            }
            _ => panic!("expected Bool variant"),
        }
    }
}
