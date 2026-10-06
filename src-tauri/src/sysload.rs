//! Machine load for the overlay footer: CPU %, RAM used, swap, and the app eating the most.
//!
//! CPU percentages are deltas between two refreshes, so one `System` lives for the whole app run
//! and each overlay poll measures the interval since the previous one.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use sysinfo::{
    CpuRefreshKind, MemoryRefreshKind, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System,
    UpdateKind,
};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppLoad {
    /// App bundle name (`1DevTool`), or the process name for command-line tools (`node`).
    pub name: String,
    /// Resident memory summed over all its processes (helpers, renderers…), bytes.
    pub memory: u64,
    /// Activity Monitor style: 100 = one full core, so it can exceed 100.
    pub cpu_percent: f32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemLoad {
    /// Whole machine, 0..100.
    pub cpu_percent: f32,
    pub cores: usize,
    /// 1-minute load average — compare against `cores`.
    pub load_one: f64,
    /// What Activity Monitor calls "Memory Used" (app + wired + compressed), bytes.
    pub memory_used: u64,
    pub memory_total: u64,
    pub swap_used: u64,
    /// The app holding the most memory.
    pub top_memory: Option<AppLoad>,
    /// The app burning the most CPU.
    pub top_cpu: Option<AppLoad>,
}

fn system() -> &'static Mutex<System> {
    static SYSTEM: OnceLock<Mutex<System>> = OnceLock::new();
    SYSTEM.get_or_init(|| {
        Mutex::new(System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
                .with_memory(MemoryRefreshKind::everything()),
        ))
    })
}

/// Electron/Chromium apps run as dozens of `… Helper` processes nested inside the main bundle,
/// so group by the outermost `.app` in the executable path; plain binaries keep their own name.
fn app_name(exe: Option<&Path>, process_name: &str) -> String {
    exe.and_then(|path| {
        path.components().find_map(|part| {
            part.as_os_str()
                .to_str()
                .and_then(|name| name.strip_suffix(".app"))
                .map(str::to_string)
        })
    })
    .unwrap_or_else(|| process_name.to_string())
}

pub fn read() -> SystemLoad {
    let mut sys = system().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    sys.refresh_cpu_usage();
    sys.refresh_memory();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_memory()
            .with_cpu()
            .with_exe(UpdateKind::OnlyIfNotSet),
    );

    let mut apps: HashMap<String, AppLoad> = HashMap::new();
    for process in sys.processes().values() {
        let name = app_name(process.exe(), &process.name().to_string_lossy());
        let entry = apps.entry(name.clone()).or_insert(AppLoad {
            name,
            memory: 0,
            cpu_percent: 0.0,
        });
        entry.memory += process.memory();
        entry.cpu_percent += process.cpu_usage();
    }
    let top_memory = apps.values().max_by_key(|app| app.memory).cloned();
    let top_cpu = apps
        .values()
        .max_by(|a, b| a.cpu_percent.total_cmp(&b.cpu_percent))
        .cloned();

    SystemLoad {
        cpu_percent: sys.global_cpu_usage(),
        cores: sys.cpus().len(),
        load_one: System::load_average().one,
        memory_used: sys.used_memory(),
        memory_total: sys.total_memory(),
        swap_used: sys.used_swap(),
        top_memory,
        top_cpu,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_helpers_under_their_outer_app() {
        let exe = Path::new(
            "/Applications/1DevTool.app/Contents/Frameworks/1DevTool Helper (Renderer).app/Contents/MacOS/1DevTool Helper (Renderer)",
        );
        assert_eq!(app_name(Some(exe), "1DevTool Helper (Renderer)"), "1DevTool");
        assert_eq!(app_name(Some(Path::new("/opt/homebrew/bin/node")), "node"), "node");
        assert_eq!(app_name(None, "kernel_task"), "kernel_task");
    }
}
