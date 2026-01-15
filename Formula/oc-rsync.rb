class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.2"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.2/oc-rsync-0.5.2-darwin-x86_64.tar.gz"
      sha256 "469c148c86ddfd448a9eda50e4e4516e45f2954ee618fd546355a788412b3a7a"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.2/oc-rsync-0.5.2-darwin-aarch64.tar.gz"
      sha256 "c91049857d91bf3b00b4655725c2b9e92e68d8feed542a0c752b90a5882b9b55"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
