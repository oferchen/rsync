class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.1"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.1/oc-rsync-0.5.0-darwin-x86_64.tar.gz"
      sha256 "04324066724b80940f79079d5e8bc711cd49dcd52154c35aed085b973ae65d06"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.1/oc-rsync-0.5.0-darwin-aarch64.tar.gz"
      sha256 "0c701daf2290b1634b4183bcd9edc07d25328ed0a19886ed5d6d62ad98d4977d"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
