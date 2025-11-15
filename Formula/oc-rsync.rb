class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "3.4.1a-rust"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v3.4.1a-rust/oc-rsync-3.4.1a-rust-x86_64-apple-darwin.tar.gz"
      sha256 "db0638b532af989af7e3933b026f2945d8f541fd62c40fb922f69ef170fb7bdb"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v3.4.1a-rust/oc-rsync-3.4.1a-rust-aarch64-apple-darwin.tar.gz"
      sha256 "5be1f1d2e8739c27c38b47b2b550fc0a075414edc383dcb4a0a26b6e7f303584"
    end
  end

  def install
    bin.install "oc-rsync"
    bin.install "oc-rsyncd"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
