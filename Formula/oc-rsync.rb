class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.2-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.2"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.2/oc-rsync-0.6.2-darwin-x86_64.tar.gz"
      sha256 "3101715a69db40d41895597197165e2db69e924aae57e67c4c3c23a72a508238"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.2/oc-rsync-0.6.2-darwin-aarch64.tar.gz"
      sha256 "07ce9957ebac33c357bdb23e11b9c306024385fdcb1d3ac3ae22c4a4434ab828"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
