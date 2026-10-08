use crate::config::Config;
use std::ffi::{c_int, c_long, c_void};
use std::fs;
use std::io;

const FADV_WILLNEED: c_int = 3;
const MAP_PRIVATE: c_int = 2;
const IOPRIO_CLASS_IDLE: c_int = 3;
#[cfg(target_arch = "x86_64")]
const SYS_IOPRIO_SET: c_long = 251;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
const SYS_IOPRIO_SET: c_long = 30;

unsafe extern "C" {
    fn setpriority(which: c_int, who: u32, prio: c_int) -> c_int;
    fn syscall(number: c_long, ...) -> c_long;
    fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn mincore(addr: *mut c_void, len: usize, vec: *mut u8) -> c_int;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
    fn getpagesize() -> c_int;
    fn kill(pid: c_int, signal: c_int) -> c_int;
    fn posix_fadvise(fd: c_int, offset: i64, len: i64, advice: c_int) -> c_int;
}

pub fn memory_headroom(cfg: &Config) -> io::Result<u64> {
    let text = fs::read_to_string("/proc/meminfo")?;
    let mut total = 0_u64;
    let mut available = 0_u64;
    for line in text.lines() {
        if let Some(kb) = line
            .strip_prefix("MemTotal:")
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse::<u64>().ok())
        {
            total = kb * 1024;
        }
        if let Some(kb) = line
            .strip_prefix("MemAvailable:")
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse::<u64>().ok())
        {
            available = kb * 1024;
        }
    }
    if total == 0 || available == 0 {
        return Err(io::Error::other("/proc/meminfo lacks memory totals"));
    }
    let guard = (cfg.min_available_mib * 1024 * 1024).max(total / 10);
    Ok(available.saturating_sub(guard))
}

pub fn lower_priority() -> io::Result<()> {
    if unsafe { setpriority(0, 0, 19) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let priority = IOPRIO_CLASS_IDLE << 13;
    if unsafe { syscall(SYS_IOPRIO_SET, 1, 0, priority) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn stop_tracer(pid: i32) {
    unsafe {
        kill(pid, 15);
    }
}

pub fn page_size() -> u64 {
    unsafe { getpagesize() }.max(4096) as u64
}

pub fn resident_pages(fd: i32, offset: u64, length: usize, page: u64) -> io::Result<Vec<u8>> {
    let map = unsafe {
        mmap(
            std::ptr::null_mut(),
            length,
            0,
            MAP_PRIVATE,
            fd,
            offset as i64,
        )
    };
    if map as isize == -1 {
        return Err(io::Error::last_os_error());
    }
    let mut vec = vec![0_u8; (length as u64).div_ceil(page) as usize];
    let status = unsafe { mincore(map, length, vec.as_mut_ptr()) };
    unsafe {
        munmap(map, length);
    }
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(vec)
}

pub fn advise(fd: i32, offset: u64, bytes: u64) -> io::Result<()> {
    let error = unsafe { posix_fadvise(fd, offset as i64, bytes as i64, FADV_WILLNEED) };
    if error != 0 {
        Err(io::Error::from_raw_os_error(error))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_guard_can_disable_warming() {
        let cfg = Config {
            min_available_mib: 1_048_576,
            ..Config::default()
        };
        assert_eq!(memory_headroom(&cfg).unwrap(), 0);
    }
}
