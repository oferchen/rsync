class OcRsyncATNightly < Formula
  desc "Pure-Rust rsync 3.4.2-compatible implementation (nightly toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.6.3"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.3/oc-rsync-0.6.3-darwin-x86_64-nightly.tar.gz"
      sha256 "65fd5a4274895f2cf18330869ec0c271b862a44624c1ee0173fde7e0764bc425"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.6.3/oc-rsync-0.6.3-darwin-aarch64-nightly.tar.gz"
      sha256 "a5aad4189abdbec02342b2d1f32e3815c851c43bb091e0f5c777d5b7b36b30cf"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
