use serde::Serialize;
use sysinfo::System;

const MB: u64 = 1024 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HwInfo {
    cpu: String,
    physical_cores: u32,
    logical_cores: u32,
    total_ram_mb: u64,
    avail_ram_mb: u64,
    /// Suggested llama.cpp thread count (physical cores).
    rec_threads: u32,
    /// Suggested context size for the memory this PC has free.
    rec_ctx: u32,
}

/// Total and available memory in MB, or `(0, 0)` when the operating system will not say.
/// Zero means "unknown": the interface hides fit badges and the planner treats it as no memory.
pub fn memory_mb() -> (u64, u64) {
    let Ok(mut sys) = System::new() else {
        return (0, 0);
    };
    sys.refresh_memory();
    (sys.total_memory() / MB, sys.available_memory() / MB)
}

/// Suggested context size for the memory this PC has free.
fn recommended_ctx(avail_mb: u64) -> u32 {
    if avail_mb >= 12_000 {
        8192
    } else if avail_mb >= 6_000 {
        4096
    } else {
        2048
    }
}

#[tauri::command]
pub fn hardware_info() -> HwInfo {
    let fallback_cores = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);
    let (total, avail) = memory_mb();

    let Ok(mut sys) = System::new() else {
        return HwInfo {
            cpu: "Unknown CPU".into(),
            physical_cores: fallback_cores,
            logical_cores: fallback_cores,
            total_ram_mb: total,
            avail_ram_mb: avail,
            rec_threads: fallback_cores,
            rec_ctx: recommended_ctx(avail),
        };
    };
    sys.refresh_cpu_all();

    let logical = (sys.cpus().len() as u32).max(1);
    let physical = System::physical_core_count()
        .map(|n| n as u32)
        .unwrap_or(logical)
        .max(1);
    let cpu = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "Unknown CPU".into());

    HwInfo {
        cpu,
        physical_cores: physical,
        logical_cores: logical,
        total_ram_mb: total,
        avail_ram_mb: avail,
        rec_threads: physical,
        rec_ctx: recommended_ctx(avail),
    }
}

#[cfg(test)]
mod tests;
