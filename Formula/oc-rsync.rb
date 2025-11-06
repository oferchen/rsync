# frozen_string_literal: true

class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible client/daemon installed as oc-rsync and oc-rsyncd"
  homepage "https://github.com/oferchen/rsync"
  url "https://github.com/oferchen/rsync/archive/refs/tags/v3.4.1a-rust.tar.gz"
  sha256 "2a1ddf17924d8263e52afd58ff80267517d2e37b1144cbd96999b4d8472e90f8"
  version "3.4.1-rust"
  license "GPL-3.0-or-later"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: ".")

    (etc/"oc-rsyncd").install "packaging/etc/oc-rsyncd/oc-rsyncd.conf"
    (etc/"oc-rsyncd").install "packaging/etc/oc-rsyncd/oc-rsyncd.secrets"
    chmod 0600, etc/"oc-rsyncd/oc-rsyncd.secrets"
    (pkgshare/"examples").install "packaging/examples/oc-rsyncd.conf"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/oc-rsync --version")
  end
end
