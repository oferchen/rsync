# Side-by-side installation guide

The oc-rsync packages install the Rust client and daemon alongside any
system-provided rsync packages without overwriting upstream binaries. This
reference explains the layout, verification steps, and coexistence behavior on
Debian/Ubuntu, RPM-based systems, and Homebrew installs.

## Binaries and version output

* Client binary: `/usr/bin/oc-rsync`
* Daemon binary: `/usr/sbin/oc-rsyncd`
* Version string: `oc-rsync 3.4.1-rust` / `oc-rsyncd 3.4.1-rust`

Run both binaries to confirm the install and to show that they are distinct from
`/usr/bin/rsync`:

```sh
/usr/bin/rsync --version
/usr/bin/oc-rsync --version
```

Both commands succeed because oc-rsync does not touch the system-provided
`rsync` binary.

## Configuration files

The daemon reads `/etc/oc-rsyncd/oc-rsyncd.conf` and keeps its secrets in
`/etc/oc-rsyncd/oc-rsyncd.secrets` (0600). Packages ship both files marked as
configuration so local changes survive upgrades.

A copy of the default configuration is also provided at
`/usr/share/doc/oc-rsync/examples/oc-rsyncd.conf` for quick reference or for
bootstrapping new deployments.

## Homebrew installation

The repository ships a Homebrew formula at `Formula/oc-rsync.rb`. Homebrew
builds install the binaries under the Homebrew prefix, so the system `rsync`
package remains untouched:

```sh
brew install ./Formula/oc-rsync.rb
brew list oc-rsync
```

The install places `oc-rsync` and `oc-rsyncd` under `$(brew --prefix)/bin` and
creates the daemon configuration directory at
`$(brew --prefix)/etc/oc-rsyncd`. The Homebrew audit job verifies that the
formula points at the current release tarball and that the installed binaries
use the `oc-` prefix. Existing Homebrew `rsync` installations are unaffected
because no conflicting files are installed.

## Systemd unit

The packaged systemd unit is `oc-rsyncd.service` and starts the daemon with
`/usr/sbin/oc-rsyncd`. It does **not** replace or conflict with any existing
`rsync.service` units. Enable or start it with:

```sh
sudo systemctl enable --now oc-rsyncd.service
```

Environment overrides can be placed in `/etc/default/oc-rsyncd`; no
update-alternatives hooks are registered, so `/usr/bin/rsync` remains untouched.

## Removal

Uninstalling the package simply removes the oc-rsync binaries and configuration
files. Because no alternatives were registered, the system `rsync` continues to
function normally after removal.
