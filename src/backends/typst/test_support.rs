//! Helpers shared by the typst backend's subprocess tests.

/// Linux-only headcount via `/proc` — good enough to catch a leaked child in
/// CI without pulling in a process-listing crate. Returns 0 (never a false
/// failure) on any other target.
pub(crate) fn count_processes_named(name: &str) -> usize {
    #[cfg(target_os = "linux")]
    {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return 0;
        };
        entries
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().parse::<u32>().is_ok())
            .filter(|e| {
                std::fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == name)
            })
            .count()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = name;
        0
    }
}
