# Match exec limitation in SSH config compression detection

## Background

`oc-rsync` detects SSH-level compression to warn when both rsync wire
compression (`--compress`) and SSH stream compression are enabled
simultaneously. Double-compressing wastes CPU and can expand
already-compressed data.

The detection parses `~/.ssh/config` (or the file specified via
`-F`) and evaluates `Host` blocks, `Match host`, `Match user`,
`Match localuser`, `Match originalhost`, and `Match all` conditions.
This covers the majority of real-world ssh_config layouts.

## Limitation: `Match exec` blocks are not evaluated

OpenSSH supports `Match exec "command"` blocks that run an arbitrary
shell command to decide whether the block applies. For example:

```
Match exec "test -f /etc/vpn.conf"
  Compression yes
  ProxyJump bastion.example.com
```

oc-rsync deliberately does **not** evaluate `Match exec` conditions.
The reasons are:

1. **Security** - executing user-supplied shell commands from a passive
   config-lookup path inverts the trust model. The compression check is
   advisory; spawning subprocesses from it is disproportionate.

2. **Performance** - fork/exec on every SSH transfer adds measurable
   overhead for a one-shot advisory warning.

3. **Reproducibility** - replicating OpenSSH's exact shell selection
   (`$SHELL`, `/bin/sh` fallback, Windows `cmd /c`) adds a surface
   area that can diverge from OpenSSH's behavior.

When a `Match exec` block contains `Compression yes`, oc-rsync emits a
warning to stderr:

```
warning: ssh_config contains "Compression yes" inside a "Match exec" block.
         The exec condition was not evaluated because executing arbitrary
         commands from a config-lookup path is a security risk. If SSH
         compression is active, oc-rsync's --compress will double-compress.
         Workaround: move "Compression yes" to a Host or Match host block,
         or pass -e "ssh -C" explicitly so oc-rsync can detect it.
```

## Workarounds

If your ssh_config uses `Match exec` to conditionally enable compression,
you have several options to ensure oc-rsync detects it:

### Option 1: Move compression to a Host or Match host block

Replace the `Match exec` block with a `Host` or `Match host` block
that oc-rsync can evaluate:

```
# Before (not detected by oc-rsync):
Match exec "test -f /etc/vpn.conf"
  Compression yes

# After (detected by oc-rsync):
Host vpn-*.example.com
  Compression yes
```

### Option 2: Pass compression explicitly on the command line

Use `-e "ssh -C"` or `-e "ssh -o Compression=yes"` so the
compression flag appears in the SSH argv where oc-rsync always
detects it:

```sh
oc-rsync -e "ssh -C" src/ host:dest/
```

### Option 3: Separate the compression directive

Move `Compression yes` out of the `Match exec` block into a scope
that oc-rsync evaluates. Other directives can remain inside the exec
block:

```
# oc-rsync detects this:
Match host vpn-*.example.com
  Compression yes

# oc-rsync skips this block but the non-compression directives
# are only relevant to OpenSSH, not to compression detection:
Match exec "test -f /etc/vpn.conf"
  ProxyJump bastion.example.com
```

## What is not affected

- **Compression detection via argv** (`-C`, `-o Compression=yes`) is
  always detected regardless of `Match exec` blocks.

- **Top-level directives** and **Host blocks** (including glob and
  negation patterns) are fully evaluated.

- **Match host**, **Match user**, **Match localuser**,
  **Match originalhost**, and **Match all** conditions are fully
  evaluated.

- The `Match exec` limitation only affects compression *detection*
  for the double-compression warning. It does not affect the actual
  SSH connection, which is established by the system `ssh` client
  that evaluates all `Match exec` conditions normally.

## References

- `docs/design/ssc-4a-match-conditions.md` - DEFER decision for exec
- `crates/rsync_io/src/ssh/config_lookup.rs` - parser implementation
