use crate::config::Config;
use crate::linux::page_size;
use crate::profile::Profile;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const MAX_PAGES: usize = 1_000_000;
const CHUNK_PAGES: usize = 4096;
const PRESENT: u64 = 1 << 63;
const FILE_PAGE: u64 = 1 << 61;

// A pagemap entry's present and file bits are available to the owner of a
// process on modern kernels even when its physical frame number is hidden.
// This marks pages actually mapped into an app, not every byte in a VMA.
pub fn mark_present_pages(
    pids: &HashSet<i32>,
    profile: &mut Profile,
    cfg: &Config,
    debug: bool,
) -> usize {
    let page = page_size();
    let mut pids: Vec<_> = pids.iter().copied().collect();
    pids.sort_unstable();
    let mut examined = 0;
    let mut present = 0;
    let mut first_seq = HashMap::new();
    for range in profile.ranges.values() {
        let path = &profile.files[range.file].path;
        first_seq
            .entry(path.clone())
            .and_modify(|seq: &mut u32| *seq = (*seq).min(range.first_seq))
            .or_insert(range.first_seq);
    }
    for pid in pids {
        if examined >= MAX_PAGES {
            break;
        }
        let maps = match fs::read_to_string(format!("/proc/{pid}/maps")) {
            Ok(text) => text,
            Err(_) => continue,
        };
        let mut pagemap = match File::open(format!("/proc/{pid}/pagemap")) {
            Ok(file) => file,
            Err(error) => {
                if debug {
                    eprintln!("appwarm: pagemap unavailable for pid {pid}: {error}");
                }
                continue;
            }
        };
        for line in maps.lines() {
            if examined >= MAX_PAGES {
                break;
            }
            let mut fields = line.split_whitespace();
            let (Some(addresses), Some(_perms), Some(offset)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let Some(path_at) = line.find('/') else {
                continue;
            };
            let path = &line[path_at..];
            if path.ends_with(" (deleted)") || !Path::new(path).is_absolute() {
                continue;
            }
            let sequence = fs::canonicalize(path)
                .ok()
                .and_then(|path| first_seq.get(&path).copied())
                .unwrap_or(u32::MAX - 1);
            let Some((start, end)) = addresses.split_once('-').and_then(|(a, b)| {
                Some((
                    u64::from_str_radix(a, 16).ok()?,
                    u64::from_str_radix(b, 16).ok()?,
                ))
            }) else {
                continue;
            };
            let Ok(file_offset) = u64::from_str_radix(offset, 16) else {
                continue;
            };
            if start >= end || start % page != 0 || file_offset % page != 0 {
                continue;
            }
            let pages = ((end - start) / page) as usize;
            let mut at = 0;
            while at < pages && examined < MAX_PAGES {
                let n = CHUNK_PAGES.min(pages - at).min(MAX_PAGES - examined);
                let Some(byte_offset) = ((start / page) + at as u64).checked_mul(8) else {
                    break;
                };
                if pagemap.seek(SeekFrom::Start(byte_offset)).is_err() {
                    break;
                }
                let mut entries = vec![0_u8; n * 8];
                if pagemap.read_exact(&mut entries).is_err() {
                    break;
                }
                for (i, entry) in entries.as_chunks::<8>().0.iter().enumerate() {
                    let bits = u64::from_ne_bytes(*entry);
                    if bits & (PRESENT | FILE_PAGE) == PRESENT | FILE_PAGE {
                        profile.mark(
                            Path::new(path),
                            file_offset + (at + i) as u64 * page,
                            page,
                            5,
                            sequence,
                            cfg,
                        );
                        present += 1;
                    }
                }
                examined += n;
                at += n;
            }
        }
    }
    present
}
