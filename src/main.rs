#![cfg(target_os = "linux")]

mod config;
mod desktop;
mod linux;
mod pagemap;
mod profile;
mod stage;
mod trace;
mod warm;

use crate::config::{valid_name, Config, Paths};
use crate::linux::lower_priority;
use crate::profile::status_one;
use crate::trace::learn;
use crate::warm::warm_one;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::{env, fs, io};

fn usage() -> ! {
    eprintln!("usage: appwarm [--debug] [--window SECONDS] [--budget-mib MIB] learn NAME [-- COMMAND [ARGS...]]\n       appwarm [--debug] [--budget-mib MIB] warm NAME|warm-all|status NAME|forget NAME|list\n       appwarm [--debug] stage NAME --app-id APP_ID -- COMMAND [ARGS...]\n       appwarm [--debug] show NAME|evict NAME|is-staged NAME|staged\n       appwarm stage-all|desktop-sync|monitor\n       appwarm desktop|launch NAME -- COMMAND [ARGS...]");
    std::process::exit(2)
}

fn run() -> io::Result<()> {
    let paths = Paths::new()?;
    let mut cfg = Config::read(&paths.config);
    let args: Vec<String> = env::args().collect();
    let mut i = 1;
    let mut debug = false;
    while i < args.len() && args[i].starts_with('-') {
        match args[i].as_str() {
            "--debug" => debug = true,
            "--window" | "--budget-mib" => {
                let key = &args[i];
                i += 1;
                let Some(n) = args
                    .get(i)
                    .and_then(|v| v.parse::<u64>().ok())
                    .filter(|n| (1..=4096).contains(n))
                else {
                    usage()
                };
                if key == "--window" {
                    cfg.window_sec = n;
                } else {
                    cfg.budget_mib = n;
                }
            }
            _ => usage(),
        }
        i += 1;
    }
    let Some(action) = args.get(i) else { usage() };
    i += 1;
    match action.as_str() {
        "list" if i == args.len() => {
            if let Ok(dir) = fs::read_dir(&paths.cache) {
                let mut names: Vec<_> = dir
                    .flatten()
                    .filter_map(|e| {
                        e.file_name()
                            .to_str()
                            .and_then(|s| s.strip_suffix(".profile"))
                            .map(str::to_owned)
                    })
                    .collect();
                names.sort();
                for name in names {
                    println!("{name}");
                }
            }
        }
        "warm-all" if i == args.len() => {
            lower_priority().map_err(|e| {
                io::Error::new(e.kind(), format!("cannot lower warm priority: {e}"))
            })?;
            let mut budget = cfg.budget_mib * 1024 * 1024;
            let apps: Vec<_> = cfg
                .apps
                .split(',')
                .map(str::trim)
                .filter(|name| valid_name(name))
                .collect();
            for (index, name) in apps.iter().enumerate() {
                let mut allowance = budget / (apps.len() - index) as u64;
                let initial = allowance;
                warm_one(name, &paths, &cfg, &mut allowance, debug)?;
                budget -= initial - allowance;
            }
            if apps.is_empty() {
                eprintln!("appwarm: no apps configured for warm-all");
            }
        }
        "staged" if i == args.len() => stage::list()?,
        "stage-all" if i == args.len() => stage::stage_all(&paths, &cfg, debug)?,
        "desktop-sync" if i == args.len() => desktop::sync(&cfg)?,
        "monitor" if i == args.len() => stage::monitor(&cfg, debug)?,
        "learn" | "warm" | "status" | "forget" | "stage" | "show" | "evict" | "is-staged"
        | "launch" | "desktop" => {
            let Some(name) = args.get(i) else { usage() };
            i += 1;
            if !valid_name(name) {
                return Err(io::Error::other("invalid app name"));
            }
            match action.as_str() {
                "learn" => {
                    let default = [name.clone()];
                    let command = if i == args.len() {
                        &default[..]
                    } else if args[i] == "--" && i + 1 < args.len() {
                        &args[i + 1..]
                    } else {
                        usage()
                    };
                    learn(name, command, &paths, &cfg, debug)?;
                }
                "warm" if i == args.len() => {
                    lower_priority().map_err(|e| {
                        io::Error::new(e.kind(), format!("cannot lower warm priority: {e}"))
                    })?;
                    let mut budget = cfg.budget_mib * 1024 * 1024;
                    warm_one(name, &paths, &cfg, &mut budget, debug)?;
                }
                "status" if i == args.len() => status_one(name, &paths, &cfg)?,
                "forget" if i == args.len() => fs::remove_file(paths.profile(name))?,
                "stage"
                    if args.get(i).map(String::as_str) == Some("--app-id")
                        && args.get(i + 2).map(String::as_str) == Some("--")
                        && i + 3 < args.len() =>
                {
                    stage::stage(name, &args[i + 1], &args[i + 3..], &paths, &cfg, debug)?;
                }
                "show" if i == args.len() => stage::show(name, &paths, debug)?,
                "evict" if i == args.len() => stage::evict(name, &paths)?,
                "is-staged" if i == args.len() => {
                    if !stage::has(name)? {
                        std::process::exit(1);
                    }
                }
                "launch" | "desktop"
                    if args.get(i).map(String::as_str) == Some("--") && i + 1 < args.len() =>
                {
                    if stage::has(name)? {
                        match stage::show(name, &paths, debug) {
                            Ok(()) => {
                                if action == "desktop" && i + 2 < args.len() {
                                    return Err(Command::new(&args[i + 1])
                                        .args(&args[i + 2..])
                                        .exec());
                                }
                            }
                            Err(error) => {
                                eprintln!("appwarm: reveal failed ({error}); launching normally");
                                let _ = stage::evict(name, &paths);
                                return Err(Command::new(&args[i + 1]).args(&args[i + 2..]).exec());
                            }
                        }
                    } else {
                        return Err(Command::new(&args[i + 1]).args(&args[i + 2..]).exec());
                    }
                }
                _ => usage(),
            }
        }
        _ => usage(),
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("appwarm: {e}");
        std::process::exit(1);
    }
}
