# Appwarm

Appwarm makes repeat Linux desktop launches faster in two ways:

1. **Page-cache warming** learns the files and file ranges an app uses at startup, then asks Linux to load those ranges before the next launch.
2. **Hidden execution staging** starts a selected app on a hidden Niri workspace, freezes it after its first window is ready, and reveals that same window when you launch it normally.

Page-cache warming uses reclaimable kernel cache and works on any Linux desktop. Hidden staging gives the larger launch-time improvement when storage is already fast, but it uses RAM and requires the patched Niri build included here. Appwarm watches memory pressure and evicts frozen apps when RAM is needed.

The CLI, NixOS module, Niri patch, and GitHub release workflow live in this repository. Appwarm is written in Rust. Ordinary learning, warming, and staging run as the desktop user; root is not required.

## Install

### NixOS flake

Add the input and module to your flake:

```nix
inputs.appwarm = {
  url = "github:abdulrahman1s/appwarm";
  inputs.nixpkgs.follows = "nixpkgs";
};

# In nixosSystem.modules:
modules = [
  appwarm.nixosModules.default
  ./configuration.nix
];

# Pass flake inputs to your configuration modules if they use the
# prebuilt-package example below.
specialArgs = { inherit inputs; };
```

Configure your apps in a NixOS module:

```nix
programs.appwarm = {
  enable = true;
  applications = {
    firefox = pkgs.firefox;
    code = {
      package = pkgs.vscode;
      stage.enable = true;
      stage.appId = "code";
      stage.desktopId = "code.desktop";
      stage.arguments = [ "--new-window" ];
    };
  };
  settings.stage_budget_mib = 1536;
  niri.enable = true;
};
```

The `applications` attribute names are learned profile names. A package value installs the package and includes its profile in page-cache warming. A detailed value can set `warm = false`, enable hidden staging, and specify `stage.appId`, `stage.desktopId`, `stage.command`, and `stage.arguments`. Staging is opt-in. The command defaults to the package's main executable, while the app ID and desktop ID default to the attribute name and `<name>.desktop`; set them explicitly when the package uses different values. Learn a profile before warming it, for example `appwarm learn firefox -- firefox`.

The module installs appwarm and configured application packages, starts delayed page-cache and staging timers, syncs selected desktop launchers, and runs a small memory-pressure watcher. A configured launcher reveals a staged window when one exists and otherwise launches normally. Existing user-owned desktop entries are left alone.

For other apps, adapt the IDs and arguments to their installed desktop entries:

```nix
programs.appwarm.applications.chromium = {
  package = pkgs.chromium;
  stage.enable = true;
  stage.arguments = [ "--ozone-platform=wayland" ];
};
```

The existing `apps = [ "firefox" ];` and `stages = [ "firefox|firefox|firefox.desktop|firefox" ];` options still work and are combined with `applications`. `settings` exposes `window_sec`, `budget_mib`, `min_available_mib`, `max_file_mib`, `stage_budget_mib`, and `stage_settle_ms`. Values in the user's `~/.config/appwarm/config` override these module defaults.

The default package builds from this flake's source. On x86_64 Linux you can instead use the static GitHub Release binary:

```nix
programs.appwarm.package = inputs.appwarm.lib.mkPrebuiltPackage pkgs {
  version = "0.4.1";
  hash = "sha256-3zS9o9xKfQII1H/1YHEV+3hLzV+pSB73NsYVmRrVg0M=";
};
```

The NixOS module installs `strace` for the prebuilt binary's `learn` command. Source builds include it in appwarm's wrapper. `nix run github:abdulrahman1s/appwarm -- --help` runs the CLI without installing the module.

### Cargo

```sh
cargo install --locked --path .
```

Install `strace` from your distribution for `learn`. The standalone binary calls `systemctl --user` and `niri` from `PATH` when staging is used.

## Niri setup for hidden staging

The flake exports `packages.<system>.niri-appwarm` and the NixOS module can install it with `programs.appwarm.niri.enable = true`. The package pins an experimental Niri 26.04 hidden-workspaces branch and adds a cgroup window rule. Stock Niri cannot safely hide staged apps, and appwarm refuses to stage without the required IPC support.

Add this to your Niri config before starting a staged app. Change `DP-2` to one of your outputs and keep the window rule after other workspace rules:

```kdl
workspace "appwarm-hidden" {
    open-on-output "DP-2"
    hidden true
}

window-rule {
    match cgroup=r#"/appwarm-stage-[A-Za-z0-9_.-]+\.service(?:\n|$)"#
    open-on-workspace "appwarm-hidden"
    open-focused false
}
```

Restart Niri after installing the patched build. A staged app remains a native Wayland client of Niri, so its window uses the normal clipboard, input, portals, app ID, and output scaling. Appwarm freezes its systemd user unit while it waits; frozen apps use no CPU but still occupy memory. This is an experimental compositor patch, so test it before using it as your only daily-driver session.

## Commands

```sh
appwarm learn firefox -- firefox
appwarm warm firefox
appwarm status firefox
appwarm forget firefox
appwarm list

appwarm stage code --app-id code -- code --new-window
appwarm show code
appwarm staged
appwarm evict code
```

`learn` launches the app under rootless `strace` for the configured startup window, 10 seconds by default. It records startup reads and samples resident file-backed mapped pages from `/proc`. Repeat it after an application update or when your startup workload changes. For browsers and Electron apps, close existing instances first; a launcher that hands off to an existing process will not give a useful profile.

`warm` checks which learned pages are already resident, requests only missing ranges with `POSIX_FADV_WILLNEED`, and returns after the requests are queued. Linux may reclaim those pages whenever it needs RAM. The process runs at nice 19 and idle I/O priority, with a per-run I/O budget and a `MemAvailable` guard. Changed files and stale launcher targets are skipped until relearned.

`stage` launches on the hidden workspace, waits for a window and settling period, then freezes the app's user unit. `show` thaws and moves that window into the current workspace. The background startup cost still happens; reveal is quick because the app has already run most of its startup work. Some apps may contact the network, send notifications, or perform updates before reveal.

## Configuration

Copy [config.example](config.example) to `${XDG_CONFIG_HOME:-$HOME/.config}/appwarm/config`. Profiles are stored privately under `${XDG_CACHE_HOME:-$HOME/.cache}/appwarm/`. Runtime stage state is under `$XDG_RUNTIME_DIR/appwarm/` and is cleared on logout or reboot.

Key settings:

| Key | Default | Purpose |
| --- | ---: | --- |
| `window_sec` | 10 | Learning window in seconds |
| `budget_mib` | 256 | Maximum page-cache requests per `warm` |
| `min_available_mib` | 1024 | Stop warming or staging below this available RAM |
| `max_file_mib` | 16 | Maximum learned range per file |
| `stage_budget_mib` | 2048 | Maximum total memory for frozen staged apps |
| `stage_settle_ms` | 2000 | Wait after the first staged window appears |

The module's `applications`, `apps`, `stages`, and `settings` options set defaults; entries in the user config override them. `stages=none` disables staging from the user config. Appwarm also stops the largest frozen stage if available RAM falls below the guard or memory pressure rises. A later launch then follows the ordinary path. The monitor never evicts an app that has already been revealed.

## How learning works

Appwarm traces a launched process tree's file reads, mappings, and executions. It uses `/proc/<pid>/maps` and `/proc/<pid>/pagemap` to rank file-backed pages actually resident during startup above broad mappings. Profiles store bounded, ranked file ranges rather than whole directories. Repeated learning raises the rank of consistently used ranges. Volatile browser HTTP caches and session files are excluded.

Warming checks file identity, size, and modification time. A changed Nix launcher target invalidates the profile even if the old target still exists. It uses `mincore()` to avoid requesting already resident pages when possible, batches nearby ranges, and paces requests. Neither warming nor staging locks pages with `mlock()`.

## Measuring performance

Run the same app version, profile, and background workload under three conditions: cold after a clean reboot, ordinary later launch, and ordinary launch after `appwarm warm NAME`. For each condition, measure first usable interaction and report multiple trials with median and spread. Do not time only the launcher process exit.

For staging, report both time spent preparing the hidden app and time from reveal to first usable interaction. Include its frozen `MemoryCurrent`. A staged reveal moves startup work earlier; it does not remove that work. [bench/launch.py](bench/launch.py) measures Niri window appearance for ordinary and warmed launches, and [bench/stage.py](bench/stage.py) checks that staging does not steal focus and that the same window is revealed. Window appearance and reveal command time are proxies, not first usable interaction. Avoid global `drop_caches` on a daily-driver machine.

## Source layout

| Path | Role |
| --- | --- |
| `src/trace.rs`, `src/profile.rs`, `src/pagemap.rs` | Learn and rank the startup working set |
| `src/warm.rs`, `src/linux.rs` | Request reclaimable page-cache reads with resource guards |
| `src/stage.rs`, `src/desktop.rs` | Hidden staging and normal launcher integration |
| `nix/package.nix`, `nix/module.nix` | Nix package and NixOS user services |
| `nix/niri-appwarm.nix`, `patches/` | Pinned Niri build and cgroup patch |
| `.github/workflows/build-release.yml` | Rust checks, Nix build, tagged release |

The repository also includes standalone `appwarm.service` and `appwarm.timer` examples for non-NixOS systemd user sessions.
