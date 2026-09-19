class E < Formula
  desc "Small, extensible coding agent for your terminal"
  homepage "https://e.intuitum.sh"
  version "0.0.2"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/arocomputer/e/releases/download/v0.0.2/e-aarch64-apple-darwin.tar.gz"
      sha256 "38ab38070cf3382835a8d283195202fc8ecd946a4a7f1a6c24868d72e9de2c46"
    end
    on_intel do
      url "https://github.com/arocomputer/e/releases/download/v0.0.2/e-x86_64-apple-darwin.tar.gz"
      sha256 "c523c17aebbdee88ca834a4b8a4af579a66a9e5fd462561884cecae6b902b2b3"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/arocomputer/e/releases/download/v0.0.2/e-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "9e471de88586e681438adb301dedcfcba833244b05ec7aa70022af21f6bfa653"
    end
    on_intel do
      url "https://github.com/arocomputer/e/releases/download/v0.0.2/e-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "5a2f09c171ce5d750f987efbde73563b681536549706b15a8e6ad056b3015d91"
    end
  end

  def install
    libexec.install "e"
    (libexec/".e-install-method").write "homebrew\n"
    bin.install_symlink libexec/"e" => "e"
  end

  test do
    assert_equal "e #{version}", shell_output("#{bin}/e --version").strip
  end
end
