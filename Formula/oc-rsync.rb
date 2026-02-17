class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.6"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.6/oc-rsync-0.5.6-darwin-x86_64.tar.gz"
      sha256 "dcdb6af4e77cc8dc3983d3558ec088841723778ce074de8d47209fbcdbc29cac"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.6/oc-rsync-0.5.6-darwin-aarch64.tar.gz"
      sha256 "a890d21d0f1e3850d3677991bbebcf529f0fb60a24b3242f22394f11cc8648b6"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
