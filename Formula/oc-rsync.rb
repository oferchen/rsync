class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.1"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.1/oc-rsync-0.5.1-darwin-x86_64.tar.gz"
      sha256 "d62a480d337ce628ee61dce33c181ca2f34521ef4c73e6b77567e368e7ae0b03"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.1/oc-rsync-0.5.1-darwin-aarch64.tar.gz"
      sha256 "68b45ce458d0c112d49397c625f5d4e3a97eaee1726467678abcac2581b69cd8"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
