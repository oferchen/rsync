class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "master"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/master/oc-rsync-0.5.0-darwin-x86_64.tar.gz"
      sha256 "58e411a48e730c41c13c770c17fc1cc6a101ebecd8b4c09b2c5cffa0c2e8211f"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/master/oc-rsync-0.5.0-darwin-aarch64.tar.gz"
      sha256 "a99777d04375a7fc92cbe79df2c2f850ec5c03d331d930c8c8e4a8c17bfba77d"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
