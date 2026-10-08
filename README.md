# appwarm

## Install with Nix flakes

This repository exports `packages.<system>.appwarm`,
`packages.<system>.niri-appwarm`, and `nixosModules.default`. On NixOS:

```nix
inputs.appwarm.url = "github:abdulrahman1s/appwarm";
inputs.appwarm.inputs.nixpkgs.follows = "nixpkgs";

# Add appwarm.nixosModules.default to nixosSystem.modules, then configure:
programs.appwarm = {
  enable = true;
  apps = [ "firefox" "brave" ];
  stages = [
    "firefox|firefox|firefox.desktop|firefox"
    "code|code|code.desktop|code|--new-window"
  ];
  niri.enable = true;
};
```

The module installs the executable and starts delayed warming and staging
timers plus a memory-pressure watcher. Staging requires the patched Niri package,
the KDL below, and a running graphical session; page-cache warming works without
Niri. The `stages` entries describe the installed desktop IDs and real launch
commands on your system. Set `stages = [ ];` to keep only page-cache warming.
The default package builds from this GitHub flake source. For x86_64 Linux,
`lib.mkPrebuiltPackage` can use a GitHub Release binary when passed the release
version and its Nix SRI hash. The statically linked release still needs `strace`
on `PATH` for `learn`; Nix source builds wrap it automatically.

Niri config for hidden staging (change the output name for your displays):

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

Keep that window rule after app-specific workspace rules. Restart Niri after
installing the patched build so the rule exists before staged clients connect.
The patch is pinned to an experimental third-party hidden-workspaces branch of
Niri 26.04. Test updates before replacing a working compositor.

`appwarm` is a Rust Linux desktop startup file profiler and page-cache warmer.
It also has opt-in execution staging for a patched Niri compositor. A small
user service watches memory pressure when execution staging is enabled.
A deliberate `learn` run launches an application under
rootless `strace` for a short window; later `warm` runs request only the file
ranges seen during learning. The kernel remains free to reclaim every page.

## Design

`strace -f` observes `read`, `pread64`, `readv`, `splice`, `mmap`, `execve`, and related calls in the
launched process tree. At the end of learning, `/proc/<pid>/maps` and
`/proc/<pid>/pagemap` identify which file-backed mapped pages are actually
present in the live application processes. The latter receive the highest
priority; executable launches, ordinary reads, and broad mappings follow.
This captures shared libraries, ELF executables, fonts, browser/Electron
resources, and ordinary file reads. Ranges are rounded to 64 KiB and given
one hit per learning run. Repeating `learn` increases the rank of consistently
used ranges; ties use access order rather than alphabetic path order. Volatile
browser HTTP caches, journals, and session files are omitted.
When a profile has enough confirmed mapped pages, broad `mmap` observations
are retained as a fallback record but excluded from normal warming.
Each file is limited to 16 MiB by default. An individual observed call is
limited to 2 MiB. The complete trace is capped at 64 MiB.

`warm` checks the launcher symlink target and its file identity, plus device,
inode, size, and nanosecond modification time for each recorded file. A new
Nix store target invalidates the whole profile, even while the old target
still exists. Changed or missing resource files are skipped and reported as stale. It
uses `mmap(PROT_NONE)` plus `mincore()` to avoid requesting resident pages, then
uses `posix_fadvise(POSIX_FADV_WILLNEED)` on missing ranges. Adjacent ranges
are batched into requests of up to 2 MiB. The call is a
nonblocking kernel hint; it may read less than requested under memory pressure.
The process sets nice 19 and `IOPRIO_CLASS_IDLE`, paces requests by 5 ms, and
stops when `MemAvailable` falls below the larger of 10% of RAM and the
configured minimum. The total requested bytes are capped by `budget_mib`.

Profiles are private, atomically replaced text files under
`${XDG_CACHE_HOME:-$HOME/.cache}/appwarm/`. Paths are hex encoded in the file.
They are local metadata; avoid sharing profiles if paths are sensitive.
The optional selection and budget config is
`${XDG_CONFIG_HOME:-$HOME/.config}/appwarm/config`.

## Hidden execution staging on Niri

Page-cache warming cannot make an already hot application execute startup code
faster. `stage` instead launches an app on a **genuinely hidden** Niri workspace,
waits for its first window and a configurable settling period, and freezes its
systemd user unit. `show` thaws that unit, moves the **same native Wayland window**
to the focused workspace, and focuses it. There is no Weston renderer, video
stream, or clipboard proxy in this path. The app keeps its native Niri clipboard,
input, portals, window rules, and output scaling. Its app ID is unchanged.

The pinned Niri 26.04 hidden-workspaces patch lives in
`nix/niri-appwarm.nix`, with a small local cgroup-matching rule patch
in `patches/niri-appwarm-cgroup.patch`. Stock Niri does not expose a hidden workspace;
`stage` checks for the patch's `workspaces-with-hidden` IPC before launching
anything and refuses to risk a visible launch. The patch is a third-party draft,
so test the Niri build and this path before depending on it for daily work.
The normal Niri config defines `workspace "appwarm-hidden" { hidden true }`
and a permanent cgroup window rule for `appwarm-stage-*` user units. The Niri
patch also ignores activation requests from windows still on hidden workspaces.
The rule
must already be loaded before a client connects: reloading it at launch time
races the first window. Matching the cgroup before the app sets its app ID
avoids a visible first window. Appwarm never rewrites the Niri config at runtime.

```sh
# Close existing instances first. Use the real app_id from `niri msg --json windows`.
appwarm stage brave-origin --app-id brave-origin -- brave --ozone-platform=wayland
appwarm show brave-origin
appwarm stage dev.zed.Zed --app-id dev.zed.Zed -- zeditor --foreground --new
appwarm launch dev.zed.Zed -- zeditor
appwarm staged
appwarm evict dev.zed.Zed
```

`launch NAME -- COMMAND` shows a staged app or executes `COMMAND` normally.
This can be used in desktop entries and shortcuts for any app. The local
`niri-launch-or-focus` script also checks for a stage whose name equals the
app ID, so the existing Brave and Firefox shortcuts reveal staged windows.
Execution staging is configured per app. The user timer starts selected apps two
minutes after login, provided they are not already open and the memory guard
allows it. A desktop entry with the same ID as the installed application routes
ordinary launcher clicks through `appwarm desktop`; it reveals a staged window
or starts the original command normally. Main desktop actions keep their original
commands. Existing user-owned desktop entries are left untouched.
Repeated staging consumes real RAM and
may cause network activity or notifications before reveal. A frozen process
uses no CPU while frozen, but its memory is not page cache and is not freely
reclaimable. `stage_budget_mib` caps systemd memory use, and staging refuses
when `MemAvailable` is too low. A lightweight user monitor checks memory PSI
and `MemAvailable` every two seconds. Under sustained pressure or below the
memory guard, it stops the largest frozen unit first. Once stopped, the next
launch starts normally; it is not restaged until the next delayed staging run.
`stage_settle_ms` controls how long the app
continues initializing after its first window appears before freezing.

This path is rootless. Freezing uses the user systemd manager's cgroup freezer.
The window process must remain in the staging unit; an app delegated to an
already running instance or another D-Bus unit is rejected. A staged app may
still need time after thaw to finish work that depends on interaction, network,
or a frame callback. Its memory and GPU allocations remain resident while
staged. The patched compositor has not been activated by a Nix build alone.
Staging is limited to native Wayland windows. The transient unit removes
`DISPLAY` so X11-only clients cannot bypass the Niri rule and appear visibly.
For Chromium/Electron launchers, select their Wayland backend explicitly where
needed (for example `--ozone-platform=wayland`). A launcher that delegates to
an existing browser process may still open a window outside the staged unit;
close that instance first. The X11 restriction applies only to `stage`, not
to ordinary `learn` or `warm`.

### Limits

- `learn` deliberately slows that one launch because ptrace stops traced
  threads at file syscalls. Benchmark an ordinary launch, never the learning run.
- A pagemap snapshot sees pages present in processes still alive at the end of
  learning; short-lived workers may be missed. If pagemap access is denied,
  syscall observations remain as a fallback. Broad `mmap` ranges still
  overestimate useful pages, so they rank below confirmed resident pages.
- Present pages prove process residency at the sample time, not that the page
  was responsible for launch latency. Repeated learn runs help stabilize rank.
- Sequential `read` offsets are reconstructed from `openat` and `lseek` per
  process and descriptor. Shared offsets after forks and unusual descriptor
  duplication can make some offsets approximate. `pread64` offsets are exact.
- An app that forwards to an already running instance may only train its tiny
  launcher. Start a fresh instance for learning. Sandboxes can prohibit ptrace
  of their child processes, so the wrapper may be all that is visible.
- `mincore` is a point-in-time check. The kernel can reclaim pages immediately
  afterward. There is no promise of launch latency improvement on every run.
- Tracing does not catch kernel-side accesses that do not pass through the
  observed syscalls. `io_uring` and unusual mmap behavior may be
  underrepresented. `/proc/<pid>/maps` alone would miss ordinary resource reads.

## Why this tracer

Unprivileged fanotify cannot mark an entire mount/filesystem or report the PID
of another process, making it unsuitable for attributing all startup accesses
to an app. Inotify is directory based and nonrecursive. eBPF/perf tracing can
be more precise, but commonly needs capabilities or permissive tracing sysctls
and a larger compatibility surface. `strace` can trace a process it launches
without root on mainstream systems; Yama's usual parent/child restriction
allows this arrangement. A system configured to ban unprivileged ptrace still
cannot use rootless `learn`. `warm`, `status`, `list`, and `forget` remain rootless.

## Source tree and build

```text
local-packages/appwarm/
  Cargo.toml      Rust package; serde_json for Niri IPC
  Cargo.lock      locked build
  src/main.rs     CLI dispatch
  src/config.rs   configuration, XDG paths, app names
  src/profile.rs  bounded, ranked file ranges and profile storage
  src/trace.rs    rootless launch tracing and trace parsing
  src/pagemap.rs  rootless per-process mapped-page sampling
  src/linux.rs    small Linux syscall boundary and memory guard
  src/warm.rs     page residency checks and paced cache warming
  src/stage.rs    hidden Niri execution, cgroup freeze/thaw, reveal
  src/desktop.rs  desktop-entry integration for configured staged apps
  bench/launch.py repeatable Niri window-appearance benchmark
  bench/stage.py  hidden-stage and reveal smoke/latency probe
  config.example  user-selected apps and budgets
  appwarm.service systemd user service
  appwarm.timer   delayed user timer
  README.md       this file
local-packages/appwarm.nix  Nix derivation
services/appwarm.nix        NixOS user package and units
nix-packages/niri-appwarm.nix  pinned hidden-workspace Niri build
nix-packages/niri-appwarm-cgroup.patch  pre-map cgroup window rule
config/niri/{config,rules}.kdl      permanent hidden workspace and stage rule
system/impermanence.nix    profile/config persistence on this machine
```

On another distribution, install Rust/Cargo and `strace`:

```sh
cargo build --release --manifest-path local-packages/appwarm/Cargo.toml
install -Dm755 local-packages/appwarm/target/release/appwarm "$HOME/.local/bin/appwarm"
mkdir -p "$HOME/.config/appwarm" "$HOME/.config/systemd/user"
cp local-packages/appwarm/config.example "$HOME/.config/appwarm/config"
cp local-packages/appwarm/appwarm.service local-packages/appwarm/appwarm.timer "$HOME/.config/systemd/user/"
systemctl --user daemon-reload
systemctl --user enable --now appwarm.timer
```

The standalone build's ordinary profiling/warming commands work with stock
compositors. Execution staging requires a Niri build with the compatible
hidden-workspace IPC patch and systemd user services.

The standalone service expects `%h/.local/bin/appwarm`. Set `apps=` in the
config to only the profiles you actually want warmed. The timer starts with
the user manager's `default.target`, fires two minutes later, and repeats every
two hours. It is safe to leave disabled and call `warm` manually. On this
NixOS flake, the service defaults to `zeditor,brave` when the user has no
`~/.config/appwarm/config`; an explicit `apps=` setting overrides the default.
The 256 MiB `warm-all` budget is shared across apps so an early app cannot
consume all of it. The service/timer and binary are installed by
`services/appwarm.nix`; activation is separate from building the flake. This
host persists both appwarm directories across its home
rollback; on ordinary distributions their durability follows `$HOME`.

On this NixOS machine, Zed and Brave are the default execution staging choices.
The same configuration works for any native Wayland app whose first window stays
inside the transient systemd unit. Set repeated `stage=` lines in
`~/.config/appwarm/config` to select other apps; each entry is
`stage=NAME|APP_ID|DESKTOP_ID.desktop|COMMAND|ARG...`. This replaces the defaults.
Set `stages=none` to disable automatic execution staging. The desktop sync user
service copies each selected system desktop entry into the persisted
`~/.local/share/applications` directory with only its main `Exec` wrapped.
It updates only files with its own marker, preserving user overrides. The
delayed stage timer and monitor are independent of the page-cache timer.
`appwarm desktop-sync` refreshes the entries after editing the selection.

## Commands and examples

Flags go before the command:

```sh
appwarm --window 10 learn firefox -- firefox --no-remote
appwarm --window 10 learn chromium -- chromium --user-data-dir="$HOME/.cache/appwarm-chromium-learn" --new-window
appwarm --window 15 learn code -- code --new-window
appwarm --window 15 learn discord -- discord
appwarm --window 10 learn zeditor -- zeditor --foreground --new
appwarm --window 10 learn brave -- brave --incognito about:blank

appwarm --budget-mib 128 warm firefox
appwarm status firefox
appwarm list
appwarm forget firefox
appwarm --debug warm-all

# Native hidden execution (patched Niri only, never started by warm-all):
appwarm stage firefox --app-id firefox -- firefox --no-remote
appwarm stage chromium --app-id chromium -- chromium --ozone-platform=wayland --new-window
appwarm stage code --app-id code -- code --ozone-platform=wayland --new-window
appwarm stage discord --app-id discord -- discord --ozone-platform=wayland
appwarm show firefox
appwarm launch code -- code --new-window
```

The command after `--` can be any launcher and arguments; omit it when its name
matches the profile name (`appwarm learn firefox`). Keep the same kind of user
profile and startup workload when learning and measuring. Close existing app
instances first, especially browsers and Electron applications that delegate
to an existing process. If `code` or `discord` is a sandbox wrapper, inspect
the learned file list with `status` to confirm that files inside the actual app
were seen. Adjust the app command to your distribution's launcher names.
Profiles created by appwarm 0.1 are kept on disk but not warmed by 0.2; relearn
each app once after upgrading so the new resident-page ranking is available.

`config.example` uses all four names as an illustration. Use fewer names on
systems with limited RAM or where an app is rarely launched. The budget is
shared across all apps in `warm-all` in config order; each individual `warm`
gets its own budget. Relearning after a package update replaces a stale launcher
profile or refreshes changed resource paths.

## Measuring launch performance

Measure the point at which the app is usable, not merely when its launcher
process exits. For browsers, time a dedicated DevTools endpoint or an
interactive page; for other GUI apps, use a repeatable compositor or
accessibility signal. `bench/launch.py` alternates ordinary and warmed launches
and measures Niri window appearance while terminating only the process group
it launched:

```sh
python3 bench/launch.py --match zed --trials 10 --warm zeditor -- zeditor --foreground --new
python3 bench/launch.py --match brave-origin --trials 10 --warm brave -- brave --incognito about:blank
```

Close existing app instances before running these commands. Window appearance
is only a proxy for usability; the benchmark does not measure the first
rendered frame or input responsiveness. Use at least 10 runs per condition,
alternate condition order, and report medians and variance.

Use three conditions with the same app version, profile, and background load:

1. **Cold:** after a clean reboot or in a disposable VM with a fresh page
   cache, before appwarm runs.
2. **Normal:** ordinary later launches without invoking appwarm.
3. **Warmed:** after the same baseline, run `appwarm warm APP`, wait for its
   queued reads to settle, then launch.

For execution staging, report both **stage-to-first-usable-window** and
**show-to-first-usable-interaction**, plus the staged unit's `MemoryCurrent`
and CPU time. Compare an ordinary launch with a staged reveal from the same
profile and app version. Revealing a fully started app is expected to be much
faster than launching it; the startup cost was paid earlier, so include the
background cost in the result. Measure a real first interaction, not merely
the return time of `appwarm show` or an IPC window event. Check for any flash
or focus change during staging, and for clipboard, portals, scaling, and
popups after reveal on both monitors.

After activating the patched Niri build, `bench/stage.py` automates the
no-focus, no-visible-workspace, frozen-unit, and same-window-reveal checks.
Its `reveal_command_ms` covers the thaw and Niri IPC actions, not the first
usable frame or click:

```sh
python3 bench/stage.py --name code --app-id code -- code --new-window
```

Also record `major-faults`, `minor-faults`, and elapsed time with `perf stat`
when the system permits unprivileged perf events. `time -v` gives major/minor
fault counts without perf permission. Include memory pressure and storage type
in results. Do not compare a traced `learn` launch to untraced runs. Cold-cache
measurements on a daily driver are best done across reboots; global
`drop_caches` affects unrelated workloads and requires root. No root privilege
is needed for ordinary learning, warming, or the user timer.

Kernel API references: [fanotify_init](https://man7.org/linux/man-pages/man2/fanotify_init.2.html),
[strace](https://man7.org/linux/man-pages/man1/strace.1.html),
[posix_fadvise](https://man7.org/linux/man-pages/man2/posix_fadvise.2.html),
[mincore](https://man7.org/linux/man-pages/man2/mincore.2.html), and
[ioprio_set](https://man7.org/linux/man-pages/man2/ioprio_set.2.html), plus
[kernel pagemap documentation](https://www.kernel.org/doc/html/latest/admin-guide/mm/pagemap.html).
