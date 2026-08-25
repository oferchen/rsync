class OcRsyncATNightly < Formula
  desc "Pure-Rust rsync 3.4.2-compatible implementation (nightly toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.2"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.2/oc-rsync-0.6.2-darwin-x86_64-nightly.tar.gz"
      sha256 "b2ca659cff41622ab71e155fbc97f71751d4b5d28286262b78d34001892ee118"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.2/oc-rsync-0.6.2-darwin-aarch64-nightly.tar.gz"
      sha256 "bf67c62f46932cac8c953e9267f7a02a78b5d2f950da35c973e94e592cb0ee2e"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
