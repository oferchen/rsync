class OcRsyncATNightly < Formula
  desc "Pure-Rust rsync 3.4.1-compatible implementation (nightly toolchain)"
  homepage "https://github.com/oferchen/rsync"
  license "GPL-3.0-or-later"
  version "0.5.4"

  on_macos do
    on_intel do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.4/oc-rsync-0.5.4-darwin-x86_64-nightly.tar.gz"
      sha256 "b3c5d88ecf821be4a57edb3ea336341a94cd1de6686970d20ba2d84d387ad8a1"
    end

    on_arm do
      url "https://github.com/oferchen/rsync/releases/download/v0.5.4/oc-rsync-0.5.4-darwin-aarch64-nightly.tar.gz"
      sha256 "659568bc8b9d32c2e94c147e8ba251805ab9f972409b59dfd7b458b24855a900"
    end
  end

  def install
    bin.install "bin/oc-rsync"
  end

  test do
    system "#{bin}/oc-rsync", "--version"
  end
end
