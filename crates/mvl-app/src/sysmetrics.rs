//! Process memory metrics for the status bar (Phase 7.2d).
//!
//! Honest scope: this module reports the process's resident set size
//! (RSS) only. Per-platform:
//! - Linux: `/proc/self/statm` (std-only, no unsafe). Pages are 4 KiB on
//!   every desktop target Rust ships (x86_64/aarch64); disclosed.
//! - macOS: `task_info(MACH_TASK_BASIC_INFO)` via libc + `mach2` (the
//!   maintained Mach shim libc's deprecation note points to) — small FFI
//!   confined to this module.
//! - Windows: `GetProcessMemoryInfo` via `windows-sys` (same confinement).
//!
//! `None` means "unknown on this platform" — the status bar hides the
//! segment rather than showing a placeholder (No-Fake-UI).

#![allow(unsafe_code)]

/// Resident set size in bytes, or `None` when unavailable.
pub fn resident_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        linux_rss()
    }
    #[cfg(target_os = "macos")]
    {
        macos_rss()
    }
    #[cfg(target_os = "windows")]
    {
        windows_rss()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn linux_rss() -> Option<u64> {
    // statm field 1 = resident pages (field 0 = total program size).
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let resident_pages = statm.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    Some(resident_pages.saturating_mul(4096))
}

#[cfg(target_os = "macos")]
fn macos_rss() -> Option<u64> {
    use std::mem::{size_of, zeroed};
    // mach_task_basic_info_data_t (flavour 20): sizes in natural units.
    #[repr(C)]
    struct MachTaskBasicInfo {
        virtual_size: u64,
        resident_size: u64,
        resident_size_max: u64,
        user_time_sec: i32,
        user_time_usec: i32,
        system_time_sec: i32,
        system_time_usec: i32,
        policy: i32,
        suspend_count: i32,
    }
    unsafe {
        // mach2 replaces libc's deprecated mach_task_self binding.
        let task = mach2::traps::mach_task_self();
        let mut info: MachTaskBasicInfo = zeroed();
        let mut count = (size_of::<MachTaskBasicInfo>() / size_of::<u32>()) as u32;
        // MACH_TASK_BASIC_INFO == 20; KERN_SUCCESS == 0.
        let kr = libc::task_info(
            task,
            20,
            &mut info as *mut MachTaskBasicInfo as *mut i32,
            &mut count,
        );
        (kr == 0).then_some(info.resident_size)
    }
}

#[cfg(target_os = "windows")]
fn windows_rss() -> Option<u64> {
    use std::mem::{size_of, zeroed};
    unsafe {
        let mut pmc: windows_sys::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS = zeroed();
        pmc.cb =
            size_of::<windows_sys::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS>() as u32;
        let handle = windows_sys::Win32::System::Threading::GetCurrentProcess();
        let ok = windows_sys::Win32::System::ProcessStatus::GetProcessMemoryInfo(
            handle, &mut pmc, pmc.cb,
        );
        (ok != 0).then_some(pmc.WorkingSetSize as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rss_reads_or_hides_honestly() {
        // On Linux this must read a sane value; on other platforms the
        // contract is only "Some sane value or None".
        if let Some(bytes) = resident_bytes() {
            assert!(bytes > 0, "rss must be positive");
            // A Rust process is at least a few MB and never PB-scale.
            assert!(bytes > 1_000_000 && bytes < 1 << 40, "rss {bytes}");
        }
    }
}
