//! Opt-in execution warming on a niri build with hidden workspaces.
//! The app remains a native client of niri; no pixels or input are proxied.

use crate::config::{valid_name, Config, Paths};
use crate::desktop;
use crate::linux::memory_headroom;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const HIDDEN_WORKSPACE: &str = "appwarm-hidden";
const LOCK_EX: i32 = 2;

unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

struct Runtime {
    dir: PathBuf,
    _lock: File,
}

#[derive(Clone)]
struct Staged {
    name: String,
    app_id: String,
    unit: String,
    window_id: u64,
}

struct StageEntry {
    name: String,
    app_id: String,
    desktop_id: String,
    command: Vec<String>,
}

fn configured_stages(cfg: &Config) -> Vec<StageEntry> {
    cfg.stages
        .iter()
        .filter_map(|entry| {
            let fields: Vec<_> = entry.split('|').collect();
            if fields.len() < 4
                || !valid_name(fields[0])
                || !valid_app_id(fields[1])
                || !fields[2].ends_with(".desktop")
                || !valid_name(fields[2])
                || fields[3..].iter().any(|part| part.is_empty())
            {
                eprintln!("appwarm: ignoring invalid stage entry: {entry}");
                return None;
            }
            Some(StageEntry {
                name: fields[0].to_owned(),
                app_id: fields[1].to_owned(),
                desktop_id: fields[2].to_owned(),
                command: fields[3..].iter().map(|part| (*part).to_owned()).collect(),
            })
        })
        .collect()
}

fn runtime() -> io::Result<Runtime> {
    let base = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| io::Error::other("XDG_RUNTIME_DIR must be an absolute path"))?;
    let dir = base.join("appwarm");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(dir.join("lock"))?;
    if unsafe { flock(lock.as_raw_fd(), LOCK_EX) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Runtime { dir, _lock: lock })
}

fn state_path(rt: &Runtime, name: &str) -> PathBuf {
    rt.dir.join(format!("{name}.stage"))
}

fn active_path(rt: &Runtime, name: &str) -> PathBuf {
    rt.dir.join(format!("{name}.active"))
}

fn save(rt: &Runtime, stage: &Staged) -> io::Result<()> {
    let path = state_path(rt, &stage.name);
    let temp = path.with_extension("stage.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)?;
    writeln!(
        file,
        "{}\n{}\n{}",
        stage.unit, stage.window_id, stage.app_id
    )?;
    file.sync_all()?;
    fs::rename(temp, path)
}

fn read_path(path: PathBuf, name: &str) -> io::Result<Staged> {
    let text = fs::read_to_string(path)?;
    let mut lines = text.lines();
    let unit = lines
        .next()
        .ok_or_else(|| io::Error::other("invalid stage state"))?;
    let window_id = lines
        .next()
        .and_then(|line| line.parse::<u64>().ok())
        .ok_or_else(|| io::Error::other("invalid stage window ID"))?;
    let app_id = lines
        .next()
        .ok_or_else(|| io::Error::other("invalid stage app ID"))?;
    let prefix = format!("appwarm-stage-{name}");
    let valid_unit = unit
        .strip_prefix(&prefix)
        .and_then(|suffix| suffix.strip_suffix(".service"))
        .is_some_and(|suffix| {
            suffix.is_empty()
                || suffix.strip_prefix('-').is_some_and(|digits| {
                    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
                })
        });
    if !valid_unit || !valid_app_id(app_id) {
        return Err(io::Error::other("invalid stage state"));
    }
    Ok(Staged {
        name: name.to_owned(),
        app_id: app_id.to_owned(),
        unit: unit.to_owned(),
        window_id,
    })
}

fn read(rt: &Runtime, name: &str) -> io::Result<Staged> {
    read_path(state_path(rt, name), name)
}

fn stages(rt: &Runtime) -> io::Result<Vec<Staged>> {
    let mut result = Vec::new();
    for entry in fs::read_dir(&rt.dir)? {
        let entry = entry?;
        let file = entry.file_name();
        let Some(name) = file.to_str().and_then(|s| s.strip_suffix(".stage")) else {
            continue;
        };
        if valid_name(name) {
            result.push(read(rt, name)?);
        }
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

fn valid_app_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_'))
}

fn run(program: &str, args: &[&str]) -> io::Result<Output> {
    let out = Command::new(program).args(args).output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "{} {}: {}",
            program,
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out)
}

fn niri(args: &[&str]) -> io::Result<Value> {
    let out = run("niri", args)?;
    serde_json::from_slice(&out.stdout).map_err(io::Error::other)
}

fn hidden_workspace_ids() -> io::Result<Vec<u64>> {
    let workspaces = niri(&["msg", "--json", "workspaces-with-hidden"]).map_err(|_| {
        io::Error::other(
            "niri needs the appwarm hidden-workspaces patch; refusing to stage visibly",
        )
    })?;
    let ids: Vec<_> = workspaces
        .as_array()
        .into_iter()
        .flatten()
        .filter(|ws| {
            ws["name"].as_str() == Some(HIDDEN_WORKSPACE) && ws["is_hidden"].as_bool() == Some(true)
        })
        .filter_map(|ws| ws["id"].as_u64())
        .collect();
    if ids.is_empty() {
        Err(io::Error::other(
            "appwarm hidden workspace is not configured; reload niri config",
        ))
    } else {
        Ok(ids)
    }
}

fn window_list() -> io::Result<Vec<Value>> {
    niri(&["msg", "--json", "windows"])?
        .as_array()
        .cloned()
        .ok_or_else(|| io::Error::other("invalid niri windows response"))
}

fn unit_memory(unit: &str) -> io::Result<u64> {
    let out = run(
        "systemctl",
        &["--user", "show", "-p", "MemoryCurrent", "--value", unit],
    )?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .map_err(|_| io::Error::other("unit memory accounting is unavailable"))
}

fn unit_active(unit: &str) -> bool {
    run("systemctl", &["--user", "is-active", unit]).is_ok()
}

fn window_in_unit(window: &Value, unit: &str) -> io::Result<bool> {
    let Some(pid) = window["pid"].as_u64() else {
        return Ok(false);
    };
    Ok(fs::read_to_string(format!("/proc/{pid}/cgroup"))?.contains(unit))
}

pub fn stage(
    name: &str,
    app_id: &str,
    command: &[String],
    _paths: &Paths,
    cfg: &Config,
    debug: bool,
) -> io::Result<()> {
    if !valid_app_id(app_id) {
        return Err(io::Error::other(
            "app ID must contain only ASCII letters, digits, '.', '-' or '_'",
        ));
    }
    let rt = runtime()?;
    if state_path(&rt, name).exists() {
        return Err(io::Error::other(
            "app is already staged; use show or evict first",
        ));
    }
    let budget = cfg.stage_budget_mib * 1024 * 1024;
    let used = stages(&rt)?.iter().try_fold(0_u64, |sum, stage| {
        Ok::<_, io::Error>(sum.saturating_add(unit_memory(&stage.unit)?))
    })?;
    if used >= budget || memory_headroom(cfg)? < budget.saturating_sub(used) {
        return Err(io::Error::other(
            "not enough free memory for the configured stage budget",
        ));
    }
    let baseline = window_list()?;
    let focused_before = baseline
        .iter()
        .find(|window| window["is_focused"].as_bool() == Some(true))
        .and_then(|window| window["id"].as_u64());
    if baseline
        .iter()
        .any(|win| win["app_id"].as_str() == Some(app_id))
    {
        return Err(io::Error::other(
            "an app window with this ID is already open",
        ));
    }

    // The permanent compositor rule must be loaded before any app connects.
    // A per-launch config reload races the first mapped Wayland surface.
    let hidden_ids = hidden_workspace_ids()?;
    let active_file = active_path(&rt, name);
    if active_file.exists() {
        let active = read_path(active_file, name)?;
        if baseline
            .iter()
            .any(|window| window_in_unit(window, &active.unit).unwrap_or(false))
        {
            return Err(io::Error::other("an earlier app window is still open"));
        }
        if unit_active(&active.unit) {
            run("systemctl", &["--user", "stop", &active.unit])?;
        }
    }
    // Revealed applications can keep a background process after their last
    // window closes. A new stage must not reuse that process's transient unit.
    let generation = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_millis();
    let unit = format!("appwarm-stage-{name}-{generation}.service");

    let result = (|| -> io::Result<Staged> {
        let mut cmd = Command::new("systemd-run");
        cmd.arg("--user")
            .arg("--collect")
            .arg("--quiet")
            .arg("--same-dir")
            .arg(format!("--unit={unit}"))
            .arg("--property=Type=exec")
            .arg("--property=CPUWeight=10")
            .arg("--property=IOWeight=10")
            .arg("--property=MemoryAccounting=yes")
            .arg("--property=TimeoutStopSec=3s")
            // Xwayland surfaces belong to the Xwayland client, not to the
            // application cgroup. Deny X11 so a staged app cannot bypass the
            // pre-map Niri rule and flash onto the visible workspace.
            .arg("--property=UnsetEnvironment=DISPLAY")
            .arg(format!("--property=MemoryMax={budget}"));
        for key in [
            "WAYLAND_DISPLAY",
            "XDG_RUNTIME_DIR",
            "DBUS_SESSION_BUS_ADDRESS",
            "XDG_CURRENT_DESKTOP",
            "XDG_SESSION_TYPE",
            "NIXOS_OZONE_WL",
            "ELECTRON_OZONE_PLATFORM_HINT",
            "NIRI_SOCKET",
        ] {
            if let Ok(value) = env::var(key) {
                cmd.arg(format!("--setenv={key}={value}"));
            }
        }
        let output = cmd.arg("--").args(command).output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "systemd-run failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }

        let baseline_ids: Vec<_> = baseline.iter().filter_map(|w| w["id"].as_u64()).collect();
        let deadline = Instant::now() + Duration::from_secs(30);
        let window = loop {
            if !unit_active(&unit) {
                return Err(io::Error::other(
                    "stage unit exited before a window appeared",
                ));
            }
            let windows = window_list()?;
            for window in &windows {
                if window["id"]
                    .as_u64()
                    .is_some_and(|id| !baseline_ids.contains(&id))
                    && !window["workspace_id"]
                        .as_u64()
                        .is_some_and(|id| hidden_ids.contains(&id))
                    && window_in_unit(window, &unit).unwrap_or(false)
                {
                    return Err(io::Error::other(
                        "staged app opened a visible window; only native Wayland clients are supported",
                    ));
                }
            }
            if let Some(window) = windows.into_iter().find(|w| {
                w["app_id"].as_str() == Some(app_id)
                    && w["workspace_id"]
                        .as_u64()
                        .is_some_and(|id| hidden_ids.contains(&id))
                    && w["id"]
                        .as_u64()
                        .is_some_and(|id| !baseline_ids.contains(&id))
            }) {
                break window;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other(
                    "no hidden app window appeared within 30 seconds",
                ));
            }
            thread::sleep(Duration::from_millis(100));
        };
        if !window_in_unit(&window, &unit)? {
            return Err(io::Error::other(
                "window process escaped its stage unit; cannot freeze it safely",
            ));
        }
        thread::sleep(Duration::from_millis(cfg.stage_settle_ms));
        let settled_windows = window_list()?;
        for candidate in &settled_windows {
            if candidate["id"]
                .as_u64()
                .is_some_and(|id| !baseline_ids.contains(&id))
                && window_in_unit(candidate, &unit).unwrap_or(false)
                && !candidate["workspace_id"]
                    .as_u64()
                    .is_some_and(|id| hidden_ids.contains(&id))
            {
                return Err(io::Error::other(
                    "staged app opened a visible window after its first hidden window",
                ));
            }
        }
        let focused_after = settled_windows
            .iter()
            .find(|candidate| candidate["is_focused"].as_bool() == Some(true))
            .and_then(|candidate| candidate["id"].as_u64());
        if focused_after != focused_before {
            if focused_after.is_none() {
                if let Some(id) = focused_before {
                    let _ = run(
                        "niri",
                        &["msg", "action", "focus-window", "--id", &id.to_string()],
                    );
                }
            }
            return Err(io::Error::other(
                "focus changed while staging; refusing to keep the app hidden",
            ));
        }
        if !unit_active(&unit) || unit_memory(&unit)? + used > budget || memory_headroom(cfg)? == 0
        {
            return Err(io::Error::other(
                "staged app exited or exceeded the memory guard",
            ));
        }
        run("systemctl", &["--user", "freeze", &unit])?;
        Ok(Staged {
            name: name.to_owned(),
            app_id: app_id.to_owned(),
            unit: unit.clone(),
            window_id: window["id"].as_u64().unwrap(),
        })
    })();

    match result {
        Ok(staged) => {
            if let Err(error) = save(&rt, &staged) {
                let _ = run("systemctl", &["--user", "thaw", &unit]);
                let _ = run("systemctl", &["--user", "stop", &unit]);
                return Err(error);
            }
            if active_path(&rt, name).exists() {
                if let Err(error) = fs::remove_file(active_path(&rt, name)) {
                    eprintln!("appwarm: could not remove old active record for {name}: {error}");
                }
            }
            if debug {
                eprintln!(
                    "appwarm: froze {} in {} ({} MiB)",
                    name,
                    unit,
                    unit_memory(&unit)? / 1024 / 1024
                );
            }
            println!("staged {name}; run `appwarm show {name}` to reveal it");
            Ok(())
        }
        Err(error) => {
            let _ = run("systemctl", &["--user", "thaw", &unit]);
            let _ = run("systemctl", &["--user", "stop", &unit]);
            Err(error)
        }
    }
}

fn focused_workspace() -> io::Result<String> {
    let workspaces = niri(&["msg", "--json", "workspaces"])?;
    let ws = workspaces
        .as_array()
        .and_then(|list| {
            list.iter()
                .find(|ws| ws["is_focused"].as_bool() == Some(true))
        })
        .ok_or_else(|| io::Error::other("no focused niri workspace"))?;
    if let Some(name) = ws["name"].as_str() {
        Ok(name.to_owned())
    } else {
        ws["idx"]
            .as_u64()
            .map(|n| n.to_string())
            .ok_or_else(|| io::Error::other("focused workspace has no index"))
    }
}

pub fn show(name: &str, _paths: &Paths, debug: bool) -> io::Result<()> {
    let rt = runtime()?;
    let staged = read(&rt, name)?;
    let dest = focused_workspace()?;
    let id = staged.window_id.to_string();
    if !window_list()?
        .iter()
        .any(|w| w["id"].as_u64() == Some(staged.window_id))
    {
        return Err(io::Error::other(
            "staged window is gone; use evict to clean up",
        ));
    }
    run("systemctl", &["--user", "thaw", &staged.unit])?;
    let _ = run(
        "systemctl",
        &[
            "--user",
            "set-property",
            "--runtime",
            &staged.unit,
            "CPUWeight=100",
            "IOWeight=100",
            "MemoryMax=infinity",
        ],
    );
    // The rename records the revealed instance for the monitor, even if the
    // user closes the window before its next poll.
    if let Err(error) = fs::rename(state_path(&rt, name), active_path(&rt, name)) {
        let _ = run("systemctl", &["--user", "freeze", &staged.unit]);
        return Err(error);
    }
    if let Err(error) = run(
        "niri",
        &[
            "msg",
            "action",
            "move-window-to-workspace",
            "--window-id",
            &id,
            "--focus",
            "false",
            &dest,
        ],
    ) {
        let _ = fs::rename(active_path(&rt, name), state_path(&rt, name));
        let _ = run("systemctl", &["--user", "freeze", &staged.unit]);
        return Err(error);
    }
    if let Err(error) = run("niri", &["msg", "action", "focus-window", "--id", &id]) {
        eprintln!("appwarm: moved {name} but could not focus its window: {error}");
    }
    if debug {
        eprintln!("appwarm: revealed window {id} in workspace {dest}");
    }
    Ok(())
}

pub fn evict(name: &str, _paths: &Paths) -> io::Result<()> {
    let rt = runtime()?;
    let staged = read(&rt, name)?;
    let _ = run("systemctl", &["--user", "thaw", &staged.unit]);
    run("systemctl", &["--user", "stop", &staged.unit])?;
    fs::remove_file(state_path(&rt, name))?;
    Ok(())
}

pub fn list() -> io::Result<()> {
    let rt = runtime()?;
    for stage in stages(&rt)? {
        println!("{}\t{}\t{}", stage.name, stage.app_id, stage.window_id);
    }
    Ok(())
}

pub fn has(name: &str) -> io::Result<bool> {
    let base = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| io::Error::other("XDG_RUNTIME_DIR must be an absolute path"))?;
    Ok(base.join("appwarm").join(format!("{name}.stage")).exists())
}

pub fn stage_all(paths: &Paths, cfg: &Config, debug: bool) -> io::Result<()> {
    for entry in configured_stages(cfg) {
        if has(&entry.name)? {
            continue;
        }
        if let Err(error) = stage(
            &entry.name,
            &entry.app_id,
            &entry.command,
            paths,
            cfg,
            debug,
        ) {
            // Already-open apps and insufficient headroom are expected. One
            // failure must not prevent another selected app from staging.
            eprintln!("appwarm: skipped {}: {error}", entry.name);
        }
    }
    Ok(())
}

pub fn doctor(cfg: &Config) -> io::Result<()> {
    let entries = configured_stages(cfg);
    if entries.is_empty() {
        println!("no apps configured for hidden staging");
        return Ok(());
    }
    if entries.len() != cfg.stages.len() {
        return Err(io::Error::other("one or more stage entries are invalid"));
    }
    let hidden_ids = hidden_workspace_ids()?;
    println!("patched Niri IPC and hidden workspace: ready ({hidden_ids:?})");
    for entry in entries {
        let source = desktop::source(&entry.desktop_id)?;
        if entry.command[0].contains('/') && !std::path::Path::new(&entry.command[0]).exists() {
            return Err(io::Error::other(format!(
                "{} stage command is unavailable: {}",
                entry.name, entry.command[0]
            )));
        }
        println!(
            "{}: app ID {}, desktop {}, command {}",
            entry.name,
            entry.app_id,
            source.display(),
            entry.command[0]
        );
    }
    println!("cgroup routing and window ownership are verified during staging");
    Ok(())
}

fn pressure_high() -> io::Result<bool> {
    let text = fs::read_to_string("/proc/pressure/memory")?;
    let some = text
        .lines()
        .find(|line| line.starts_with("some "))
        .ok_or_else(|| io::Error::other("memory PSI is unavailable"))?;
    let avg10 = some
        .split_whitespace()
        .find_map(|field| {
            field
                .strip_prefix("avg10=")
                .and_then(|value| value.parse::<f64>().ok())
        })
        .ok_or_else(|| io::Error::other("invalid memory PSI data"))?;
    Ok(avg10 >= 2.0)
}

pub fn monitor(paths: &Paths, cfg: &Config, debug: bool) -> io::Result<()> {
    let entries = configured_stages(cfg);
    let mut next_attempt: HashMap<String, Instant> = HashMap::new();
    let mut observed_open: HashSet<String> = HashSet::new();
    loop {
        let headroom = memory_headroom(cfg)?;
        let pressured = headroom == 0 || pressure_high()?;
        if pressured {
            let rt = runtime()?;
            let mut candidates = stages(&rt)?;
            // Reclaim the largest frozen unit first. No user-visible unit is
            // considered because only files still in the stage registry qualify.
            candidates
                .sort_by_key(|entry| std::cmp::Reverse(unit_memory(&entry.unit).unwrap_or(0)));
            if let Some(candidate) = candidates.first() {
                eprintln!(
                    "appwarm: memory pressure; evicting {} ({} MiB)",
                    candidate.name,
                    unit_memory(&candidate.unit).unwrap_or(0) / 1024 / 1024
                );
                let _ = run("systemctl", &["--user", "thaw", &candidate.unit]);
                run("systemctl", &["--user", "stop", &candidate.unit])?;
                fs::remove_file(state_path(&rt, &candidate.name))?;
            }
        } else if !entries.is_empty() {
            let windows = match window_list() {
                Ok(windows) => windows,
                Err(error) => {
                    if debug {
                        eprintln!("appwarm: waiting for Niri: {error}");
                    }
                    thread::sleep(Duration::from_secs(2));
                    continue;
                }
            };
            for entry in &entries {
                let rt = runtime()?;
                let stage_file = state_path(&rt, &entry.name);
                let active_file = active_path(&rt, &entry.name);
                if stage_file.exists() {
                    let staged = read(&rt, &entry.name)?;
                    if unit_active(&staged.unit)
                        && windows
                            .iter()
                            .any(|w| w["id"].as_u64() == Some(staged.window_id))
                    {
                        observed_open.remove(&entry.name);
                        next_attempt.remove(&entry.name);
                        continue;
                    }
                    // A crashed frozen app must not leave a permanent stage
                    // record that prevents preparation of the next launch.
                    fs::rename(&stage_file, &active_file)?;
                }
                let active = if active_file.exists() {
                    Some(read_path(active_file, &entry.name)?)
                } else {
                    None
                };
                drop(rt);
                if windows.iter().any(|window| {
                    window["app_id"].as_str() == Some(entry.app_id.as_str())
                        || active.as_ref().is_some_and(|active| {
                            window_in_unit(window, &active.unit).unwrap_or(false)
                        })
                }) {
                    observed_open.insert(entry.name.clone());
                    next_attempt.remove(&entry.name);
                    continue;
                }
                if active.is_none()
                    && !observed_open.remove(&entry.name)
                    && !next_attempt.contains_key(&entry.name)
                {
                    continue;
                }
                observed_open.remove(&entry.name);
                let now = Instant::now();
                let deadline = next_attempt
                    .entry(entry.name.clone())
                    .or_insert_with(|| now + Duration::from_secs(cfg.restage_delay_sec));
                if now < *deadline {
                    continue;
                }
                if let Err(error) = stage(
                    &entry.name,
                    &entry.app_id,
                    &entry.command,
                    paths,
                    cfg,
                    debug,
                ) {
                    eprintln!("appwarm: could not restage {}: {error}", entry.name);
                    *deadline = Instant::now() + Duration::from_secs(60);
                } else {
                    next_attempt.remove(&entry.name);
                }
            }
        }
        thread::sleep(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_id_cannot_inject_a_window_rule() {
        assert!(valid_app_id("dev.zed.Zed"));
        assert!(!valid_app_id("foo\"\nopen-on-workspace \"main"));
    }

    #[test]
    fn stage_state_accepts_generated_units_and_rejects_other_units() {
        let path = env::temp_dir().join(format!("appwarm-stage-test-{}", std::process::id()));
        fs::write(&path, "appwarm-stage-zed-123.service\n42\ndev.zed.Zed\n").unwrap();
        assert_eq!(read_path(path.clone(), "zed").unwrap().window_id, 42);
        fs::write(&path, "appwarm-stage-zed-.service\n42\ndev.zed.Zed\n").unwrap();
        assert!(read_path(path.clone(), "zed").is_err());
        fs::write(&path, "appwarm-stage-other-123.service\n42\ndev.zed.Zed\n").unwrap();
        assert!(read_path(path.clone(), "zed").is_err());
        fs::remove_file(path).unwrap();
    }
}
