class OcRsyncATBeta < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (beta toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.4"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.4/oc-rsync-0.5.4-darwin-x86_64-beta.tar.gz"
      sha256 "bb36a8ff17c96c2078a5428c7b2653958fc3d8351653f4e8c0fbeb66ca876b90"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.4/oc-rsync-0.5.4-darwin-aarch64-beta.tar.gz"
      sha256 "7579cfa756fa03eac71864b863a7e0c101441b6a1efe47ae8328c94d82959732"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
