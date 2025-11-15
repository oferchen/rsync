class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "3.4.1-rust"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v3.4.1-rust/oc-rsync-3.4.1-rust-darwin-x86_64.tar.gz"
      sha256 "a8d50e19aec94bef6d9987ed8050fa36ad82993d6b606af61a6b98eb578af626"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v3.4.1-rust/oc-rsync-3.4.1-rust-darwin-aarch64.tar.gz"
      sha256 "0a99de03f06b8532738a61325d80cfbff960218432c74cfcc2a88c976db1eae3"
    end
  end

  def install
    bin.install "oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
