//! "Last used" timestamps for skills, read from Claude Code's session logs.
//!
//! Claude Code writes one JSONL transcript per session under
//! `<config>/projects/<project>/<session>.jsonl` (`<config>` is
//! `$CLAUDE_CONFIG_DIR` or `~/.claude`). A skill shows up there in two ways:
//!
//! * the model calls the `Skill` tool: an assistant `tool_use` block with
//!   `"name": "Skill"` and `input.skill` (older builds used `input.command`);
//! * the user types `/skill-name`: the user message carries
//!   `<command-name>/skill-name</command-name>`.
//!
//! Only what is still in those logs can be seen: a skill missing from the
//! result was not observed there, which does not mean it was never used.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::DateTime;
use serde_json::Value;

/// Skill name (lower-case, plugin prefix and leading `/` removed) → last use,
/// as Unix milliseconds.
pub type LastUsedMap = HashMap<String, i64>;

/// Directories holding Claude Code session logs on this machine.
pub fn claude_code_projects_dirs() -> Vec<PathBuf> {
    let mut dirs_out = Vec::new();
    if let Ok(custom) = std::env::var("CLAUDE_CONFIG_DIR") {
        for part in custom.split(if cfg!(windows) { ';' } else { ':' }) {
            let trimmed = part.trim();
            if !trimmed.is_empty() {
                dirs_out.push(PathBuf::from(trimmed).join("projects"));
            }
        }
    }
    if let Some(home) = dirs::home_dir() {
        let default = home.join(".claude").join("projects");
        if !dirs_out.contains(&default) {
            dirs_out.push(default);
        }
    }
    dirs_out
}

/// Scan every `*.jsonl` under `projects_dirs` and keep the newest timestamp
/// seen for each skill.
pub fn scan_last_used(projects_dirs: &[PathBuf]) -> LastUsedMap {
    let mut map = LastUsedMap::new();
    for dir in projects_dirs {
        if !dir.is_dir() {
            continue;
        }
        for entry in walkdir::WalkDir::new(dir)
            .max_depth(4)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if entry.file_type().is_file()
                && path.extension().and_then(|e| e.to_str()) == Some("jsonl")
            {
                scan_file(path, &mut map);
            }
        }
    }
    map
}

fn scan_file(path: &Path, map: &mut LastUsedMap) {
    let Ok(file) = File::open(path) else { return };
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        // Cheap pre-filter: most lines mention neither form.
        if !line.contains("\"Skill\"") && !line.contains("<command-name>") {
            continue;
        }
        scan_line(&line, map);
    }
}

fn scan_line(line: &str, map: &mut LastUsedMap) {
    let Ok(value) = serde_json::from_str::<Value>(line) else { return };
    let Some(ts) = value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp_millis())
    else {
        return;
    };

    let mut names = Vec::new();
    let line_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    let content = value.get("message").and_then(|m| m.get("content"));

    if line_type == "assistant" {
        if let Some(Value::Array(blocks)) = content {
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("tool_use")
                    && block.get("name").and_then(Value::as_str) == Some("Skill")
                {
                    let input = block.get("input");
                    let name = input
                        .and_then(|i| i.get("skill"))
                        .or_else(|| input.and_then(|i| i.get("command")))
                        .and_then(Value::as_str);
                    if let Some(name) = name {
                        names.push(name.to_string());
                    }
                }
            }
        }
    } else if line_type == "user" {
        match content {
            Some(Value::String(text)) => collect_command_names(text, &mut names),
            Some(Value::Array(blocks)) => {
                for block in blocks {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        collect_command_names(text, &mut names);
                    }
                }
            }
            _ => {}
        }
    }

    for raw in names {
        if let Some(key) = normalize_skill_name(&raw) {
            let slot = map.entry(key).or_insert(ts);
            if ts > *slot {
                *slot = ts;
            }
        }
    }
}

fn collect_command_names(text: &str, out: &mut Vec<String>) {
    const OPEN: &str = "<command-name>";
    const CLOSE: &str = "</command-name>";
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let Some(end) = after.find(CLOSE) else { break };
        out.push(after[..end].to_string());
        rest = &after[end + CLOSE.len()..];
    }
}

/// `"/plugin:My-Skill "` → `"my-skill"`. Returns `None` for empty names.
pub fn normalize_skill_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_start_matches('/');
    let base = trimmed.rsplit(':').next().unwrap_or(trimmed).trim();
    if base.is_empty() {
        None
    } else {
        Some(base.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn normalizes_names() {
        assert_eq!(normalize_skill_name("/pdf").as_deref(), Some("pdf"));
        assert_eq!(normalize_skill_name("anthropic-skills:Docx").as_deref(), Some("docx"));
        assert_eq!(normalize_skill_name("  "), None);
    }

    #[test]
    fn reads_tool_use_and_slash_commands_keeping_latest() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("projects").join("p1");
        std::fs::create_dir_all(&proj).unwrap();
        let mut f = File::create(proj.join("s.jsonl")).unwrap();
        writeln!(f, r#"{{"type":"assistant","timestamp":"2026-09-01T10:00:00.000Z","message":{{"content":[{{"type":"tool_use","id":"t1","name":"Skill","input":{{"skill":"plug:report-4-0"}}}}]}}}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","timestamp":"2026-09-03T10:00:00.000Z","message":{{"content":[{{"type":"tool_use","id":"t2","name":"Skill","input":{{"command":"report-4-0"}}}}]}}}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","timestamp":"2026-09-02T08:00:00.000Z","message":{{"content":"<command-name>/caveman-setup</command-name>"}}}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","timestamp":"2026-09-05T08:00:00.000Z","message":{{"content":[{{"type":"text","text":"x <command-name>/pdf</command-name>"}}]}}}}"#).unwrap();
        // Tool definitions also contain "Skill" but are not uses.
        writeln!(f, r#"{{"type":"system","timestamp":"2026-09-09T08:00:00.000Z","tools":[{{"name":"Skill"}}]}}"#).unwrap();
        writeln!(f, "not json \"Skill\"").unwrap();
        drop(f);

        let map = scan_last_used(&[dir.path().join("projects")]);
        let ms = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().timestamp_millis();
        assert_eq!(map.get("report-4-0"), Some(&ms("2026-09-03T10:00:00.000Z")));
        assert_eq!(map.get("caveman-setup"), Some(&ms("2026-09-02T08:00:00.000Z")));
        assert_eq!(map.get("pdf"), Some(&ms("2026-09-05T08:00:00.000Z")));
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn missing_dir_is_empty() {
        let map = scan_last_used(&[PathBuf::from("/definitely/not/here")]);
        assert!(map.is_empty());
    }
}
