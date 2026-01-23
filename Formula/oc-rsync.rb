class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.3"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.3/oc-rsync-0.5.3-darwin-x86_64.tar.gz"
      sha256 "e96c368924a3c249fa6c796a1183892d819cac3972bfd72a9374c05c7958be4c"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.3/oc-rsync-0.5.3-darwin-aarch64.tar.gz"
      sha256 "9104e94afbaec72e9f4a1c8d00d8e9e788cca56f37b194527123c95418d20b58"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
