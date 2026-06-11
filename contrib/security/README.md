# LSM profile templates for oc-rsyncd

This directory ships AppArmor and SELinux templates for the `oc-rsyncd` daemon. They are starting points for distribution packagers and site operators, not strict requirements. Operators are expected to extend the module-root and tmpdir stanzas to match their deployment.

These templates compose with the `--features landlock` defense-in-depth layer documented in [docs/packaging/landlock-feature-guidance.md](../../docs/packaging/landlock-feature-guidance.md). Landlock is purely additive and stacks with both AppArmor and SELinux.

## AppArmor (Ubuntu LTS, openSUSE, Debian)

File: `usr.sbin.oc-rsyncd.apparmor`

### Install

```sh
sudo cp usr.sbin.oc-rsyncd.apparmor /etc/apparmor.d/usr.sbin.oc-rsyncd
sudo apparmor_parser -r /etc/apparmor.d/usr.sbin.oc-rsyncd
```

### Verify

```sh
sudo aa-status | grep oc-rsyncd
```

The profile should appear in the "profiles are in enforce mode" list.

### Customize

Before reloading, edit the profile and add the module roots your `oc-rsyncd.conf` exposes. The shipped template includes commented examples:

```
# /srv/backups/** rw,
# /var/lib/oc-rsync-modules/** rw,
# /tmp/oc-rsync-stage/** rw,
```

Uncomment and adjust to match your `path =` lines.

### Troubleshooting

If the daemon hits a permission denied that does not appear in `oc-rsyncd.log`, check the kernel audit log:

```sh
sudo journalctl -k --grep="apparmor.*DENIED.*oc-rsyncd"
```

Add the missing path to the profile and reload with `apparmor_parser -r`.

## SELinux (RHEL, Fedora, CentOS Stream)

Files: `oc_rsyncd.te`, `oc_rsyncd.fc`, `oc_rsyncd.if`

### Build

Install the SELinux policy development tooling first:

```sh
sudo dnf install selinux-policy-devel
```

Then build the policy module:

```sh
make -f /usr/share/selinux/devel/Makefile oc_rsyncd.pp
```

### Install

```sh
sudo semodule -i oc_rsyncd.pp
sudo chcon -t oc_rsyncd_exec_t /usr/sbin/oc-rsyncd
```

If the binary lives elsewhere (for example `/usr/local/sbin/oc-rsyncd` from a source build), edit `oc_rsyncd.fc` to match before building.

### Verify

```sh
ls -Z /usr/sbin/oc-rsyncd
sudo semodule -l | grep oc_rsyncd
```

The first command should show `oc_rsyncd_exec_t`; the second should list `oc_rsyncd` as an installed module.

### Customize

The template grants read/write on `rsync_data_t`, which is the existing label used by upstream rsync. Run

```sh
sudo semanage fcontext -a -t rsync_data_t '/srv/backups(/.*)?'
sudo restorecon -Rv /srv/backups
```

to apply the label to your module roots. Repeat for each `path =` directory in `oc-rsyncd.conf`.

### Troubleshooting

If the daemon hits an AVC denial, capture it:

```sh
sudo ausearch -m AVC -c oc-rsyncd --start recent
```

Generate a supplemental policy stanza with `audit2allow` and review it before installing:

```sh
sudo ausearch -m AVC -c oc-rsyncd --start recent | audit2allow -M oc_rsyncd_local
sudo semodule -i oc_rsyncd_local.pp
```

## Cross-references

- [docs/packaging/landlock-feature-guidance.md](../../docs/packaging/landlock-feature-guidance.md) - Landlock build/runtime guidance for the same distro audience.
- Upstream rsync ships no AppArmor or SELinux templates of its own, so these files do not have a one-to-one counterpart in `target/interop/upstream-src/rsync-3.4.4/packaging/`. The `rsync_data_t` SELinux label referenced in `oc_rsyncd.te` is provided by the base `selinux-policy` package and is the same label upstream `rsync` uses.
