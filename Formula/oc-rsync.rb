class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.1"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.1/oc-rsync-0.5.1-darwin-x86_64.tar.gz"
      sha256 "ab5a25b31a2b154143d0e9b6696e3d9a882890a01bd8220ae10290397beb58af"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.1/oc-rsync-0.5.1-darwin-aarch64.tar.gz"
      sha256 "35141d43d84588cf2905e62d315c270ea07c4e4c0f3cb5c0c53a06c79bfe2ba0"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
