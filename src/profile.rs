use crate::config::{Config, Paths};
use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const BLOCK: u64 = 64 * 1024;
const MAX_EVENT: u64 = 2 * 1024 * 1024;
const MAX_FILES: usize = 8192;
const MAX_RANGES: usize = 65536;

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Stamp {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub sec: i64,
    pub nsec: i64,
}

impl Stamp {
    pub fn from_meta(meta: &fs::Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            size: meta.size(),
            sec: meta.mtime(),
            nsec: meta.mtime_nsec(),
        }
    }
}

// Browser HTTP caches, journals, and session files change too often to be useful
// across launches. They otherwise crowd out stable executable and resource pages.
fn volatile_path(path: &Path) -> bool {
    let bytes = path.as_os_str().as_bytes();
    let contains = |needle: &[u8]| bytes.windows(needle.len()).any(|part| part == needle);
    [
        b"/Cache/".as_slice(),
        b"/Code Cache/",
        b"/GPUCache/",
        b"/Crashpad/",
        b"/BrowserMetrics/",
        b"/Session Storage/",
        b"/Service Worker/CacheStorage/",
    ]
    .iter()
    .any(|needle| contains(needle))
        || [b"-wal".as_slice(), b"-shm", b".log", b".tmp"]
            .iter()
            .any(|suffix| bytes.ends_with(suffix))
        || bytes.ends_with(b"/LOCK")
        || contains(b"/Singleton")
}

pub struct FileRecord {
    pub path: PathBuf,
    pub stamp: Stamp,
    blocks: usize,
}

pub struct Launcher {
    raw: PathBuf,
    target: PathBuf,
    stamp: Stamp,
}

impl Launcher {
    pub fn resolve(command: &str) -> io::Result<Self> {
        let raw = if command.contains('/') {
            let path = PathBuf::from(command);
            if path.is_absolute() {
                path
            } else {
                env::current_dir()?.join(path)
            }
        } else {
            let path = env::var_os("PATH").ok_or_else(|| io::Error::other("PATH is unset"))?;
            env::split_paths(&path)
                .map(|dir| dir.join(command))
                .find(|candidate| {
                    fs::metadata(candidate)
                        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                })
                .ok_or_else(|| io::Error::other(format!("cannot find launcher {command}")))?
        };
        Self::from_raw(raw)
    }

    fn from_raw(raw: PathBuf) -> io::Result<Self> {
        let target = fs::canonicalize(&raw)?;
        let stamp = Stamp::from_meta(&fs::metadata(&target)?);
        Ok(Self { raw, target, stamp })
    }

    fn still_current(&self) -> bool {
        Self::from_raw(self.raw.clone())
            .is_ok_and(|now| now.target == self.target && now.stamp == self.stamp)
    }
}
#[derive(Clone, Copy)]
pub struct Range {
    pub file: usize,
    pub offset: u64,
    pub hits: u32,
    pub priority: u8,
    pub first_seq: u32,
    seen: bool,
}

pub struct Profile {
    pub files: Vec<FileRecord>,
    raw_files: HashMap<PathBuf, Option<usize>>,
    canonical_files: HashMap<PathBuf, usize>,
    pub ranges: HashMap<(usize, u64), Range>,
    pub samples: u32,
    pub stale: usize,
    pub legacy: bool,
    pub launcher: Option<Launcher>,
    cache: PathBuf,
}

impl Profile {
    pub fn new(cache: &Path) -> Self {
        Self {
            files: Vec::new(),
            raw_files: HashMap::new(),
            canonical_files: HashMap::new(),
            ranges: HashMap::new(),
            samples: 0,
            stale: 0,
            legacy: false,
            launcher: None,
            cache: cache.to_path_buf(),
        }
    }

    fn file_id(&mut self, raw: &Path) -> Option<usize> {
        if let Some(id) = self.raw_files.get(raw) {
            return *id;
        }
        let result = (|| {
            let path = fs::canonicalize(raw).ok()?;
            let bytes = path.as_os_str().as_bytes();
            if [
                b"/proc/".as_slice(),
                b"/sys/",
                b"/dev/",
                b"/tmp/",
                b"/run/user/",
            ]
            .iter()
            .any(|p| bytes.starts_with(p))
                || path.starts_with(&self.cache)
                || volatile_path(&path)
            {
                return None;
            }
            if let Some(id) = self.canonical_files.get(&path) {
                return Some(*id);
            }
            if self.files.len() >= MAX_FILES {
                return None;
            }
            let meta = fs::metadata(&path).ok()?;
            if !meta.is_file() || meta.len() == 0 || File::open(&path).is_err() {
                return None;
            }
            let id = self.files.len();
            self.files.push(FileRecord {
                path: path.clone(),
                stamp: Stamp::from_meta(&meta),
                blocks: 0,
            });
            self.canonical_files.insert(path, id);
            Some(id)
        })();
        self.raw_files.insert(raw.to_path_buf(), result);
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn add_range(
        &mut self,
        file: usize,
        offset: u64,
        hits: u32,
        priority: u8,
        first_seq: u32,
        seen: bool,
        cfg: &Config,
    ) {
        let key = (file, offset);
        if let Some(range) = self.ranges.get_mut(&key) {
            if seen && !range.seen {
                range.first_seq = if first_seq == u32::MAX - 1 {
                    range.first_seq
                } else if range.first_seq == u32::MAX {
                    first_seq
                } else {
                    ((range.first_seq as u64 * range.hits as u64 + first_seq as u64)
                        / (range.hits as u64 + 1)) as u32
                };
                range.hits = range.hits.saturating_add(1);
                range.seen = true;
            }
            if seen {
                range.priority = range.priority.max(priority);
                if self.samples == 0 {
                    range.first_seq = range.first_seq.min(first_seq);
                }
            }
            return;
        }
        if self.ranges.len() >= MAX_RANGES
            || self.files[file].blocks as u64 * BLOCK >= cfg.max_file_mib * 1024 * 1024
        {
            return;
        }
        self.ranges.insert(
            key,
            Range {
                file,
                offset,
                hits,
                priority,
                first_seq,
                seen,
            },
        );
        self.files[file].blocks += 1;
    }

    pub fn mark(
        &mut self,
        raw: &Path,
        offset: u64,
        length: u64,
        priority: u8,
        seq: u32,
        cfg: &Config,
    ) {
        let Some(id) = self.file_id(raw) else { return };
        let size = self.files[id].stamp.size;
        if length == 0 || offset >= size {
            return;
        }
        let length = length.min(MAX_EVENT).min(size - offset);
        let start = offset / BLOCK * BLOCK;
        let end = (offset + length - 1) / BLOCK * BLOCK;
        for at in (start..=end).step_by(BLOCK as usize) {
            self.add_range(id, at, 1, priority, seq, true, cfg);
        }
    }

    pub fn load(&mut self, path: &Path, cfg: &Config) -> io::Result<bool> {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        let mut lines = BufReader::new(file).lines();
        let Some(first) = lines.next() else {
            return Err(io::Error::other("empty profile"));
        };
        let first = first?;
        let header: Vec<_> = first.split('\t').collect();
        if header.len() != 10 || !matches!(header[0], "APPWARM2" | "APPWARM3") {
            return Err(io::Error::other("unsupported profile format"));
        }
        let version3 = header[0] == "APPWARM3";
        self.legacy = !version3;
        self.samples = header[2].parse().unwrap_or(0);
        let raw = hex_decode(header[3]).ok_or_else(|| io::Error::other("invalid launcher path"))?;
        let target =
            hex_decode(header[4]).ok_or_else(|| io::Error::other("invalid launcher target"))?;
        let stamp = Stamp {
            dev: header[5]
                .parse()
                .map_err(|_| io::Error::other("invalid launcher device"))?,
            ino: header[6]
                .parse()
                .map_err(|_| io::Error::other("invalid launcher inode"))?,
            size: header[7]
                .parse()
                .map_err(|_| io::Error::other("invalid launcher size"))?,
            sec: header[8]
                .parse()
                .map_err(|_| io::Error::other("invalid launcher time"))?,
            nsec: header[9]
                .parse()
                .map_err(|_| io::Error::other("invalid launcher time"))?,
        };
        self.launcher = Some(Launcher { raw, target, stamp });
        for line in lines {
            let line = line?;
            let fields: Vec<_> = line.split('\t').collect();
            if fields.len() != if version3 { 11 } else { 9 } || fields[0] != "P" {
                self.stale += 1;
                continue;
            }
            let Some(raw) = hex_decode(fields[1]) else {
                self.stale += 1;
                continue;
            };
            let Some(id) = self.file_id(&raw) else {
                self.stale += 1;
                continue;
            };
            let parsed = (|| {
                Some((
                    Stamp {
                        dev: fields[2].parse().ok()?,
                        ino: fields[3].parse().ok()?,
                        size: fields[4].parse().ok()?,
                        sec: fields[5].parse().ok()?,
                        nsec: fields[6].parse().ok()?,
                    },
                    fields[7].parse::<u64>().ok()?,
                    fields[8].parse::<u32>().ok()?,
                    if version3 {
                        fields[9].parse::<u8>().ok()?
                    } else {
                        0
                    },
                    if version3 {
                        fields[10].parse::<u32>().ok()?
                    } else {
                        u32::MAX
                    },
                ))
            })();
            let Some((stamp, offset, hits, priority, first_seq)) = parsed else {
                self.stale += 1;
                continue;
            };
            if stamp != self.files[id].stamp
                || offset % BLOCK != 0
                || offset >= stamp.size
                || hits == 0
                || hits > 1_000_000
            {
                self.stale += 1;
                continue;
            }
            self.add_range(id, offset, hits, priority, first_seq, false, cfg);
        }
        Ok(true)
    }

    pub fn ordered(&self) -> Vec<Range> {
        let mut ranges: Vec<_> = self.ranges.values().copied().collect();
        ranges.sort_by(|a, b| {
            b.hits
                .cmp(&a.hits)
                .then_with(|| b.priority.cmp(&a.priority))
                .then_with(|| a.first_seq.cmp(&b.first_seq))
                .then_with(|| self.files[a.file].path.cmp(&self.files[b.file].path))
                .then_with(|| a.offset.cmp(&b.offset))
        });
        ranges
    }

    pub fn warm_ranges(&self) -> Vec<Range> {
        let confirmed = self
            .ranges
            .values()
            .filter(|range| range.priority == 5)
            .count();
        let mut ranges = self.ordered();
        if confirmed >= 64 {
            // With at least 4 MiB of confirmed mapped pages, broad mmap ranges
            // are speculation and can evict more useful cache pages.
            ranges.retain(|range| range.priority != 1);
        }
        ranges
    }

    pub fn observed_count(&self) -> usize {
        self.ranges.values().filter(|range| range.seen).count()
    }

    pub fn launcher_current(&self) -> bool {
        self.launcher.as_ref().is_some_and(Launcher::still_current)
    }

    pub fn save(&self, path: &Path, name: &str) -> io::Result<()> {
        let launcher = self
            .launcher
            .as_ref()
            .ok_or_else(|| io::Error::other("launcher is missing"))?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let tmp = path.with_file_name(format!(".new-{}-{nonce}", std::process::id()));
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        let result = (|| {
            writeln!(
                out,
                "APPWARM3\t{name}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                self.samples + 1,
                hex_encode(&launcher.raw),
                hex_encode(&launcher.target),
                launcher.stamp.dev,
                launcher.stamp.ino,
                launcher.stamp.size,
                launcher.stamp.sec,
                launcher.stamp.nsec
            )?;
            for range in self.ordered() {
                let f = &self.files[range.file];
                writeln!(
                    out,
                    "P\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    hex_encode(&f.path),
                    f.stamp.dev,
                    f.stamp.ino,
                    f.stamp.size,
                    f.stamp.sec,
                    f.stamp.nsec,
                    range.offset,
                    range.hits,
                    range.priority,
                    range.first_seq
                )?;
            }
            out.sync_all()
        })();
        drop(out);
        if let Err(e) = result {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        if let Err(e) = fs::rename(&tmp, path) {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(())
    }
}

fn hex_encode(path: &Path) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let bytes = path.as_os_str().as_bytes();
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 15) as usize] as char);
    }
    out
}

fn hex_decode(s: &str) -> Option<PathBuf> {
    if s.is_empty() || !s.len().is_multiple_of(2) || s.len() >= 8192 {
        return None;
    }
    let mut bytes = Vec::with_capacity(s.len() / 2);
    for pair in s.as_bytes().chunks_exact(2) {
        let value = |b: u8| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            _ => None,
        };
        let b = value(pair[0])? * 16 + value(pair[1])?;
        if b == 0 {
            return None;
        }
        bytes.push(b);
    }
    Some(PathBuf::from(OsString::from_vec(bytes)))
}

pub fn status_one(name: &str, paths: &Paths, cfg: &Config) -> io::Result<()> {
    let mut profile = Profile::new(&paths.cache);
    if !profile.load(&paths.profile(name), cfg)? {
        return Err(io::Error::other(format!("no profile for {name}")));
    }
    let files = profile
        .ranges
        .keys()
        .map(|(id, _)| *id)
        .collect::<HashSet<_>>()
        .len();
    let warm_mib = if profile.legacy {
        0.0
    } else {
        profile.warm_ranges().len() as f64 * BLOCK as f64 / 1048576.0
    };
    println!(
        "{name}: {} samples, {files} files, {} ranges, {:.1} MiB observed, {warm_mib:.1} MiB eligible to warm, {} stale ranges, launcher {}",
        profile.samples,
        profile.ranges.len(),
        profile.ranges.len() as f64 * BLOCK as f64 / 1048576.0,
        profile.stale,
        if profile.launcher_current() { "current" } else { "changed" }
    );
    if profile.legacy {
        println!("  legacy profile: relearn with appwarm 0.2 before warming");
    }
    let mut seen = HashSet::new();
    for range in profile.ordered() {
        if seen.insert(range.file) {
            println!(
                "  {} hits  {}",
                range.hits,
                profile.files[range.file].path.display()
            );
        }
        if seen.len() == 5 {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn launcher_retarget_invalidates_profile() {
        let dir = env::temp_dir().join(format!("appwarm-launcher-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a");
        let b = dir.join("b");
        let link = dir.join("current");
        fs::write(&a, b"one").unwrap();
        fs::write(&b, b"two").unwrap();
        symlink(&a, &link).unwrap();
        let launcher = Launcher::resolve(link.to_str().unwrap()).unwrap();
        assert!(launcher.still_current());
        fs::remove_file(&link).unwrap();
        symlink(&b, &link).unwrap();
        assert!(!launcher.still_current());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn resident_pages_rank_ahead_of_broad_mappings() {
        let cfg = Config::default();
        let mut profile = Profile::new(Path::new("/tmp/appwarm-test-cache"));
        profile.mark(Path::new("/etc/hosts"), 0, 1, 1, 0, &cfg);
        profile.mark(Path::new("/etc/passwd"), 0, 1, 5, 100, &cfg);
        assert_eq!(profile.ordered()[0].priority, 5);
        assert_eq!(profile.ordered()[1].priority, 1);
    }

    #[test]
    fn volatile_browser_cache_is_excluded() {
        assert!(volatile_path(Path::new(
            "/home/user/.cache/BraveSoftware/Default/Cache/Cache_Data/123"
        )));
        assert!(volatile_path(Path::new(
            "/home/user/.config/BraveSoftware/Default/History-wal"
        )));
        assert!(!volatile_path(Path::new("/nix/store/app/lib/libgtk.so")));
    }

    #[test]
    fn confirmed_pages_suppress_speculative_mappings() {
        let mut profile = Profile::new(Path::new("/tmp/appwarm-test-cache"));
        profile.files.push(FileRecord {
            path: PathBuf::from("/nix/store/example"),
            stamp: Stamp {
                dev: 1,
                ino: 1,
                size: 5 * 1024 * 1024,
                sec: 0,
                nsec: 0,
            },
            blocks: 65,
        });
        for block in 0..64 {
            profile.ranges.insert(
                (0, block * BLOCK),
                Range {
                    file: 0,
                    offset: block * BLOCK,
                    hits: 1,
                    priority: 5,
                    first_seq: block as u32,
                    seen: true,
                },
            );
        }
        profile.ranges.insert(
            (0, 64 * BLOCK),
            Range {
                file: 0,
                offset: 64 * BLOCK,
                hits: 1,
                priority: 1,
                first_seq: 65,
                seen: true,
            },
        );
        assert_eq!(profile.warm_ranges().len(), 64);
    }

    #[test]
    fn old_profile_is_flagged_for_relearning() {
        let path = env::temp_dir().join(format!("appwarm-legacy-test-{}", std::process::id()));
        let launcher = Launcher::resolve("/etc/hosts").unwrap();
        fs::write(
            &path,
            format!(
                "APPWARM2\ttest\t1\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                hex_encode(&launcher.raw),
                hex_encode(&launcher.target),
                launcher.stamp.dev,
                launcher.stamp.ino,
                launcher.stamp.size,
                launcher.stamp.sec,
                launcher.stamp.nsec,
            ),
        )
        .unwrap();
        let mut profile = Profile::new(Path::new("/tmp/appwarm-test-cache"));
        assert!(profile.load(&path, &Config::default()).unwrap());
        assert!(profile.legacy);
        fs::remove_file(path).unwrap();
    }
}
