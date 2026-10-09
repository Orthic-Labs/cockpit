//! Structured stderr diagnostics: one `key=value` line per event, no multi-line output.
//! Format: `pulse-windows level=<l> event=<e> k=v ...`; values with spaces, quotes or
//! control characters are double-quoted and escaped. Release builds use the Windows
//! subsystem (no console), where stderr writes are ignored; debug/CI runs show them.

use std::io::Write;

pub fn quote_value(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_graphic() && c != '"' && c != '\\' && c != '=');
    if plain {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push('?'),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn format_line(level: &str, event: &str, fields: &[(&str, &str)]) -> String {
    let mut line = format!(
        "pulse-windows level={} event={}",
        quote_value(level),
        quote_value(event)
    );
    for (key, value) in fields {
        line.push(' ');
        line.push_str(key);
        line.push('=');
        line.push_str(&quote_value(value));
    }
    line
}

/// Write one line to stderr and append it to `%LOCALAPPDATA%\Pulse\notch.log`.
/// Errors are ignored: diagnostics must never take the pill down.
pub fn write_line(line: &str) {
    let _ = writeln!(std::io::stderr(), "{line}");
    append_log(line);
}

const LOG_CAP: u64 = 1024 * 1024;

fn append_log(line: &str) {
    let Some(base) = std::env::var_os("LOCALAPPDATA") else {
        return;
    };
    let dir = std::path::Path::new(&base).join("Pulse");
    let path = dir.join("notch.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_CAP) {
        let _ = std::fs::rename(&path, dir.join("notch.log.1"));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "{line}");
    }
}

pub fn emit(level: &str, event: &str, fields: &[(&str, &str)]) {
    write_line(&format_line(level, event, fields));
}

pub fn info(event: &str, fields: &[(&str, &str)]) {
    emit("info", event, fields);
}

pub fn failure_fields(op: &str, code: u32, message: &str, ctx: &str) -> String {
    let code_text = format!("0x{code:08X}");
    format_line(
        "error",
        "win32_failure",
        &[
            ("op", op),
            ("code", &code_text),
            ("msg", message.trim()),
            ("ctx", ctx),
        ],
    )
}

pub fn win32_error(op: &str, error: &windows::core::Error, ctx: &str) {
    let line = failure_fields(op, error.code().0 as u32, &error.message(), ctx);
    write_line(&line);
}

/// Report the calling thread's last Win32 error for APIs that only return BOOL/null.
pub fn last_error(op: &str, ctx: &str) {
    win32_error(op, &windows::core::Error::from_win32(), ctx);
}

/// Edge-triggered failure latch so a persistently failing 2 s sampler logs once per
/// failure episode, plus once on recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transition {
    Failed,
    Recovered,
    Unchanged,
}

#[derive(Default)]
pub struct FailureLatch {
    failing: bool,
}

impl FailureLatch {
    pub const fn new() -> Self {
        Self { failing: false }
    }
    pub fn observe(&mut self, failed: bool) -> Transition {
        let transition = match (self.failing, failed) {
            (false, true) => Transition::Failed,
            (true, false) => Transition::Recovered,
            _ => Transition::Unchanged,
        };
        self.failing = failed;
        transition
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_values_are_unquoted() {
        assert_eq!(quote_value("EnumWindows"), "EnumWindows");
        assert_eq!(quote_value("0x80070005"), "0x80070005");
    }

    #[test]
    fn spaces_quotes_and_newlines_are_escaped() {
        assert_eq!(quote_value(""), "\"\"");
        assert_eq!(quote_value("a b"), "\"a b\"");
        assert_eq!(quote_value("say \"hi\"\r\n"), "\"say \\\"hi\\\"\\r\\n\"");
        assert_eq!(quote_value("\\\\.\\DISPLAY1"), "\"\\\\\\\\.\\\\DISPLAY1\"");
    }

    #[test]
    fn failure_line_is_single_structured_line() {
        let line = failure_fields(
            "CreateWindowExW",
            0x8007_0005,
            "Access is denied.\r\n",
            "monitor=\\\\.\\DISPLAY2",
        );
        assert_eq!(
            line,
            "pulse-windows level=error event=win32_failure op=CreateWindowExW \
             code=0x80070005 msg=\"Access is denied.\" ctx=\"monitor=\\\\\\\\.\\\\DISPLAY2\""
        );
        assert!(!line.contains('\n') && !line.contains('\r'));
    }

    #[test]
    fn latch_reports_edges_only() {
        let mut latch = FailureLatch::new();
        assert_eq!(latch.observe(false), Transition::Unchanged);
        assert_eq!(latch.observe(true), Transition::Failed);
        assert_eq!(latch.observe(true), Transition::Unchanged);
        assert_eq!(latch.observe(false), Transition::Recovered);
        assert_eq!(latch.observe(false), Transition::Unchanged);
    }
}
