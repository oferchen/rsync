class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.9"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.9/oc-rsync-0.5.9-darwin-x86_64.tar.gz"
      sha256 "e461d5a662f8ee4eca2f385844353a7aa3584a014560f391808951d0765d9b25"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.9/oc-rsync-0.5.9-darwin-aarch64.tar.gz"
      sha256 "66c54d26826f002605266e7afec58e6884bf71fad2df927808ca2470b819533b"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
