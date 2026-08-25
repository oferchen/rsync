class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.2-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.2"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.2/oc-rsync-0.6.2-darwin-x86_64.tar.gz"
      sha256 "b1a08bb1cef95dc4f47898eaca05240e04a24b64cc4adb245a988e6c2a004fa9"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.2/oc-rsync-0.6.2-darwin-aarch64.tar.gz"
      sha256 "3510650b5564ca03b8ed771ad3684d76ccf4c602a13510a31951462ea451aca9"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
