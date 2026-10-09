# Appwarm

Appwarm helps Linux desktop apps feel ready sooner. It has two independent modes:

- **Page-cache warming** learns which files an app reads during startup and requests those pages before the next launch. It works on any Linux desktop. The cache is reclaimable; warming can reduce storage faults but does not run the app or make an already hot app execute faster.
- **Hidden staging** starts an app on a hidden workspace, waits for its window, then freezes the process until you open it. This pays startup cost before reveal and uses real RAM. It requires the experimental patched Niri build and is opt-in.

Both modes run as your desktop user. Appwarm limits I/O and memory use and evicts frozen stages under memory pressure. After a configured app's last window closes, the monitor prepares a fresh stage for its next launch. It also detects apps opened normally before their first stage was ready.

## The macOS analogy

On macOS, opening an app that is already running activates its existing process instead of launching it again ([Apple's Launch Services documentation](https://developer.apple.com/library/archive/documentation/Carbon/Conceptual/LaunchServicesConcepts/LSCConcepts/LSCConcepts.html)). Appwarm's hidden staging aims for a similar *ready-to-show* result on the first requested launch: it starts a selected app in advance, freezes it, then reveals that same process. Page-cache warming is a smaller step: it prepares file data, while the app still starts when you click. Appwarm does not reproduce macOS app lifecycle behavior or eliminate startup work.

## Install on NixOS

Add the flake input and module:

```nix
inputs.appwarm = {
  url = "github:abdulrahman1s/appwarm";
  inputs.nixpkgs.follows = "nixpkgs";
};

# In nixosSystem:
modules = [ appwarm.nixosModules.default ./configuration.nix ];
```

Configure apps in a NixOS module:

```nix
programs.appwarm = {
  enable = true;
  applications = {
    firefox = {
      package = pkgs.firefox;
      stage.enable = true; # Requires the Niri setup below.
    };
    chromium = pkgs.chromium; # Install and warm its learned profile.
  };
  niri.enable = true;
};
```

Application names are profile names. Staging defaults to off; `stage.appId` and `stage.desktopId` default to the name and `<name>.desktop`. Set them when the installed app uses different IDs. You can also set `stage.command`, `stage.arguments`, or `warm = false`. The older `apps` and `stages` options still work.

The module builds Appwarm from source by default. On x86_64 Linux, a [prebuilt release](https://github.com/abdulrahman1s/appwarm/releases) is available if you pass `inputs` to your module:

```nix
# In nixosSystem:
specialArgs = { inherit inputs; };

# In your module:
programs.appwarm.package = inputs.appwarm.lib.mkPrebuiltPackage pkgs {
  version = "0.4.3";
  hash = "sha256-PIiXsXnm2vnyRdK73v9Y4KknGEDVpG9YoII8Y7XULEk=";
};
```

## Learn and warm

Close existing app instances before learning, especially browsers that hand a new launch to an existing process:

```sh
appwarm learn firefox -- firefox
appwarm warm firefox
appwarm status firefox
```

Relearn after app updates or major startup changes. The NixOS module schedules warming after login and periodically afterward. Its `settings` option exposes `window_sec`, `budget_mib`, `min_available_mib`, `max_file_mib`, `stage_budget_mib`, `stage_settle_ms`, and `restage_delay_sec`; see [config.example](config.example) for defaults and user overrides.

For a standalone install, run `cargo install --locked --path .` and install `strace` for `learn`.

## Hidden staging on Niri

Set `programs.appwarm.niri.enable = true`, then add this to your Niri config. Replace `DP-2` with your output and put the window rule after other workspace rules:

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

Restart Niri. Stock Niri lacks the hidden-workspace IPC and cgroup rule; Appwarm refuses to stage without them. The module stages selected apps after login, wraps their desktop launchers to reveal them, and runs a monitor that restages an app after its last window closes. The default delay is five seconds; failed attempts retry after one minute. Closing all windows of an Appwarm-launched app also stops any remaining process in its Appwarm unit before restaging. Manual `evict` does not schedule a replacement. Existing user-owned desktop entries are left alone. A staged app can still perform network activity or send notifications before you reveal it.

For sandboxed apps, set `applications.<name>.package` and `stage.command` to the sandbox wrapper executable, and set `stage.appId` to the actual Wayland app ID. Keep the wrapper's desktop ID in `stage.desktopId`. Appwarm does not add sandbox permissions or bypass the wrapper. Its hidden-window guard also requires the window process to remain in the Appwarm transient unit; wrappers that move it to another cgroup cannot be staged safely.

### Prebuilt patched Niri

On x86_64 Linux, `programs.appwarm.niri.enable = true` uses the [prebuilt patched Niri](https://github.com/abdulrahman1s/appwarm/releases/tag/v0.4.3) by default. The release archive contains only the Niri package output (about 11 MiB); Nix fetches its libraries from a separate pinned nixpkgs input. The main `nixpkgs` input can follow yours as shown above. Set `programs.appwarm.niri.prebuilt = false` to compile Niri locally. A custom `niri.basePackage` or aarch64 Linux also builds from source.

For manual control: `appwarm stage firefox --app-id firefox -- firefox`, `appwarm show firefox`, `appwarm staged`, and `appwarm evict firefox`.
Run `appwarm doctor` in a Niri session to check the patched IPC, hidden workspace, configured desktop entries, and stage commands. The cgroup rule is verified when an app is staged.

## Benchmark

Use the same app version and workload for each run, close existing instances, and repeat several trials. Measure **first usable interaction** separately from window appearance or the launcher command's exit time. A clean reboot provides a cold baseline; avoid globally dropping page cache on your daily session.

On Niri, this script alternates ordinary and warmed launches and reports median **window appearance** time:

```sh
python3 bench/launch.py --match firefox --trials 6 --warm firefox -- firefox
```

Normal trials may benefit from cache left by earlier trials, so record that limitation. For staging on the patched Niri session, measure preparation, reveal command time, and frozen memory:

```sh
python3 bench/stage.py --name firefox --app-id firefox -- firefox
```

Report preparation time and memory alongside reveal time and your separately measured time to first usable interaction. A fast reveal means startup work happened earlier; reveal command time alone does not measure app usability.
