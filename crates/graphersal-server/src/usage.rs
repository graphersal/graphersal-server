//! What the server uses (round 3 B5): the size of a database on disk, the process's resident
//! memory and the machine's memory, and the memory summary of `GET /api/admin/server`
//! ([`memory_json`]).
//!
//! **Disk** ([`disk_bytes`]): the bytes of every file in a database's directory (Store
//! snapshots, journal, models, settings), walked by [`crate::layout::dir_bytes`] and cached per
//! directory for [`DISK_TTL`], so a listing read again and again (every tab visit, every
//! focus) walks a directory at most once per TTL. A Store directory holds a handful of files,
//! so one walk is cheap; the cache bounds the cost of many databases and many browsers.
//!
//! **Process** ([`process_resident_bytes`]): the resident set size (RSS) of this process, read
//! from the OS without a new dependency: Linux `/proc/self/statm` (resident pages times the
//! page size), macOS `proc_pidinfo(PROC_PIDTASKINFO)` (`pti_resident_size`, through `libc`),
//! Windows `GetProcessMemoryInfo` (`WorkingSetSize`, through `windows-sys`, already in the
//! tree). Other platforms: `None` (`null` in JSON).
//!
//! **Machine** ([`machine_memory`]): the physical memory and what is available: Linux
//! `/proc/meminfo` (`MemTotal`, `MemAvailable`), macOS `sysctl hw.memsize` and
//! `host_statistics64(HOST_VM_INFO64)` (free + inactive + purgeable pages: what the system
//! gives back without swapping, an approximation like `vm_stat`'s), Windows
//! `GlobalMemoryStatusEx` (`ullTotalPhys`, `ullAvailPhys`). Inside a container these are the
//! host's figures; the engine's ceiling (below) follows a cgroup limit.
//!
//! **The memory summary** (`memory` of `GET /api/admin/server`, and `graphersal-server
//! status`): `processBytes` (RSS), `liveBytes` (the engine's tracked live heap: `null` in this
//! binary, which installs no tracking allocator), `databasesBytes` (the loaded graphs, each
//! once: [`crate::loader::Loader::memory`]), `machineTotalBytes`, `machineAvailableBytes`,
//! `limitBytes` (the engine's process memory ceiling in effect,
//! `graphersal::exec::memory_room().limit()`: a query, import or write that would take the
//! process over it is refused; `null` = no ceiling), `limitSource` (`configured` | `cgroup` |
//! `physical` | `none` | `unknown`: where the ceiling comes from), `configuredLimit`
//! (`server.toml [limits] memory_limit` as written: a number of bytes, `"auto"`, `"none"`, or
//! `null` when not set, which is the engine default `auto`), `headroomBytes` (`[limits]
//! memory_headroom`, `null` = the default: a tenth of a cgroup limit, at least 64 MiB),
//! `availableBytes` (the engine's room below the ceiling: the whole ceiling without a tracking
//! allocator), `freeBytes` + `freeBasis`: "free under the limit" = `limitBytes` minus the
//! memory used (`processBytes`, else `databasesBytes`) when a ceiling is in effect
//! (`freeBasis: "limit"`), else `machineAvailableBytes` (`"machine"`), `null` when neither is
//! known.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use graphersal::exec::MemoryLimit;
use serde_json::{Value, json};

/// How long a database directory's measured size is reused ([`disk_bytes`]).
pub(crate) const DISK_TTL: Duration = Duration::from_secs(5);

/// The cache of [`disk_bytes`]: directory -> (when measured, bytes).
fn disk_cache() -> &'static Mutex<HashMap<PathBuf, (Instant, u64)>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, (Instant, u64)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The bytes of every file below the database directory `dir` (module docs), measured at most
/// once per [`DISK_TTL`].
pub(crate) fn disk_bytes(dir: &Path) -> u64 {
    let now = Instant::now();
    if let Ok(cache) = disk_cache().lock()
        && let Some((at, bytes)) = cache.get(dir)
        && now.duration_since(*at) < DISK_TTL
    {
        return *bytes;
    }
    // Walked without the cache's lock: two requests may walk the same directory at once, never
    // one waiting for another's walk.
    let bytes = crate::layout::dir_bytes(dir);
    if let Ok(mut cache) = disk_cache().lock() {
        cache.retain(|_, (at, _)| now.duration_since(*at) < DISK_TTL);
        cache.insert(dir.to_path_buf(), (now, bytes));
    }
    bytes
}

/// The machine's memory: total and available bytes (module docs), each `None` when unknown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MachineMemory {
    /// The physical memory.
    pub(crate) total: Option<u64>,
    /// What the system can give without swapping.
    pub(crate) available: Option<u64>,
}

/// The process's resident set size in bytes (module docs); `None` where it cannot be read.
pub(crate) fn process_resident_bytes() -> Option<u64> {
    platform::process_resident_bytes()
}

/// The machine's memory (module docs).
pub(crate) fn machine_memory() -> MachineMemory {
    platform::machine_memory()
}

/// The memory settings of `server.toml`'s `[limits]` as the server applied them at start.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MemorySettings {
    /// `memory_limit` (`None`: not set, the engine default `auto`).
    pub(crate) limit: Option<MemoryLimit>,
    /// `memory_headroom` (`None`: not set).
    pub(crate) headroom: Option<u64>,
}

/// The configured `memory_limit` as JSON: bytes, `"auto"`, `"none"` or `null`.
pub(crate) fn configured_limit(limit: Option<MemoryLimit>) -> Value {
    match limit {
        None => Value::Null,
        Some(MemoryLimit::Bytes(bytes)) => json!(bytes),
        Some(MemoryLimit::Auto) => json!("auto"),
        Some(MemoryLimit::Unlimited) => json!("none"),
        // A newer engine's kind: its own words (the engine's `Display`, its config spelling).
        Some(other) => json!(other.to_string()),
    }
}

/// The figures of the memory summary (module docs), measured now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MemoryFigures {
    /// The process's RSS.
    pub(crate) process: Option<u64>,
    /// The loaded databases, each graph once.
    pub(crate) databases: u64,
    /// The machine's memory.
    pub(crate) machine: MachineMemory,
}

impl MemoryFigures {
    /// The figures now, with `databases` (the loader's sum).
    pub(crate) fn now(databases: u64) -> Self {
        Self {
            process: process_resident_bytes(),
            databases,
            machine: machine_memory(),
        }
    }
}

/// "Free under the limit" (module docs): `(bytes, basis)`.
pub(crate) fn free_bytes(
    limit: Option<u64>,
    figures: &MemoryFigures,
) -> (Option<u64>, &'static str) {
    match (limit, figures.machine.available) {
        (Some(limit), available) => {
            let used = figures.process.unwrap_or(figures.databases);
            let under = limit.saturating_sub(used);
            match available {
                Some(available) if available < under => (Some(available), "machine-below-limit"),
                _ => (Some(under), "limit"),
            }
        }
        (None, available) => (available, "machine"),
    }
}

/// The `memory` object of `GET /api/admin/server` (module docs).
pub(crate) fn memory_json(settings: Option<&MemorySettings>, figures: &MemoryFigures) -> Value {
    let room = graphersal::exec::memory_room().ok();
    let limit = room.as_ref().and_then(|r| r.limit());
    let (free, basis) = free_bytes(limit, figures);
    json!({
        "processBytes": figures.process,
        "liveBytes": room.as_ref().and_then(|r| r.live_bytes()),
        "databasesBytes": figures.databases,
        "machineTotalBytes": figures.machine.total,
        "machineAvailableBytes": figures.machine.available,
        "limitBytes": limit,
        "limitSource": room.as_ref().map(|r| r.source().as_str()),
        "configuredLimit": configured_limit(settings.and_then(|s| s.limit)),
        "headroomBytes": settings.and_then(|s| s.headroom),
        "availableBytes": room.as_ref().and_then(|r| r.available()),
        "freeBytes": free,
        "freeBasis": basis,
    })
}

#[cfg(target_os = "linux")]
mod platform {
    use super::MachineMemory;

    pub(super) fn process_resident_bytes() -> Option<u64> {
        let text = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = text.split_whitespace().nth(1)?.parse().ok()?;
        Some(pages.saturating_mul(page_size()?))
    }

    fn page_size() -> Option<u64> {
        // SAFETY: `sysconf` takes an integer and touches no memory of ours.
        let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        u64::try_from(size).ok().filter(|s| *s > 0)
    }

    pub(super) fn machine_memory() -> MachineMemory {
        std::fs::read_to_string("/proc/meminfo")
            .map(|text| super::parse_meminfo(&text))
            .unwrap_or_default()
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::MachineMemory;

    pub(super) fn process_resident_bytes() -> Option<u64> {
        let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
        let size = i32::try_from(std::mem::size_of::<libc::proc_taskinfo>()).ok()?;
        // SAFETY: `info` is a writable buffer of `size` bytes, the size `proc_pidinfo` expects
        // for `PROC_PIDTASKINFO`; it writes at most that many bytes and answers how many.
        let written = unsafe {
            libc::proc_pidinfo(
                i32::try_from(std::process::id()).ok()?,
                libc::PROC_PIDTASKINFO,
                0,
                info.as_mut_ptr().cast::<libc::c_void>(),
                size,
            )
        };
        if written != size {
            return None;
        }
        // SAFETY: the call filled the whole struct (checked above); zeroed before anyway.
        Some(unsafe { info.assume_init() }.pti_resident_size)
    }

    pub(super) fn machine_memory() -> MachineMemory {
        MachineMemory {
            total: total(),
            available: available(),
        }
    }

    fn total() -> Option<u64> {
        let mut bytes: u64 = 0;
        let mut size = std::mem::size_of::<u64>();
        // SAFETY: the name is a NUL-terminated C string; `bytes` is a writable u64 and `size`
        // holds its size, as "hw.memsize" (a 64-bit integer) requires; nothing is set.
        let status = unsafe {
            libc::sysctlbyname(
                c"hw.memsize".as_ptr(),
                (&raw mut bytes).cast::<libc::c_void>(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        (status == 0 && bytes > 0).then_some(bytes)
    }

    fn available() -> Option<u64> {
        let mut stats = std::mem::MaybeUninit::<libc::vm_statistics64>::zeroed();
        let mut count = libc::HOST_VM_INFO64_COUNT;
        // SAFETY: `mach_host_self` takes no arguments (its port is not released: the
        // process keeps one send right per host port, as every caller of it does).
        #[allow(deprecated)]
        let host = unsafe { libc::mach_host_self() };
        // SAFETY: `stats` is a writable `vm_statistics64` and `count` its size in integers
        // (`HOST_VM_INFO64_COUNT`); the kernel writes at most `count` integers and updates it.
        let status = unsafe {
            libc::host_statistics64(
                host,
                libc::HOST_VM_INFO64,
                stats.as_mut_ptr().cast::<libc::integer_t>(),
                &mut count,
            )
        };
        if status != libc::KERN_SUCCESS {
            return None;
        }
        // SAFETY: zeroed first; the kernel filled the fields it knows.
        let stats = unsafe { stats.assume_init() };
        // SAFETY: `sysconf` takes an integer and touches no memory of ours.
        let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
        let pages = u64::from(stats.free_count)
            + u64::from(stats.inactive_count)
            + u64::from(stats.purgeable_count);
        Some(pages.saturating_mul(page))
    }
}

#[cfg(windows)]
mod platform {
    use super::MachineMemory;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    pub(super) fn process_resident_bytes() -> Option<u64> {
        // SAFETY: a zeroed PROCESS_MEMORY_COUNTERS is valid (plain integers).
        let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
        let size = u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS>()).ok()?;
        counters.cb = size;
        // SAFETY: the pseudo handle of the current process needs no closing; `counters` is a
        // writable struct of `size` bytes.
        let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) };
        (ok != 0).then(|| u64::try_from(counters.WorkingSetSize).unwrap_or(u64::MAX))
    }

    pub(super) fn machine_memory() -> MachineMemory {
        // SAFETY: a zeroed MEMORYSTATUSEX is valid (plain integers).
        let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        status.dwLength = u32::try_from(std::mem::size_of::<MEMORYSTATUSEX>()).unwrap_or(0);
        // SAFETY: `status` is a writable MEMORYSTATUSEX with its `dwLength` set, as required.
        let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
        if ok == 0 {
            return MachineMemory::default();
        }
        MachineMemory {
            total: Some(status.ullTotalPhys),
            available: Some(status.ullAvailPhys),
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod platform {
    use super::MachineMemory;

    pub(super) fn process_resident_bytes() -> Option<u64> {
        None
    }

    pub(super) fn machine_memory() -> MachineMemory {
        MachineMemory::default()
    }
}

/// `MemTotal` and `MemAvailable` of a `/proc/meminfo` text (kB lines), in bytes.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
fn parse_meminfo(text: &str) -> MachineMemory {
    let field = |name: &str| -> Option<u64> {
        text.lines().find_map(|line| {
            let rest = line.strip_prefix(name)?.strip_prefix(':')?;
            let kb: u64 = rest.trim().strip_suffix("kB")?.trim().parse().ok()?;
            Some(kb.saturating_mul(1024))
        })
    };
    MachineMemory {
        total: field("MemTotal"),
        available: field("MemAvailable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meminfo_is_parsed_in_bytes() {
        let text = "MemTotal:       16314772 kB\nMemFree:  1 kB\nMemAvailable:    8123456 kB\n";
        assert_eq!(
            parse_meminfo(text),
            MachineMemory {
                total: Some(16_314_772 * 1024),
                available: Some(8_123_456 * 1024),
            }
        );
        assert_eq!(parse_meminfo("garbage"), MachineMemory::default());
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn the_process_and_the_machine_have_memory() {
        let rss = process_resident_bytes().unwrap();
        let machine = machine_memory();
        let total = machine.total.unwrap();
        let available = machine.available.unwrap();
        assert!(rss > 1024 * 1024, "rss {rss}");
        assert!(rss < total, "rss {rss} total {total}");
        assert!(available <= total, "available {available} total {total}");
    }

    #[test]
    fn free_is_the_limit_minus_the_used_memory_capped_by_the_machine_available() {
        let figures = MemoryFigures {
            process: Some(300),
            databases: 100,
            machine: MachineMemory {
                total: Some(10_000),
                available: Some(4_000),
            },
        };
        assert_eq!(free_bytes(Some(1_000), &figures), (Some(700), "limit"));
        assert_eq!(free_bytes(Some(200), &figures), (Some(0), "limit"));
        assert_eq!(free_bytes(None, &figures), (Some(4_000), "machine"));
        let no_rss = MemoryFigures {
            process: None,
            ..figures
        };
        assert_eq!(free_bytes(Some(1_000), &no_rss), (Some(900), "limit"));
        // Never more than the machine has available.
        assert_eq!(
            free_bytes(Some(9_000), &figures),
            (Some(4_000), "machine-below-limit")
        );
        let unknown_machine = MemoryFigures {
            machine: MachineMemory::default(),
            ..figures
        };
        assert_eq!(
            free_bytes(Some(9_000), &unknown_machine),
            (Some(8_700), "limit")
        );
        assert_eq!(free_bytes(None, &unknown_machine), (None, "machine"));
    }

    #[test]
    fn the_configured_limit_reads_as_written() {
        assert_eq!(configured_limit(None), Value::Null);
        assert_eq!(configured_limit(Some(MemoryLimit::Bytes(5))), json!(5));
        assert_eq!(configured_limit(Some(MemoryLimit::Auto)), json!("auto"));
        assert_eq!(
            configured_limit(Some(MemoryLimit::Unlimited)),
            json!("none")
        );
    }

    #[test]
    fn a_directory_size_is_cached_for_the_ttl() {
        let temp = crate::test_support::TempDir::new("usage");
        let dir = temp.path().join("db");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a"), [0u8; 100]).unwrap();
        assert_eq!(disk_bytes(&dir), 100);
        std::fs::write(dir.join("b"), [0u8; 50]).unwrap();
        // Within the TTL: the cached figure.
        assert_eq!(disk_bytes(&dir), 100);
        assert_eq!(crate::layout::dir_bytes(&dir), 150);
    }
}
