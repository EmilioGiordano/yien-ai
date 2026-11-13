//! Which known program runs under a shell, read from the process tree, so a
//! shell pane's head can name the agent CLI typed in it (RF-PAN-12).
//!
//! One snapshot of the processes answers for every shell. Only Windows reads
//! the tree; elsewhere nothing is detected.
//!
//! ```no_run
//! let found = yien_platform::programs::programs_under(&[4242]);
//! assert_eq!(found.len(), 1);
//! ```

#![allow(unsafe_code)]

use std::collections::HashMap;

pub use yien_proto::KNOWN_PROGRAMS;

/// How far below the shell a known program is looked for: an npm shim runs
/// `cmd /c`, then node, then the CLI's own executable.
const DEPTH: usize = 3;

/// The known program an executable name means, if any: case does not matter,
/// a folder and a `.exe` are dropped.
///
/// ```
/// use yien_platform::programs::known_program;
///
/// assert_eq!(known_program("Claude.EXE"), Some("claude"));
/// assert_eq!(known_program("node.exe"), None);
/// ```
pub fn known_program(exe: &str) -> Option<&'static str> {
    let file = exe.rsplit(['/', '\\']).next().unwrap_or(exe);
    let stem = match file.len().checked_sub(4) {
        Some(at) if file.is_char_boundary(at) && file[at..].eq_ignore_ascii_case(".exe") => {
            &file[..at]
        }
        _ => file,
    };
    KNOWN_PROGRAMS
        .iter()
        .copied()
        .find(|known| known.eq_ignore_ascii_case(stem))
}

/// One process in a snapshot.
///
/// ```
/// let p = yien_platform::programs::Process { pid: 9, parent: 1, exe: "codex.exe".into() };
/// assert_eq!(p.parent, 1);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    /// Its id.
    pub pid: u32,
    /// The id of the process that started it.
    pub parent: u32,
    /// Its executable's file name: `claude.exe`.
    pub exe: String,
}

/// The processes of a snapshot, by parent.
///
/// ```
/// let tree = yien_platform::programs::ProcessTree::new(&[]);
/// assert_eq!(tree.program_under(1), None);
/// ```
#[derive(Debug, Default)]
pub struct ProcessTree<'a> {
    children: HashMap<u32, Vec<&'a Process>>,
}

impl<'a> ProcessTree<'a> {
    /// Indexes `processes` by parent.
    ///
    /// ```
    /// use yien_platform::programs::{Process, ProcessTree};
    ///
    /// let all = [Process { pid: 9, parent: 1, exe: "codex.exe".into() }];
    /// assert_eq!(ProcessTree::new(&all).program_under(1), Some("codex"));
    /// ```
    pub fn new(processes: &'a [Process]) -> Self {
        let mut children: HashMap<u32, Vec<&Process>> = HashMap::new();
        for p in processes.iter().filter(|p| p.pid != p.parent) {
            children.entry(p.parent).or_default().push(p);
        }
        Self { children }
    }

    /// The known program closest below `shell`, a few levels deep at most;
    /// `None` when the shell runs nothing known, or has no child.
    ///
    /// ```
    /// use yien_platform::programs::{Process, ProcessTree};
    ///
    /// let all = [
    ///     Process { pid: 9, parent: 1, exe: "node.exe".into() },
    ///     Process { pid: 10, parent: 9, exe: "opencode.exe".into() },
    /// ];
    /// assert_eq!(ProcessTree::new(&all).program_under(1), Some("opencode"));
    /// ```
    pub fn program_under(&self, shell: u32) -> Option<&'static str> {
        let mut level = vec![shell];
        for _ in 0..DEPTH {
            let below: Vec<&Process> = level
                .iter()
                .filter_map(|pid| self.children.get(pid))
                .flatten()
                .copied()
                .collect();
            if let Some(found) = below.iter().find_map(|p| known_program(&p.exe)) {
                return Some(found);
            }
            level = below.iter().map(|p| p.pid).collect();
        }
        None
    }
}

/// The known program under each shell in `shells`, in the same order, from
/// one snapshot. All `None` when the snapshot fails or off Windows.
///
/// ```no_run
/// let found = yien_platform::programs::programs_under(&[4242, 4343]);
/// assert_eq!(found.len(), 2);
/// ```
pub fn programs_under(shells: &[u32]) -> Vec<Option<&'static str>> {
    let processes = if shells.is_empty() {
        Vec::new()
    } else {
        snapshot()
    };
    let tree = ProcessTree::new(&processes);
    shells.iter().map(|s| tree.program_under(*s)).collect()
}

#[cfg(not(windows))]
fn snapshot() -> Vec<Process> {
    Vec::new()
}

#[cfg(windows)]
fn snapshot() -> Vec<Process> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    struct Snapshot(HANDLE);
    impl Drop for Snapshot {
        fn drop(&mut self) {
            // SAFETY: the handle came from CreateToolhelp32Snapshot, is valid
            // and is closed only here.
            unsafe { CloseHandle(self.0) };
        }
    }

    // SAFETY: no pointers are passed; the result is checked below.
    let handle = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Vec::new();
    }
    let snapshot = Snapshot(handle);
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut processes = Vec::new();
    // SAFETY: the snapshot handle is open and `entry` is a live
    // PROCESSENTRY32W whose dwSize is set, as the call requires.
    let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
    while more {
        let len = entry
            .szExeFile
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(entry.szExeFile.len());
        processes.push(Process {
            pid: entry.th32ProcessID,
            parent: entry.th32ParentProcessID,
            exe: String::from_utf16_lossy(&entry.szExeFile[..len]),
        });
        // SAFETY: as for Process32FirstW above.
        more = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
    }
    processes
}

#[cfg(test)]
mod tests {
    use super::{KNOWN_PROGRAMS, Process, ProcessTree, known_program, programs_under};

    fn p(pid: u32, parent: u32, exe: &str) -> Process {
        Process {
            pid,
            parent,
            exe: exe.to_owned(),
        }
    }

    #[test]
    fn rf_pan_12_known_names_ignore_case_and_an_optional_exe() {
        for name in KNOWN_PROGRAMS {
            assert_eq!(known_program(name), Some(*name));
            assert_eq!(known_program(&format!("{name}.exe")), Some(*name));
            assert_eq!(
                known_program(&name.to_uppercase()),
                Some(*name),
                "{name} in capitals"
            );
            assert_eq!(
                known_program(&format!("{}.EXE", name.to_uppercase())),
                Some(*name)
            );
            assert_eq!(known_program(&format!("C:\\bin\\{name}.exe")), Some(*name));
        }
    }

    #[test]
    fn rf_pan_12_other_names_are_not_known() {
        for exe in [
            "",
            ".exe",
            "node.exe",
            "pwsh.exe",
            "claude.cmd",
            "claude-code.exe",
            "codexx",
            "exe",
            "é.exe",
        ] {
            assert_eq!(known_program(exe), None, "{exe}");
        }
    }

    #[test]
    fn rf_pan_12_a_shell_without_a_child_runs_nothing_known() {
        let all = [p(10, 1, "pwsh.exe"), p(11, 1, "claude.exe")];
        assert_eq!(ProcessTree::new(&all).program_under(10), None);
        assert_eq!(ProcessTree::new(&[]).program_under(10), None);
    }

    #[test]
    fn rf_pan_12_a_direct_child_is_named() {
        let all = [
            p(10, 1, "pwsh.exe"),
            p(11, 10, "conhost.exe"),
            p(12, 10, "claude.exe"),
        ];
        assert_eq!(ProcessTree::new(&all).program_under(10), Some("claude"));
    }

    #[test]
    fn rf_pan_12_a_cli_started_through_a_shim_and_node_is_named() {
        let all = [
            p(10, 1, "cmd.exe"),
            p(11, 10, "cmd.exe"),
            p(12, 11, "node.exe"),
            p(13, 12, "codex.exe"),
        ];
        assert_eq!(ProcessTree::new(&all).program_under(10), Some("codex"));
    }

    #[test]
    fn rf_pan_12_the_closest_program_wins_over_what_it_runs() {
        let all = [
            p(10, 1, "pwsh.exe"),
            p(11, 10, "claude.exe"),
            p(12, 11, "codex.exe"),
        ];
        assert_eq!(ProcessTree::new(&all).program_under(10), Some("claude"));
    }

    #[test]
    fn rf_pan_12_a_program_too_deep_or_in_another_shell_is_not_named() {
        let all = [
            p(10, 1, "pwsh.exe"),
            p(11, 10, "a.exe"),
            p(12, 11, "b.exe"),
            p(13, 12, "c.exe"),
            p(14, 13, "claude.exe"),
            p(20, 1, "pwsh.exe"),
            p(21, 20, "opencode.exe"),
        ];
        let tree = ProcessTree::new(&all);
        assert_eq!(tree.program_under(10), None);
        assert_eq!(tree.program_under(20), Some("opencode"));
    }

    #[test]
    fn a_process_that_is_its_own_parent_does_not_loop() {
        let all = [p(0, 0, "System Idle Process"), p(4, 0, "System")];
        assert_eq!(ProcessTree::new(&all).program_under(0), None);
    }

    #[test]
    fn no_shells_need_no_snapshot() {
        assert!(programs_under(&[]).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn the_snapshot_lists_this_process_and_finds_nothing_under_it() {
        let me = std::process::id();
        let all = super::snapshot();
        assert!(all.iter().any(|p| p.pid == me && !p.exe.is_empty()));
        assert_eq!(programs_under(&[me]), vec![None]);
    }
}
