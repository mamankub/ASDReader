# Self-hosted CI

CI and release jobs must use only the owner's infrastructure. No hosted fallback
is allowed. Missing runners leave required checks pending and must be reported.

## Routing

- Linux: `self-hosted, Linux, X64` (or `ARM64`), `asd-ci`, plus the requested
  `ubuntu-24.04` or `ubuntu-22.04` environment label. Use an actual matching OS image.
- Windows: `self-hosted, Windows, X64` (or `ARM64`), `asd-ci`. Windows GUI tests
  require a logged-in desktop session. Do not claim ARM64 coverage from x64 tests.
- macOS: `self-hosted, macOS, ARM64` (or `X64` where requested), `asd-ci`.
- asd-project-library retains its existing `asd-library-ci` runner and test harness.

The original matrix image names remain compatibility keys, not hosted runner
selectors. A closed mapping resolves them to self-hosted labels; unknown keys
fail rather than falling back. Register and verify these labels before running.

## Execution and tools

Owner-triggered jobs and same-repository pull requests are allowed; fork pull
requests must not execute on the private runner fleet. Provision isolated runner
environments without personal credentials or production mounts. Keep release
credentials scoped to the existing release jobs.

Linux builds need Git, curl, Python, C/C++ tools, pkg-config, Rust/rustup and the
packages requested by each workflow. .NET jobs use setup-dotnet with a writable
DOTNET_INSTALL_DIR. FreeBSD jobs need QEMU/KVM. Packaging additionally needs
PowerShell, actionlint, shellcheck, XML/Desktop/AppStream tools and the relevant
package builders; Nix evaluation requires Nix. Keep workflow-pinned tool versions.
Windows needs Git (including Bash), PowerShell 7, .NET SDK 8, Rust/rustup, MSVC
C++ build tools and Windows SDK; installers also require the pinned WiX tools.
macOS requires the matching Xcode/SDK and Rust toolchain.

Do not run hosted-image disk cleanup against a persistent self-hosted machine.
Installing runner software alone is not evidence that application tests passed.
