//! `cswitch tui` / `cswitch watch` entry. (Task F)

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiStart {
    Dashboard,
    Watch,
}

/// Returns the exit status: 0 on a clean quit, 1 when there is no terminal.
pub fn run(start: TuiStart) -> i32 {
    crate::tui::run(start)
}
