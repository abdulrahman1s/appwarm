# Appwarm contributor instructions

This repository owns the Rust CLI, Nix flake, NixOS module, and experimental
Niri integration. Keep changes portable across Linux desktops and make hidden
execution staging opt-in.

## Layout

- `src/`: Rust CLI. `trace.rs` learns startup file use; `profile.rs` stores and
  ranks ranges; `warm.rs` requests reclaimable page-cache reads; `stage.rs`
  manages frozen hidden-window processes; `desktop.rs` wraps desktop entries.
- `nix/package.nix`: source package.
- `nix/module.nix`: reusable NixOS integration and user units.
- `nix/niri-appwarm.nix` and `patches/`: pinned experimental Niri build.
- `.github/workflows/build-release.yml`: tests, Nix build, and tagged static
  x86_64 Linux releases.

## Behavioral rules

- Never claim warming makes an already hot app execute faster. It only avoids
  some storage faults. Execution staging pays startup cost before reveal and
  consumes real RAM while staged.
- Keep ordinary learning and warming rootless. Do not use `mlock()` or globally
  drop page cache. Keep warming at idle I/O and low CPU priority; respect RAM
  and per-app budgets.
- Never stage on stock Niri or a visible workspace. Verify the hidden-workspace
  IPC and cgroup rule before launching. Preserve the active window and focus.
- Treat Niri hidden-workspaces as an experimental third-party patch. Pin its
  source revision and test against the selected nixpkgs Niri version.
- Preserve existing user-owned desktop files and application profiles. Write
  private state atomically where possible.
- Separate page-cache warming, hidden staging, and first usable interaction in
  documentation and benchmarks. Do not report IPC reveal time as app usability.

## Validation before publishing

Run `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`,
`cargo test --locked`, `nix flake check --no-build --no-update-lock-file`, and
`nix build .#appwarm --no-update-lock-file`. For Niri or module changes, also
evaluate a NixOS configuration using the module and build the patched Niri
package. Run live Niri checks in an isolated session before promising that
staging works on a new compositor revision.

GitHub release tags use the Cargo version (`vMAJOR.MINOR.PATCH`). Release
assets are static x86_64 Linux binaries. The prebuilt Nix package requires the
release tarball's SRI hash. Keep `Cargo.toml`, `Cargo.lock`, the release URL,
and flake defaults aligned when changing versions. Do not make automated
version-bump commits with a bot author; maintainers control release commits.
