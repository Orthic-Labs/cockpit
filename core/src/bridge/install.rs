//! `pulse bridge install|uninstall [--claude] [--codex] [--dry-run] [--json]`:
//! register (or remove) the Pulse MCP server so Claude and Codex chats can use
//! the bridge tools.
//!
//! What is written (other servers and every other key are left exactly as they
//! are; the file is edited in place as text, not re-serialised):
//!
//! * Claude Code (user scope): `mcpServers.pulse` in `~/.claude.json`
//!   (`$CLAUDE_CONFIG_DIR/.claude.json` when set) =
//!   `{"type":"stdio","command":<pulse binary>,"args":["bridge","mcp"],"env":{}}`.
//!   Skipped when the file doesn't exist (Claude Code never ran here).
//! * Claude Desktop: `mcpServers.pulse` = `{"command":<pulse binary>,"args":["bridge","mcp"]}` in
//!   `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS),
//!   `%APPDATA%\Claude\claude_desktop_config.json` (Windows). Created when the
//!   Claude folder exists, skipped when it doesn't.
//! * Codex: a `[mcp_servers.pulse]` table with `command` and `args` in
//!   `~/.codex/config.toml` (`$CODEX_HOME`), marked with a `# pulse-owned`
//!   comment line. Created when `~/.codex` exists.
//!
//! Every write is atomic (temp file + rename, permissions kept), keeps a
//! one-time `<file>.pulse-bak` copy, re-checks the file just before renaming
//! (Claude Code rewrites `~/.claude.json` while it runs) and refuses a file it
//! can't parse. Chats already running need a restart to see the new server.

use serde::Serialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const MARK: &str = "# pulse-owned";

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub claude: bool,
    pub codex: bool,
    pub dry_run: bool,
    /// The `pulse` binary the MCP entry runs; found next to the running program when None.
    pub command: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Change {
    /// "claude-code", "claude-desktop" or "codex".
    pub target: String,
    pub path: String,
    /// "added", "updated", "removed", "unchanged" or "skipped" ("would …" never; see `Report::dry_run`).
    pub action: String,
    pub note: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub dry_run: bool,
    pub command: String,
    pub changes: Vec<Change>,
}

fn home() -> PathBuf {
    #[cfg(windows)]
    let variable = "USERPROFILE";
    #[cfg(not(windows))]
    let variable = "HOME";
    std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn claude_code_config() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir).join(".claude.json"),
        None => home().join(".claude.json"),
    }
}

fn claude_desktop_config() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home().join("Library/Application Support/Claude/claude_desktop_config.json")
    }
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join("AppData").join("Roaming"))
            .join("Claude")
            .join("claude_desktop_config.json")
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        home().join(".config/Claude/claude_desktop_config.json")
    }
}

fn codex_config() -> PathBuf {
    super::deliver_codex::codex_home().join("config.toml")
}

/// The bundled `pulse` binary: the running program when it is `pulse`, else a
/// sibling, `Helpers/` or `Contents/MacOS` of the app bundle the hub runs from.
pub fn locate_pulse_binary() -> Option<PathBuf> {
    let name = if cfg!(windows) { "pulse.exe" } else { "pulse" };
    let exe = std::env::current_exe().ok()?;
    if exe.file_name().and_then(|n| n.to_str()) == Some(name) {
        return Some(exe);
    }
    let mut candidates = Vec::new();
    if let Some(dir) = exe.parent() {
        candidates.push(dir.join(name));
        candidates.push(dir.join("Helpers").join(name));
    }
    // Outermost first: the hub runs from `Pulse.app/Contents/Helpers/Pulse.app/...`
    // and the CLI ships at the outer bundle's `Contents/Helpers/pulse`.
    let ancestors: Vec<&Path> = exe.ancestors().collect();
    for ancestor in ancestors.into_iter().rev() {
        if ancestor.file_name().and_then(|n| n.to_str()) == Some("Helpers") {
            candidates.push(ancestor.join(name));
        }
        if ancestor.extension().and_then(|e| e.to_str()) == Some("app") {
            for sub in ["Contents/MacOS", "Contents/Helpers", "Contents/Resources"] {
                candidates.push(ancestor.join(sub).join(name));
            }
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

// ---- a small JSON scanner so files are edited as text ---------------------------------

struct Member {
    key: String,
    start: usize,
    vstart: usize,
    vend: usize,
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// `i` is at an opening quote; returns the index after the closing one.
fn skip_string(b: &[u8], mut i: usize) -> Option<usize> {
    i += 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

fn skip_value(b: &[u8], i: usize) -> Option<usize> {
    let i = skip_ws(b, i);
    match *b.get(i)? {
        b'"' => skip_string(b, i),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut j = i;
            while j < b.len() {
                match b[j] {
                    b'"' => {
                        j = skip_string(b, j)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(j + 1);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            None
        }
        _ => {
            let mut j = i;
            while j < b.len() && !matches!(b[j], b',' | b'}' | b']') && !b[j].is_ascii_whitespace()
            {
                j += 1;
            }
            (j > i).then_some(j)
        }
    }
}

/// The members of the object whose `{` is at `open`.
fn members(text: &str, open: usize) -> Option<Vec<Member>> {
    let b = text.as_bytes();
    if b.get(open) != Some(&b'{') {
        return None;
    }
    let mut out = Vec::new();
    let mut i = skip_ws(b, open + 1);
    loop {
        match *b.get(i)? {
            b'}' => return Some(out),
            b'"' => {
                let end = skip_string(b, i)?;
                let key: String = serde_json::from_str(&text[i..end]).ok()?;
                let colon = skip_ws(b, end);
                if b.get(colon) != Some(&b':') {
                    return None;
                }
                let vstart = skip_ws(b, colon + 1);
                let vend = skip_value(b, vstart)?;
                out.push(Member {
                    key,
                    start: i,
                    vstart,
                    vend,
                });
                i = skip_ws(b, vend);
                match *b.get(i)? {
                    b',' => i = skip_ws(b, i + 1),
                    b'}' => {}
                    _ => return None,
                }
            }
            _ => return None,
        }
    }
}

fn root_open(text: &str) -> Option<usize> {
    let i = skip_ws(text.as_bytes(), 0);
    (text.as_bytes().get(i) == Some(&b'{')).then_some(i)
}

fn indent_of(text: &str, member_start: usize) -> String {
    let line_start = text[..member_start].rfind('\n').map(|p| p + 1);
    match line_start {
        Some(s) if text[s..member_start].chars().all(|c| c == ' ' || c == '\t') => {
            text[s..member_start].to_string()
        }
        _ => String::new(),
    }
}

/// Set `key` in the object at `open` to `value` (compact JSON), keeping everything else.
fn set_member(text: &str, open: usize, key: &str, value: &Value) -> Option<String> {
    let list = members(text, open)?;
    let rendered = serde_json::to_string(value).ok()?;
    let key_json = serde_json::to_string(key).ok()?;
    if let Some(m) = list.iter().find(|m| m.key == key) {
        return Some(format!(
            "{}{}{}",
            &text[..m.vstart],
            rendered,
            &text[m.vend..]
        ));
    }
    let insert = match list.first() {
        None => format!("{key_json}:{rendered}"),
        Some(first) => {
            let indent = indent_of(text, first.start);
            if indent.is_empty() {
                format!("{key_json}:{rendered},")
            } else {
                format!("\n{indent}{key_json}: {rendered},")
            }
        }
    };
    Some(format!(
        "{}{}{}",
        &text[..open + 1],
        insert,
        &text[open + 1..]
    ))
}

/// Remove `key` from the object at `open`, with its comma.
fn remove_member(text: &str, open: usize, key: &str) -> Option<String> {
    let list = members(text, open)?;
    let at = list.iter().position(|m| m.key == key)?;
    let m = &list[at];
    let (from, to) = if let Some(next) = list.get(at + 1) {
        (m.start, next.start)
    } else if at > 0 {
        (list[at - 1].vend, m.vend)
    } else {
        (m.start, m.vend)
    };
    Some(format!("{}{}", &text[..from], &text[to..]))
}

fn servers_open(text: &str) -> Result<Option<usize>, String> {
    let root = root_open(text).ok_or("the file isn't a JSON object")?;
    let list = members(text, root).ok_or("the file isn't valid JSON")?;
    match list.iter().find(|m| m.key == "mcpServers") {
        None => Ok(None),
        Some(m) if text.as_bytes()[m.vstart] == b'{' => Ok(Some(m.vstart)),
        Some(_) => Err("mcpServers isn't an object".to_string()),
    }
}

type Edited = Result<Option<(String, &'static str)>, String>;

fn json_install(text: &str, entry: &Value) -> Edited {
    let root = root_open(text).ok_or("the file isn't a JSON object")?;
    let Some(inner) = servers_open(text)? else {
        let wrapper = json!({"pulse": entry});
        let new = set_member(text, root, "mcpServers", &wrapper).ok_or("couldn't edit the file")?;
        return Ok(Some((new, "added")));
    };
    let list = members(text, inner).ok_or("mcpServers isn't valid JSON")?;
    if let Some(m) = list.iter().find(|m| m.key == "pulse") {
        let current: Option<Value> = serde_json::from_str(&text[m.vstart..m.vend]).ok();
        if current.as_ref() == Some(entry) {
            return Ok(None);
        }
        let new = set_member(text, inner, "pulse", entry).ok_or("couldn't edit the file")?;
        return Ok(Some((new, "updated")));
    }
    let new = set_member(text, inner, "pulse", entry).ok_or("couldn't edit the file")?;
    Ok(Some((new, "added")))
}

fn json_uninstall(text: &str) -> Edited {
    let Some(inner) = servers_open(text)? else {
        return Ok(None);
    };
    let list = members(text, inner).ok_or("mcpServers isn't valid JSON")?;
    if !list.iter().any(|m| m.key == "pulse") {
        return Ok(None);
    }
    let new = remove_member(text, inner, "pulse").ok_or("couldn't edit the file")?;
    Ok(Some((new, "removed")))
}

// ---- TOML (Codex) ---------------------------------------------------------------------

fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn toml_block(command: &str) -> String {
    format!(
        "{MARK}\n[mcp_servers.pulse]\ncommand = {}\nargs = [\"bridge\", \"mcp\"]\n",
        toml_string(command)
    )
}

fn header_name(line: &str) -> Option<String> {
    let t = line.trim();
    if !t.starts_with('[') || t.starts_with("[[") {
        return t.starts_with("[[").then(|| "[[".to_string());
    }
    let end = t.find(']')?;
    Some(
        t[1..end]
            .replace(['"', '\'', ' '], ""),
    )
}

fn is_pulse_table(name: &str) -> bool {
    name == "mcp_servers.pulse" || name.starts_with("mcp_servers.pulse.")
}

/// Byte range of the Pulse table(s) and the marker line just above them.
fn toml_range(text: &str) -> Option<(usize, usize)> {
    let mut offset = 0usize;
    let mut start: Option<usize> = None;
    let mut end = text.len();
    let mut previous_line: Option<(usize, &str)> = None;
    for line in text.split_inclusive('\n') {
        if let Some(name) = header_name(line) {
            if is_pulse_table(&name) {
                if start.is_none() {
                    start = Some(match previous_line {
                        Some((p, l)) if l.trim() == MARK => p,
                        _ => offset,
                    });
                }
            } else if start.is_some() {
                end = offset;
                break;
            }
        }
        previous_line = Some((offset, line));
        offset += line.len();
    }
    start.map(|s| (s, end))
}

fn toml_install(text: &str, command: &str) -> Edited {
    let block = toml_block(command);
    match toml_range(text) {
        Some((start, end)) => {
            if text[start..end].trim_end() == block.trim_end() {
                return Ok(None);
            }
            let tail = &text[end..];
            let sep = if tail.is_empty() { "" } else { "\n" };
            Ok(Some((
                format!("{}{block}{sep}{tail}", &text[..start]),
                "updated",
            )))
        }
        None => {
            let mut new = text.to_string();
            if !new.is_empty() && !new.ends_with('\n') {
                new.push('\n');
            }
            if !new.is_empty() {
                new.push('\n');
            }
            new.push_str(&block);
            Ok(Some((new, "added")))
        }
    }
}

fn toml_uninstall(text: &str) -> Edited {
    let Some((start, end)) = toml_range(text) else {
        return Ok(None);
    };
    let mut head = text[..start].to_string();
    while head.ends_with("\n\n") {
        head.pop();
    }
    Ok(Some((format!("{head}{}", &text[end..]), "removed")))
}

// ---- files ----------------------------------------------------------------------------

fn write_atomic(path: &Path, text: &str, like: Option<&std::fs::Metadata>) -> Result<(), String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config");
    let temp = path.with_file_name(format!("{name}.pulse-tmp"));
    let result = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(&temp)?;
        if let Some(meta) = like {
            let _ = std::fs::set_permissions(&temp, meta.permissions());
        }
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map_err(|e| format!("couldn't write the file ({})", e.kind()))
}

/// Edit one file in place. `missing_ok` starts from `empty` when the file doesn't exist.
fn edit_file(
    path: &Path,
    empty: Option<&str>,
    dry_run: bool,
    edit: &dyn Fn(&str) -> Edited,
) -> Result<(&'static str, String), String> {
    for _ in 0..3 {
        let meta = std::fs::metadata(path).ok();
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => match empty {
                Some(empty) => empty.to_string(),
                None => return Ok(("skipped", "not set up on this computer".to_string())),
            },
            Err(e) => return Err(format!("couldn't read the file ({})", e.kind())),
        };
        let Some((new, action)) =
            edit(text.trim_start_matches('\u{feff}')).map_err(|e| format!("left alone: {e}"))?
        else {
            return Ok(("unchanged", "already in place".to_string()));
        };
        if dry_run {
            return Ok((action, "dry run: nothing written".to_string()));
        }
        if meta.is_some() {
            let mut backup = path.as_os_str().to_owned();
            backup.push(".pulse-bak");
            let backup = PathBuf::from(backup);
            if !backup.exists() {
                std::fs::copy(path, &backup)
                    .map_err(|e| format!("couldn't back the file up ({})", e.kind()))?;
            }
        }
        // Another program (Claude Code) may have rewritten the file meanwhile: start over if so.
        let unchanged_since = match std::fs::read_to_string(path) {
            Ok(now) => meta.is_some() && now == text,
            Err(_) => meta.is_none(),
        };
        if !unchanged_since {
            continue;
        }
        write_atomic(path, &new, meta.as_ref())?;
        return Ok((action, "written".to_string()));
    }
    Err("the file kept changing; try again".to_string())
}

fn record(
    report: &mut Report,
    target: &str,
    path: &Path,
    result: Result<(&'static str, String), String>,
) {
    let (action, note) = match result {
        Ok((action, note)) => (action.to_string(), note),
        Err(note) => ("skipped".to_string(), note),
    };
    report.changes.push(Change {
        target: target.to_string(),
        path: path.to_string_lossy().into_owned(),
        action,
        note,
    });
}

/// Register (`uninstall == false`) or remove the Pulse MCP server.
pub fn apply(uninstall: bool, options: &Options) -> Result<Report, String> {
    let command = if uninstall {
        String::new()
    } else {
        options
            .command
            .clone()
            .or_else(locate_pulse_binary)
            .ok_or("Couldn't find the pulse program to register.")?
            .to_string_lossy()
            .into_owned()
    };
    let mut report = Report {
        dry_run: options.dry_run,
        command: command.clone(),
        changes: Vec::new(),
    };
    if options.claude {
        let code_entry =
            json!({"type": "stdio", "command": command, "args": ["bridge", "mcp"], "env": {}});
        let path = claude_code_config();
        let result = edit_file(&path, None, options.dry_run, &|t| {
            if uninstall {
                json_uninstall(t)
            } else {
                json_install(t, &code_entry)
            }
        });
        record(&mut report, "claude-code", &path, result);

        let desktop_entry = json!({"command": command, "args": ["bridge", "mcp"]});
        let path = claude_desktop_config();
        let folder_exists = path.parent().is_some_and(Path::is_dir);
        let empty = (folder_exists && !uninstall).then_some("{}\n");
        let result = edit_file(&path, empty, options.dry_run, &|t| {
            if uninstall {
                json_uninstall(t)
            } else {
                json_install(t, &desktop_entry)
            }
        });
        record(&mut report, "claude-desktop", &path, result);
    }
    if options.codex {
        let path = codex_config();
        let folder_exists = path.parent().is_some_and(Path::is_dir);
        let empty = (folder_exists && !uninstall).then_some("");
        let result = edit_file(&path, empty, options.dry_run, &|t| {
            if uninstall {
                toml_uninstall(t)
            } else {
                toml_install(t, &command)
            }
        });
        record(&mut report, "codex", &path, result);
    }
    Ok(report)
}

/// The `bridge install` / `bridge uninstall` command line (flags after the subcommand).
pub fn run(subcommand: &str, args: Vec<String>, machine: bool) -> Result<(), String> {
    let mut options = Options::default();
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--claude" => options.claude = true,
            "--codex" => options.codex = true,
            "--dry-run" => options.dry_run = true,
            "--command" => {
                options.command = Some(PathBuf::from(iter.next().ok_or("--command needs a path")?))
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    if !options.claude && !options.codex {
        options.claude = true;
        options.codex = true;
    }
    let report = apply(subcommand == "uninstall", &options)?;
    if machine {
        println!(
            "{}",
            serde_json::to_string(&report).map_err(|e| e.to_string())?
        );
    } else {
        if report.dry_run {
            println!("Dry run: nothing is written.");
        }
        for change in &report.changes {
            println!(
                "{:<15} {:<10} {} ({})",
                change.target, change.action, change.path, change.note
            );
        }
    }
    Ok(())
}
