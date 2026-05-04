# Daemon TLS-in-Front Deployment

`oc-rsync --daemon` does not implement TLS natively. To expose it over an
untrusted network, bind the daemon to the loopback interface and run a TLS
terminator in front of it. This document gives runnable recipes for the three
supported terminators - `stunnel`, `ssh -L`, and HAProxy in TCP mode - plus
hardened systemd unit excerpts and host-firewall rules that prevent external
access to the loopback-bound daemon port.

The daemon's default listening port is **873/tcp** (matches upstream rsync's
`rsync://` scheme; see `crates/daemon/src/daemon.rs::DEFAULT_PORT`). The
canonical "rsync over TLS" external port used by upstream `stunnel-rsyncd.conf`
is **874/tcp**. Both are used consistently throughout the examples below.

> **Threat model.** These recipes assume an operator who controls the
> terminator host, the daemon host (or both, when colocated), and a CA whose
> certificates the clients trust. They do **not** address client-side TLS
> verification of an `rsync://` URL - that is performed by the client's TLS
> terminator (`stunnel` client mode, `ssh`, or an HTTPS proxy).

## Contents

- [1. Bind the daemon to loopback](#1-bind-the-daemon-to-loopback)
- [2. stunnel (server side)](#2-stunnel-server-side)
- [3. SSH local-port-forward (`ssh -L`)](#3-ssh-local-port-forward-ssh--l)
- [4. HAProxy in TCP mode](#4-haproxy-in-tcp-mode)
- [5. systemd unit excerpts](#5-systemd-unit-excerpts)
- [6. Firewall guidance](#6-firewall-guidance)
- [7. Verification checklist](#7-verification-checklist)

---

## 1. Bind the daemon to loopback

The terminator and the daemon must agree that only the loopback interface (or
a private VPC/wireguard interface) is reachable. Either set `address = ` in
`oc-rsyncd.conf` or pass `--address` on the command line. This example uses
the config file approach so the bind address is auditable in version control.

`/etc/oc-rsyncd/oc-rsyncd.conf`:

```ini
# Global daemon settings - mirrors upstream rsyncd.conf(5) syntax.
address = 127.0.0.1
port = 873
uid = nobody
gid = nogroup
use chroot = yes
max connections = 16
timeout = 600
log file = /var/log/oc-rsyncd.log
pid file = /run/oc-rsyncd.pid

[backups]
    path = /srv/backups
    comment = Off-site backup mirror
    read only = no
    list = no
    auth users = backup
    secrets file = /etc/oc-rsyncd/oc-rsyncd.secrets
    hosts allow = 127.0.0.1
    hosts deny = *
    refuse options = delete-excluded
```

`/etc/oc-rsyncd/oc-rsyncd.secrets` (mode `0600`, owned by the daemon user):

```text
backup:CHANGE-ME-TO-A-LONG-RANDOM-SECRET
```

Note the `hosts allow = 127.0.0.1` / `hosts deny = *` pair. Even with a
loopback bind, this is defense-in-depth: if the operator later widens the
listening address, the daemon-layer ACL still rejects non-loopback peers
until the configuration is re-reviewed. `hosts allow` runs before
authentication, so it is the cheapest layer to enforce.

---

## 2. stunnel (server side)

`stunnel` is the simplest terminator and is what upstream rsync ships an
example for (`stunnel-rsyncd.conf` in the upstream tree). Install it from
distro packages (`stunnel`, `stunnel4`, or `stunnel5` depending on the
distribution).

`/etc/stunnel/oc-rsyncd.conf`:

```ini
# Global stunnel settings.
foreground = no
pid = /run/stunnel-oc-rsyncd.pid
output = /var/log/stunnel-oc-rsyncd.log

socket = l:TCP_NODELAY=1
socket = r:TCP_NODELAY=1

# Drop privileges after binding the listening port.
setuid = stunnel
setgid = stunnel

[oc-rsync]
accept = 0.0.0.0:874
connect = 127.0.0.1:873

# Server certificate. A combined PEM (cert + key) works too; in that case
# omit `key` and point `cert` at the combined file.
cert = /etc/stunnel/certs/host.example.fullchain.pem
key  = /etc/stunnel/certs/host.example.key
client = no

# Require modern TLS. stunnel 5.56+ accepts the explicit minimum; older
# versions use `sslVersion = TLSv1.2` instead.
sslVersionMin = TLSv1.2
ciphers = HIGH:!aNULL:!MD5:!RC4

# Public-CA mode: any client trusted by the system CA bundle may connect.
verify = 0
CAfile = /etc/ssl/certs/ca-certificates.crt

# To require client certificates instead, comment out the two lines above
# and uncomment these:
# verify = 3
# CAfile = /etc/stunnel/certs/allowed-clients.pem
```

Client side, the operator runs `stunnel` in client mode (or uses
`rsync-ssl`/`openrsync` style wrappers). A minimal client config:

`/etc/stunnel/oc-rsync-client.conf`:

```ini
foreground = no
pid = /run/stunnel-oc-rsync-client.pid

[oc-rsync]
client = yes
accept = 127.0.0.1:1873
connect = host.example:874
verify = 2
CAfile = /etc/ssl/certs/ca-certificates.crt
checkHost = host.example
sslVersionMin = TLSv1.2
```

The client then pulls or pushes through the local terminator:

```bash
oc-rsync -av rsync://127.0.0.1:1873/backups/ ./local-mirror/
oc-rsync -av ./local-mirror/ rsync://127.0.0.1:1873/backups/
```

---

## 3. SSH local-port-forward (`ssh -L`)

For ad-hoc operator access (no public listener, no certificate management),
forward the daemon's loopback port over an existing SSH connection. This is
the lowest-overhead recipe and reuses the SSH host key as the trust anchor.

On the operator workstation:

```bash
ssh -N -L 1873:127.0.0.1:873 admin@host.example
```

`-N` disables the remote shell so the SSH connection serves only as a
forwarder. In another terminal, talk to the local end of the tunnel:

```bash
oc-rsync -av rsync://127.0.0.1:1873/backups/ ./local-mirror/
```

For long-running access, prefer a dedicated SSH config block so the tunnel
is reproducible and survives reboots when paired with `autossh` or a systemd
user unit:

`~/.ssh/config`:

```text
Host oc-rsyncd-tunnel
    HostName host.example
    User admin
    IdentityFile ~/.ssh/id_ed25519
    LocalForward 1873 127.0.0.1:873
    ServerAliveInterval 30
    ServerAliveCountMax 3
    ExitOnForwardFailure yes
```

Then:

```bash
ssh -N oc-rsyncd-tunnel
```

`ExitOnForwardFailure yes` is important: without it, SSH silently keeps the
control connection alive even when the forward fails to bind, and rsync runs
will hang on the local port.

---

## 4. HAProxy in TCP mode

Use HAProxy when the operator already runs it as the edge load balancer or
when the daemon should sit behind a TLS terminator that supports
PROXY-protocol, multiple backends, or rate limiting.

`/etc/haproxy/haproxy.cfg`:

```text
global
    log /dev/log local0
    maxconn 4096
    user haproxy
    group haproxy
    daemon
    # Modern TLS defaults (mozilla intermediate, 2024).
    ssl-default-bind-options ssl-min-ver TLSv1.2 no-tls-tickets
    ssl-default-bind-ciphers ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-CHACHA20-POLY1305:ECDHE-RSA-CHACHA20-POLY1305:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384

defaults
    mode tcp
    log global
    option tcplog
    option dontlognull
    timeout connect 10s
    timeout client  1h
    timeout server  1h

frontend oc-rsync-tls
    bind 0.0.0.0:874 ssl crt /etc/haproxy/certs/host.example.pem
    default_backend oc-rsync-loopback

backend oc-rsync-loopback
    server local 127.0.0.1:873 check
```

The combined PEM expected by HAProxy is the server certificate concatenated
with the private key (and any intermediate certs):

```bash
cat host.example.fullchain.pem host.example.key \
    > /etc/haproxy/certs/host.example.pem
chmod 0600 /etc/haproxy/certs/host.example.pem
chown haproxy:haproxy /etc/haproxy/certs/host.example.pem
```

If the daemon should see the original client IP (for `hosts allow` ACLs and
log correlation), enable HAProxy's PROXY protocol on the backend and set
`proxy protocol = yes` in the global section of `oc-rsyncd.conf`. Add
`send-proxy-v2` to the backend `server` line:

```text
server local 127.0.0.1:873 check send-proxy-v2
```

oc-rsync supports v1 and v2 PROXY headers via `proxy protocol = yes`; see
`crates/daemon/src/daemon/sections/proxy_protocol.rs` for the supported
formats. Without `send-proxy-v2`, the daemon sees `127.0.0.1` as every
client's source address, which is also a valid choice when client-IP ACLs
are enforced by HAProxy itself rather than the daemon.

---

## 5. systemd unit excerpts

Each terminator needs its own systemd unit. The daemon unit shipped in
`packaging/systemd/oc-rsyncd.service` is already hardened (`NoNewPrivileges`,
`ProtectSystem=full`, `PrivateTmp`, etc.); the excerpts below add unit
ordering so the terminator starts only after the daemon is ready.

### 5a. oc-rsyncd hardening (drop-in)

`/etc/systemd/system/oc-rsyncd.service.d/loopback.conf`:

```ini
[Service]
# Reinforce the loopback bind from outside the rsync config.
ExecStartPre=/usr/sbin/sysctl -q net.ipv4.ip_unprivileged_port_start=1024
# Without CAP_NET_BIND_SERVICE the daemon cannot bind 873 on Linux.
# It is set in the shipped unit; the drop-in keeps it explicit here.
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_CHOWN CAP_DAC_OVERRIDE CAP_FOWNER CAP_FSETID CAP_MKNOD CAP_NET_BIND_SERVICE
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
SystemCallArchitectures=native
SystemCallFilter=@system-service
SystemCallFilter=~@privileged @resources
```

Apply with `systemctl daemon-reload && systemctl restart oc-rsyncd`.

### 5b. stunnel terminator unit

`/etc/systemd/system/stunnel-oc-rsyncd.service`:

```ini
[Unit]
Description=stunnel TLS terminator for oc-rsyncd
After=network-online.target oc-rsyncd.service
Requires=oc-rsyncd.service
Wants=network-online.target

[Service]
Type=forking
ExecStart=/usr/bin/stunnel /etc/stunnel/oc-rsyncd.conf
PIDFile=/run/stunnel-oc-rsyncd.pid
Restart=on-failure
RestartSec=5s

NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ReadWritePaths=/var/log /run
ProtectHome=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
LockPersonality=true
MemoryDenyWriteExecute=true
RestrictAddressFamilies=AF_INET AF_INET6
RestrictNamespaces=true
SystemCallArchitectures=native
SystemCallFilter=@system-service
SystemCallFilter=~@privileged @resources
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
```

### 5c. HAProxy terminator unit (drop-in)

Most distributions ship `/lib/systemd/system/haproxy.service`. Add a drop-in
that orders it after the daemon and tightens it:

`/etc/systemd/system/haproxy.service.d/oc-rsyncd.conf`:

```ini
[Unit]
After=oc-rsyncd.service
Wants=oc-rsyncd.service

[Service]
ProtectSystem=strict
ReadWritePaths=/var/log /run /run/haproxy
ProtectHome=true
NoNewPrivileges=true
PrivateTmp=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
SystemCallArchitectures=native
```

### 5d. SSH-tunnel user unit (operator workstation)

`~/.config/systemd/user/oc-rsyncd-tunnel.service`:

```ini
[Unit]
Description=SSH local-forward to oc-rsyncd
After=network-online.target

[Service]
Type=simple
ExecStart=/usr/bin/ssh -N -o ExitOnForwardFailure=yes -o ServerAliveInterval=30 oc-rsyncd-tunnel
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=default.target
```

Enable with `systemctl --user enable --now oc-rsyncd-tunnel`. The unit relies
on the `oc-rsyncd-tunnel` block in the user's `~/.ssh/config` from
[section 3](#3-ssh-local-port-forward-ssh--l).

---

## 6. Firewall guidance

The daemon listens on `127.0.0.1:873`. The terminator listens on the public
interface (`0.0.0.0:874` for stunnel/HAProxy). The host firewall must:

1. Allow inbound `tcp/874` from authorised client networks.
2. Drop inbound `tcp/873` on every non-loopback interface.
3. Allow loopback freely (otherwise the terminator cannot reach the daemon).

### nftables (recommended on systemd distributions)

`/etc/nftables.d/oc-rsyncd.nft`:

```nft
table inet oc_rsyncd {
    chain input {
        type filter hook input priority filter; policy accept;

        # Loopback is always permitted.
        iif "lo" accept

        # Public TLS port - restrict to your client CIDR(s).
        ip  saddr 198.51.100.0/24 tcp dport 874 accept
        ip6 saddr 2001:db8::/32   tcp dport 874 accept

        # Belt and braces: never expose the daemon's plaintext port.
        iif != "lo" tcp dport 873 drop
    }
}
```

Load with `nft -f /etc/nftables.d/oc-rsyncd.nft` and persist via the
`nftables.service` unit your distribution provides.

### iptables (legacy)

```bash
# Permit loopback unconditionally.
iptables -A INPUT -i lo -j ACCEPT

# Allow public clients to reach the TLS terminator.
iptables -A INPUT -p tcp --dport 874 -s 198.51.100.0/24 -j ACCEPT

# Drop any non-loopback attempt to reach the daemon's plaintext port.
iptables -A INPUT -p tcp --dport 873 ! -i lo -j DROP
```

Persist with `iptables-save > /etc/iptables/rules.v4` (Debian/Ubuntu) or
`service iptables save` (RHEL family).

### Container bridges (Docker / Podman)

When the daemon runs in a container, the host firewall above does not see
container-to-container traffic on the default bridge. Two safe patterns:

- **Host networking + firewall (preferred).** Run the daemon container with
  `--network=host` and let the host nftables/iptables rules above enforce
  isolation.
- **User-defined bridge with explicit publish.** Create an isolated bridge
  and publish only the terminator's port:

  ```bash
  podman network create --internal oc-rsyncd-net
  podman run -d --name oc-rsyncd --network oc-rsyncd-net \
      -v /etc/oc-rsyncd:/etc/oc-rsyncd:ro \
      ghcr.io/oferchen/oc-rsync:latest \
      --daemon --no-detach --config /etc/oc-rsyncd/oc-rsyncd.conf
  podman run -d --name stunnel --network oc-rsyncd-net \
      -p 0.0.0.0:874:874 \
      -v /etc/stunnel:/etc/stunnel:ro \
      stunnel
  ```

  `--internal` prevents the daemon container from being reached from outside
  the bridge, while `-p 0.0.0.0:874:874` only exposes the terminator. The
  same pattern works with `docker network create --internal`.

Verify isolation from another host:

```bash
nc -zv host.example 873   # must time out or be refused
nc -zv host.example 874   # must connect
```

---

## 7. Verification checklist

Before declaring a deployment production-ready:

- [ ] `ss -tlnp | grep oc-rsync` shows the daemon bound to `127.0.0.1:873`,
      not `0.0.0.0:873`.
- [ ] `ss -tlnp | grep ':874'` shows the terminator bound to a public
      interface.
- [ ] `openssl s_client -connect host.example:874 -servername host.example`
      negotiates TLS 1.2+ and returns the expected certificate chain.
- [ ] `nc -zv host.example 873` from a non-loopback peer fails (firewall
      drop or RST).
- [ ] An end-to-end transfer succeeds:
      `oc-rsync -av rsync://127.0.0.1:1873/backups/ /tmp/check/`
      via the client-side stunnel or SSH tunnel.
- [ ] `journalctl -u oc-rsyncd -u stunnel-oc-rsyncd` shows clean startup
      and the daemon's `connect from 127.0.0.1` log line on each transfer.
- [ ] Secrets file is `0600` and owned by the daemon user only:
      `stat -c '%a %U:%G' /etc/oc-rsyncd/oc-rsyncd.secrets`.

If any item fails, do not advertise the daemon to external clients - the
`hosts deny = *` rule from [section 1](#1-bind-the-daemon-to-loopback) is
the last line of defense.

---

## See also

- `SECURITY.md` - "Daemon TLS" hardening note that links here.
- `packaging/systemd/oc-rsyncd.service` - shipped, hardened daemon unit.
- `packaging/etc/oc-rsyncd/oc-rsyncd.conf` - example daemon configuration.
- `target/interop/upstream-src/rsync-3.4.1/stunnel-rsyncd.conf` - upstream
  rsync's reference stunnel example.
- `rsyncd.conf(5)` - canonical syntax for daemon directives.
