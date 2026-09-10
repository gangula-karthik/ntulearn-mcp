# Homebrew formula template for ntulearn-mcp.
#
# Homebrew requires formulae to live in a *tap* repository (a separate GitHub
# repo named homebrew-<name>, e.g. gangula-karthik/homebrew-tap). To use:
#   1. Create that repo and copy this file into Formula/ntulearn-mcp.rb
#   2. Update the url/sha256 below to the release you want to serve
#      (sha256 of the tarball; keep arm/mac + intel/mac variants if you wish)
#   3. Users then install with:
#        brew tap gangula-karthik/tap
#        brew install ntulearn-mcp
#
# Note: a Homebrew install is NOT self-updatable (the updater detects brew
# Cellar paths and tells users to `brew upgrade` instead).
class NtulearnMcp < Formula
  desc "NTULearn Blackboard REST MCP server + agent-first CLI (Rust)"
  homepage "https://github.com/gangula-karthik/ntulearn-mcp"
  url "https://github.com/gangula-karthik/ntulearn-mcp/releases/download/v0.6.0/ntulearn-mcp-aarch64-apple-darwin.tar.gz"
  version "0.6.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000" # replace on release
  license "MIT"

  def install
    bin.install "ntulearn-mcp"
  end

  def caveats
    <<~EOS
      Run `ntulearn-mcp setup` once to capture your NTULearn session cookie.
      Updates: `brew upgrade ntulearn-mcp` (the built-in self-updater defers
      to Homebrew for brew-managed installs).
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/ntulearn-mcp version")
  end
end
