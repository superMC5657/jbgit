use chrono::{DateTime, Local};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitConsoleEntry {
    pub id: u64,
    pub started_at: DateTime<Local>,
    pub duration_ms: Option<u64>,
    pub command_line: String,
    pub stdout_lines: Vec<String>,
    pub stderr_lines: Vec<String>,
    pub exit_code: Option<i32>,
}

impl GitConsoleEntry {
    pub fn is_running(&self) -> bool {
        self.exit_code.is_none()
    }

    pub fn is_success(&self) -> bool {
        self.exit_code == Some(0)
    }

    pub fn timestamp_label(&self) -> String {
        self.started_at.format("%H:%M:%S").to_string()
    }
}
