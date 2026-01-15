class OcRsync < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.3"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.3/oc-rsync-0.5.2-darwin-x86_64.tar.gz"
      sha256 "c4fc832fff7095663380b023d011faff1268310dd2571d47f19cd3d0dbe9c34d"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.3/oc-rsync-0.5.2-darwin-aarch64.tar.gz"
      sha256 "c36f720f129344a61ccb8cf6b7d5c3f7da028451de05288784e44ef42ee85428"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
