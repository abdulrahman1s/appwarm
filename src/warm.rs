use crate::config::{Config, Paths};
use crate::linux::{advise, memory_headroom, page_size, resident_pages};
use crate::profile::{Profile, Stamp, BLOCK};
use std::fs::{self, File};
use std::io;
use std::thread;
use std::time::Duration;

pub fn warm_one(
    name: &str,
    paths: &Paths,
    cfg: &Config,
    budget: &mut u64,
    debug: bool,
) -> io::Result<()> {
    let mut profile = Profile::new(&paths.cache);
    if !profile.load(&paths.profile(name), cfg)? {
        eprintln!("appwarm: no profile for {name}");
        return Ok(());
    }
    if profile.legacy {
        eprintln!(
            "appwarm: {name} profile needs relearning with appwarm 0.2; run `appwarm learn {name}`"
        );
        return Ok(());
    }
    if !profile.launcher_current() {
        eprintln!(
            "appwarm: {name} launcher changed; run `appwarm learn {name}` to refresh the profile"
        );
        return Ok(());
    }
    let mut scheduled = 0_u64;
    let mut resident = 0_u64;
    let mut errors = 0_usize;
    let mut last_id = None;
    let mut opened: Option<File> = None;
    let page = page_size();
    let ordered = profile.warm_ranges();
    let mut index = 0;
    while index < ordered.len() {
        if *budget < page {
            break;
        }
        let range = ordered[index];
        index += 1;
        let record = &profile.files[range.file];
        let mut length = BLOCK.min(record.stamp.size - range.offset);
        // One mincore and fadvise pass for adjacent ranked blocks. This avoids
        // thousands of syscalls and 5 ms sleeps on large startup mappings.
        while index < ordered.len()
            && ordered[index].file == range.file
            && ordered[index].priority == range.priority
            && ordered[index].offset == range.offset + length
            && length < 2 * 1024 * 1024
        {
            let next = BLOCK.min(record.stamp.size - ordered[index].offset);
            length += next;
            index += 1;
        }
        if fs::metadata(&record.path)
            .map(|m| Stamp::from_meta(&m) != record.stamp)
            .unwrap_or(true)
        {
            profile.stale += 1;
            continue;
        }
        if last_id != Some(range.file) {
            opened = File::open(&record.path).ok();
            last_id = Some(range.file);
        }
        let Some(file) = opened.as_ref() else {
            errors += 1;
            continue;
        };
        if file
            .metadata()
            .map(|m| Stamp::from_meta(&m) != record.stamp)
            .unwrap_or(true)
        {
            profile.stale += 1;
            continue;
        }
        use std::os::fd::AsRawFd;
        let vec = match resident_pages(file.as_raw_fd(), range.offset, length as usize, page) {
            Ok(vec) => vec,
            Err(_) => {
                errors += 1;
                continue;
            }
        };
        let pages = vec.len();
        let mut p = 0;
        let mut advised = false;
        while p < pages && *budget >= page {
            while p < pages && vec[p] & 1 != 0 {
                resident += page;
                p += 1;
            }
            let start = p;
            while p < pages && vec[p] & 1 == 0 && ((p - start + 1) as u64) * page <= *budget {
                p += 1;
            }
            if p == start {
                break;
            }
            let offset = range.offset + start as u64 * page;
            let bytes = (p - start) as u64 * page;
            if memory_headroom(cfg)? < bytes {
                eprintln!("appwarm: stopping at low memory headroom");
                *budget = 0;
                break;
            }
            let result = advise(file.as_raw_fd(), offset, bytes);
            if let Err(error) = result {
                errors += 1;
                if debug {
                    eprintln!("appwarm: fadvise {}: {error}", record.path.display());
                }
            } else {
                if debug {
                    eprintln!(
                        "appwarm: queued {} +{offset} ({bytes} bytes)",
                        record.path.display()
                    );
                }
                scheduled += bytes;
                *budget -= bytes;
                advised = true;
            }
        }
        if advised {
            thread::sleep(Duration::from_millis(5));
        }
    }
    eprintln!("appwarm: {name} scheduled {:.1} MiB, already resident {:.1} MiB, stale {}, errors {errors}", scheduled as f64 / 1048576.0, resident as f64 / 1048576.0, profile.stale);
    if profile.stale > 0 {
        eprintln!("appwarm: run `appwarm learn {name}` to refresh changed files");
    }
    Ok(())
}
