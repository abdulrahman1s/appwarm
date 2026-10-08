use crate::config::{Config, Paths};
use crate::linux::stop_tracer;
use crate::pagemap::mark_present_pages;
use crate::profile::{Launcher, Profile, BLOCK};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TRACE_LIMIT: u64 = 64 * 1024 * 1024;

fn unquote(s: &str) -> Option<PathBuf> {
    let start = s.find('"')? + 1;
    let mut bytes = Vec::new();
    let mut chars = s.as_bytes()[start..].iter().copied();
    while let Some(b) = chars.next() {
        if b == b'"' {
            return Some(PathBuf::from(OsString::from_vec(bytes)));
        }
        if b != b'\\' {
            bytes.push(b);
            continue;
        }
        let escape = chars.next()?;
        let value = match escape {
            b'n' => b'\n',
            b't' => b'\t',
            b'r' => b'\r',
            b'\\' => b'\\',
            b'"' => b'"',
            b'x' => {
                let a = chars.next()?.to_ascii_lowercase();
                let b = chars.next()?.to_ascii_lowercase();
                let hex = |v: u8| match v {
                    b'0'..=b'9' => Some(v - b'0'),
                    b'a'..=b'f' => Some(v - b'a' + 10),
                    _ => None,
                };
                hex(a)? * 16 + hex(b)?
            }
            b'0'..=b'7' => escape - b'0',
            other => other,
        };
        if value == 0 {
            return None;
        }
        bytes.push(value);
    }
    None
}

fn fd_path(s: &str) -> Option<(i32, PathBuf)> {
    let lt = s.find('<')?;
    let gt = lt + 1 + s[lt + 1..].find('>')?;
    let before = &s.as_bytes()[..lt];
    let start = before
        .iter()
        .rposition(|b| !b.is_ascii_digit())
        .map_or(0, |p| p + 1);
    let fd = s[start..lt].parse().ok()?;
    let raw = &s[lt + 1..gt];
    if !raw.starts_with('/') || raw.contains(" (deleted)") {
        return None;
    }
    Some((fd, PathBuf::from(raw)))
}

#[allow(clippy::too_many_arguments)]
fn mark_sequential(
    fds: &mut HashMap<(i32, i32), (PathBuf, u64)>,
    pid: i32,
    fd: i32,
    path: PathBuf,
    bytes: u64,
    profile: &mut Profile,
    seq: u32,
    cfg: &Config,
) {
    if bytes == 0 {
        return;
    }
    let state = fds.entry((pid, fd)).or_insert_with(|| (path.clone(), 0));
    if state.0 != path {
        *state = (path.clone(), 0);
    }
    profile.mark(&path, state.1, bytes, 2, seq, cfg);
    state.1 = state.1.saturating_add(bytes);
}

fn parse_trace(path: &Path, profile: &mut Profile, cfg: &Config) -> io::Result<HashSet<i32>> {
    let file = File::open(path)?;
    let mut fds: HashMap<(i32, i32), (PathBuf, u64)> = HashMap::new();
    let mut pids = HashSet::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let seq = u32::try_from(index).unwrap_or(u32::MAX - 1);
        let line = line?;
        if line.contains("<unfinished ...>") {
            continue;
        }
        let Some((call, result)) = line.split_once(") = ") else {
            continue;
        };
        let token = result.split_whitespace().next().unwrap_or("");
        if token == "-1" || token == "?" {
            continue;
        }
        let pid = line
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0);
        if pid > 0 && pids.len() < 512 {
            pids.insert(pid);
        }
        if let Some(at) = call.find("execve(").or_else(|| call.find("execveat(")) {
            if token == "0" {
                if let Some(path) = unquote(&call[at..]) {
                    profile.mark(&path, 0, BLOCK * 4, 4, seq, cfg);
                }
            }
        } else if call.contains("openat(") || call.contains("openat2(") {
            if let Some((fd, path)) = fd_path(result) {
                fds.insert((pid, fd), (path, 0));
            }
        } else if let Some(at) = call.find("mmap(") {
            let part = &call[at..];
            if let (Some((_, path)), Some(first), Some(last)) =
                (fd_path(part), part.find(','), part.rfind(','))
            {
                let len = part[first + 1..]
                    .split(',')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .parse::<u64>()
                    .ok();
                let off = part[last + 1..].trim().parse::<u64>().ok();
                if let (Some(len), Some(off)) = (len, off) {
                    profile.mark(&path, off, len, 1, seq, cfg);
                }
            }
        } else if let Some(at) = call.find("pread64(").or_else(|| call.find("preadv(")) {
            if let (Some((_, path)), Some(n), Some(last)) = (
                fd_path(&call[at..]),
                token.parse::<u64>().ok(),
                call.rfind(','),
            ) {
                if let Ok(off) = call[last + 1..].trim().parse::<u64>() {
                    profile.mark(&path, off, n, 2, seq, cfg);
                }
            }
        } else if let Some(at) = call.find("preadv2(") {
            let part = &call[at..];
            if let (Some((_, path)), Some(n)) = (fd_path(part), token.parse::<u64>().ok()) {
                if let Some(off) = part
                    .rsplit(',')
                    .nth(1)
                    .and_then(|s| s.trim().parse::<u64>().ok())
                {
                    profile.mark(&path, off, n, 2, seq, cfg);
                }
            }
        } else if let Some(at) = call.find("lseek(") {
            if let (Some((fd, path)), Ok(off)) = (fd_path(&call[at..]), token.parse::<u64>()) {
                fds.insert((pid, fd), (path, off));
            }
        } else if let Some(at) = call.find("sendfile(") {
            let part = &call[at..];
            if let (Some(comma), Ok(n)) = (part.find(','), token.parse::<u64>()) {
                if let Some((fd, path)) = fd_path(&part[comma + 1..]) {
                    mark_sequential(&mut fds, pid, fd, path, n, profile, seq, cfg);
                }
            }
        } else if let Some(at) = call
            .find("splice(")
            .or_else(|| call.find("copy_file_range("))
        {
            if let (Some((fd, path)), Ok(n)) = (fd_path(&call[at..]), token.parse::<u64>()) {
                mark_sequential(&mut fds, pid, fd, path, n, profile, seq, cfg);
            }
        } else if let Some(at) = call.find("read(").or_else(|| call.find("readv(")) {
            if let (Some((fd, path)), Ok(n)) = (fd_path(&call[at..]), token.parse::<u64>()) {
                mark_sequential(&mut fds, pid, fd, path, n, profile, seq, cfg);
            }
        }
    }
    Ok(pids)
}

pub fn learn(
    name: &str,
    command: &[String],
    paths: &Paths,
    cfg: &Config,
    debug: bool,
) -> io::Result<()> {
    let launcher = Launcher::resolve(&command[0])?;
    paths.ensure_cache()?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let trace = paths
        .cache
        .join(format!(".trace-{}-{nonce}", std::process::id()));
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&trace)?;
    const STRACE: &str = "@STRACE@";
    let strace = if STRACE.starts_with('@') {
        "strace"
    } else {
        STRACE
    };
    let child = Command::new(strace)
        .args([
            "-I",
            "1",
            "-f",
            "-qq",
            "-ttt",
            "-yy",
            "-s",
            "0",
            "-e",
            "trace=execve,execveat,openat,openat2,read,readv,pread64,preadv,preadv2,lseek,mmap,splice,sendfile,copy_file_range",
            "-o",
        ])
        .arg(&trace)
        .arg("--")
        .args(command)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            let _ = fs::remove_file(&trace);
            return Err(e);
        }
    };
    eprintln!(
        "appwarm: tracing {name} for up to {} seconds (app stays open)",
        cfg.window_sec
    );
    let start = Instant::now();
    let mut truncated = false;
    let mut exited = None;
    let mut finished_early = false;
    while start.elapsed() < Duration::from_secs(cfg.window_sec) {
        thread::sleep(Duration::from_millis(100));
        if let Some(status) = child.try_wait()? {
            exited = Some(status);
            finished_early = true;
            break;
        }
        if fs::metadata(&trace).is_ok_and(|m| m.len() > TRACE_LIMIT) {
            truncated = true;
            break;
        }
    }
    if exited.is_none() {
        stop_tracer(child.id() as i32);
        exited = Some(child.wait()?);
    }
    if debug {
        eprintln!(
            "appwarm: trace at {}, tracer status {:?}",
            trace.display(),
            exited
        );
    }
    if truncated {
        eprintln!("appwarm: trace reached 64 MiB; captured first part only");
    }
    if finished_early && exited.is_some_and(|status| !status.success()) {
        fs::remove_file(&trace)?;
        return Err(io::Error::other(
            "application exited unsuccessfully during learning",
        ));
    }
    let mut profile = Profile::new(&paths.cache);
    let profile_path = paths.profile(name);
    if profile.load(&profile_path, cfg)? && (profile.legacy || !profile.launcher_current()) {
        eprintln!("appwarm: old format or launcher changed; rebuilding {name} profile");
        profile = Profile::new(&paths.cache);
    }
    profile.launcher = Some(launcher);
    let pids = parse_trace(&trace, &mut profile, cfg)?;
    let present = mark_present_pages(&pids, &mut profile, cfg, debug);
    if debug {
        eprintln!("appwarm: observed {present} file-backed resident pages in live app processes");
    }
    fs::remove_file(&trace)?;
    if profile.observed_count() == 0 {
        return Err(io::Error::other("no readable startup file ranges found"));
    }
    let files = profile
        .ranges
        .keys()
        .map(|(id, _)| *id)
        .collect::<HashSet<_>>()
        .len();
    profile.save(&profile_path, name)?;
    eprintln!(
        "appwarm: learned {files} files, {} ranges ({} samples)",
        profile.ranges.len(),
        profile.samples + 1
    );
    if profile.stale > 0 {
        eprintln!("appwarm: replaced {} stale ranges", profile.stale);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splice_reads_are_profiled_once_per_run() {
        let trace = std::env::temp_dir().join(format!("appwarm-trace-test-{}", std::process::id()));
        let text = "123 1.0 openat(AT_FDCWD</>, \"/etc/hosts\", O_RDONLY) = 3</etc/hosts>\n\
123 1.1 splice(3</etc/hosts>, NULL, 4<pipe:[1]>, NULL, 128, 0) = 128\n\
123 1.2 readv(3</etc/hosts>, [], 1) = 16\n";
        fs::write(&trace, text).unwrap();
        let mut profile = Profile::new(Path::new("/tmp/appwarm-test-cache"));
        parse_trace(&trace, &mut profile, &Config::default()).unwrap();
        fs::remove_file(trace).unwrap();
        assert_eq!(profile.ranges.len(), 1);
        assert_eq!(profile.ranges.values().next().unwrap().hits, 1);
    }
}
