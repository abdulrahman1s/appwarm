use std::env;
use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct Config {
    pub window_sec: u64,
    pub budget_mib: u64,
    pub min_available_mib: u64,
    pub max_file_mib: u64,
    pub stage_budget_mib: u64,
    pub stage_settle_ms: u64,
    pub restage_delay_sec: u64,
    pub stages: Vec<String>,
    pub apps: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            window_sec: default_number("WINDOW_SEC", 10, 3600),
            budget_mib: default_number("BUDGET_MIB", 256, 4096),
            min_available_mib: default_number("MIN_AVAILABLE_MIB", 1024, 1_048_576),
            max_file_mib: default_number("MAX_FILE_MIB", 16, 4096),
            stage_budget_mib: default_number("STAGE_BUDGET_MIB", 2048, 16384),
            stage_settle_ms: default_number("STAGE_SETTLE_MS", 2000, 30000),
            restage_delay_sec: default_number("RESTAGE_DELAY_SEC", 5, 300),
            stages: env::var("APPWARM_DEFAULT_STAGES")
                .ok()
                .map(|value| {
                    value
                        .split(';')
                        .filter(|entry| !entry.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            apps: env::var("APPWARM_DEFAULT_APPS").unwrap_or_default(),
        }
    }
}

fn default_number(name: &str, fallback: u64, max: u64) -> u64 {
    env::var(format!("APPWARM_DEFAULT_{name}"))
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (1..=max).contains(value))
        .unwrap_or(fallback)
}

impl Config {
    pub fn read(path: &Path) -> Self {
        let mut cfg = Self::default();
        if let Ok(text) = fs::read_to_string(path) {
            let mut custom_stages = false;
            for line in text.lines() {
                let line = line.trim();
                if line.starts_with('#') || line.is_empty() {
                    continue;
                }
                let Some((key, value)) = line.split_once('=') else {
                    continue;
                };
                let key = key.trim();
                let value = value.trim();
                if key == "apps" {
                    cfg.apps = value.to_string();
                    continue;
                }
                if key == "stage" || key == "stages" {
                    if !custom_stages {
                        cfg.stages.clear();
                        custom_stages = true;
                    }
                    if key == "stages" {
                        continue;
                    }
                    cfg.stages.push(value.to_owned());
                    continue;
                }
                let Ok(n) = value.parse::<u64>() else {
                    continue;
                };
                if n == 0 {
                    continue;
                }
                match key {
                    "window_sec" if n <= 3600 => cfg.window_sec = n,
                    "budget_mib" if n <= 4096 => cfg.budget_mib = n,
                    "min_available_mib" if n <= 1_048_576 => cfg.min_available_mib = n,
                    "max_file_mib" if n <= 4096 => cfg.max_file_mib = n,
                    "stage_budget_mib" if n <= 16384 => cfg.stage_budget_mib = n,
                    "stage_settle_ms" if n <= 30000 => cfg.stage_settle_ms = n,
                    "restage_delay_sec" if n <= 300 => cfg.restage_delay_sec = n,
                    _ => {}
                }
            }
        }
        cfg
    }
}

pub struct Paths {
    pub cache: PathBuf,
    pub config: PathBuf,
}

impl Paths {
    pub fn new() -> io::Result<Self> {
        let home = env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is unset"))?;
        let home = PathBuf::from(home);
        if !home.is_absolute() {
            return Err(io::Error::other("HOME must be absolute"));
        }
        let cache_base = env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".cache"));
        let config_base = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".config"));
        Ok(Self {
            cache: cache_base.join("appwarm"),
            config: config_base.join("appwarm/config"),
        })
    }

    pub fn profile(&self, name: &str) -> PathBuf {
        self.cache.join(format!("{name}.profile"))
    }

    pub fn ensure_cache(&self) -> io::Result<()> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.cache)?;
        if !self.cache.is_dir() {
            return Err(io::Error::other("cache path is not a directory"));
        }
        Ok(())
    }
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}
