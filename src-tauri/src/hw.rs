use serde::Serialize;
use sysinfo::System;

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

#[tauri::command]
pub fn hardware_info() -> HwInfo {
    let mut sys = System::new();
    sys.refresh_memory();
    sys.refresh_cpu_all();

    let logical = sys.cpus().len().max(1) as u32;
    let physical = sys.physical_core_count().unwrap_or(logical as usize).max(1) as u32;
    let total = sys.total_memory() / (1024 * 1024);
    let avail = sys.available_memory() / (1024 * 1024);
    let cpu = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .unwrap_or_else(|| "Unknown CPU".into());

    let rec_ctx = if avail >= 12_000 {
        8192
    } else if avail >= 6_000 {
        4096
    } else {
        2048
    };

    HwInfo {
        cpu,
        physical_cores: physical,
        logical_cores: logical,
        total_ram_mb: total,
        avail_ram_mb: avail,
        rec_threads: physical,
        rec_ctx,
    }
}
