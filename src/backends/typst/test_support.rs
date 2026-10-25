//! Helpers shared by the typst backend's subprocess tests.

/// Linux-only headcount via `/proc` — good enough to catch a leaked child in
/// CI without pulling in a process-listing crate. Returns 0 (never a false
/// failure) on any other target.
///
/// Only *direct children of this test process* count: a machine-wide count
/// flaked whenever an unrelated `sleep`/`typst` came or went mid-test.
/// `process::run_bounded` spawns the program itself (no shell in between),
/// and an unreaped zombie keeps its ppid, so a missing reap is still caught.
pub(crate) fn count_child_processes_named(name: &str) -> usize {
    #[cfg(target_os = "linux")]
    {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return 0;
        };
        let me = std::process::id();
        entries
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().parse::<u32>().is_ok())
            .filter(|e| {
                std::fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == name)
            })
            .filter(|e| {
                std::fs::read_to_string(e.path().join("stat"))
                    .is_ok_and(|stat| parent_pid(&stat) == Some(me))
            })
            .count()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = name;
        0
    }
}

/// `comm` (field 2 of `/proc/<pid>/stat`) may itself contain spaces and `)`,
/// so the ppid is located after the *last* `)`: state is the first field
/// there, ppid the second.
#[cfg(target_os = "linux")]
fn parent_pid(stat: &str) -> Option<u32> {
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(target_os = "linux")]
mod tests {
    use super::*;

    #[test]
    fn parent_pid_skips_a_comm_holding_spaces_and_parens() {
        assert_eq!(parent_pid("42 (a) b (c)) S 7 42 42 0"), Some(7));
        assert_eq!(parent_pid("garbage"), None);
    }

    #[test]
    fn a_spawned_child_is_ours() {
        // `true`, not `sleep`: a sibling test counts `sleep` children. An
        // exited child stays readable as a zombie until `wait`.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", child.id())).unwrap();
        child.wait().unwrap();
        assert_eq!(parent_pid(&stat), Some(std::process::id()));
    }
}
