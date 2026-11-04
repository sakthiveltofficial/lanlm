use super::*;

#[test]
fn recommended_context_grows_with_free_memory() {
    assert_eq!(recommended_ctx(0), 2048);
    assert_eq!(recommended_ctx(5_999), 2048);
    assert_eq!(recommended_ctx(6_000), 4096);
    assert_eq!(recommended_ctx(11_999), 4096);
    assert_eq!(recommended_ctx(12_000), 8192);
    assert_eq!(recommended_ctx(64_000), 8192);
}

#[test]
fn memory_figures_are_consistent() {
    let (total, avail) = memory_mb();
    assert!(total > 0, "any machine running the tests has some memory");
    assert!(
        avail <= total,
        "available memory cannot exceed total memory"
    );
}

#[test]
fn hardware_info_reports_sane_numbers() {
    let hw = hardware_info();
    assert!(hw.logical_cores >= 1);
    assert!(hw.physical_cores >= 1);
    assert_eq!(hw.rec_threads, hw.physical_cores);
    assert!(!hw.cpu.is_empty());
    assert!(hw.total_ram_mb >= hw.avail_ram_mb);
}
