class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.2-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.3"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.3/oc-rsync-0.6.3-darwin-x86_64.tar.gz"
      sha256 "d30d5f6b5068a79330c6a899571b5cc379b56a2cbe98fd3632c69c3558ca72cf"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.3/oc-rsync-0.6.3-darwin-aarch64.tar.gz"
      sha256 "2980d6e3e112e4326b8ad0eaa62c44ef23034cc3b45a5e1d71ac410cdfc8150e"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
