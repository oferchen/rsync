# frozen_string_literal: true

class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible client/daemon installed as oc-rsync"
  homepage "https://github.com/oferchen/rsync"
  url "https://github.com/oferchen/rsync/archive/refs/tags/v3.4.1-rust.tar.gz"
  sha256 "b51196ce14884b4e99c9823b4dbee2cd3815dbdd647f2fba324cd109b00bfda2"
  version "3.4.1-rust"
  license "GPL-3.0-or-later"

  depends_on "rust" => :build

  def install
    system "cargo", "build", "--release", "--locked", "--bin", "oc-rsync", "--bin", "oc-rsyncd"
    bin.install "target/release/oc-rsync"
    bin.install "target/release/oc-rsyncd"

    (etc/"oc-rsyncd").install "packaging/etc/oc-rsyncd/oc-rsyncd.conf"
    (etc/"oc-rsyncd").install "packaging/etc/oc-rsyncd/oc-rsyncd.secrets"
    chmod 0600, etc/"oc-rsyncd/oc-rsyncd.secrets"
    (pkgshare/"examples").install "packaging/examples/oc-rsyncd.conf"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/oc-rsync --version")
    assert_match "oc-rsync", shell_output("#{bin}/oc-rsyncd --help")
  end
end
