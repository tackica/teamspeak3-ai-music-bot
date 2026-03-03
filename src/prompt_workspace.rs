use std::fs;
use std::path::{Path, PathBuf};

use tracing::warn;

const AGENTS_TEMPLATE: &str = r#"# AGENTS.md

This workspace provides persistent prompt context for the TS3 bot.

Rules:
- Always return valid JSON action arrays.
- Use only supported actions.
- Keep replies useful and concise.
"#;

const SOUL_TEMPLATE: &str = r#"# SOUL.md

Identity:
- Helpful, direct, and calm.
- Avoid filler words.
- Be proactive with internal reasoning and context.

Boundaries:
- Do not expose private data.
- Ask before external/destructive actions.
"#;

const TOOLS_TEMPLATE: &str = r#"# TOOLS.md

Local notes for this installation:
- Preferred TTS voices
- Local server details
- Short operator-specific reminders
"#;

const MEMORY_TEMPLATE: &str = r#"# MEMORY.md

Long-term bot memory:
- Important decisions
- Persistent preferences
- Known constraints
"#;

const AUTO_PROFILE_START: &str = "<!-- AUTO_PROFILE_START -->";
const AUTO_PROFILE_END: &str = "<!-- AUTO_PROFILE_END -->";
const AUTO_NOTES_START: &str = "<!-- AUTO_NOTES_START -->";
const AUTO_NOTES_END: &str = "<!-- AUTO_NOTES_END -->";

fn normalize_uid(user_uid: &str) -> String {
    let trimmed = user_uid.trim();
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn user_profile_path(workspace_dir: &str, user_uid: &str) -> PathBuf {
    let normalized_uid = normalize_uid(user_uid);
    let encoded_uid = urlencoding::encode(&normalized_uid).into_owned();
    PathBuf::from(workspace_dir)
        .join("users")
        .join(encoded_uid)
        .join("USER.md")
}

fn user_template(user_uid: &str, user_name: &str) -> String {
    format!(
        "# USER.md\n\nUID: {}\nName: {}\nPreferred language: \nNotes:\n- \n\n## Auto Profile (managed)\n{}\ntrust_tier: member\nresponse_language: \nresponse_style: \ntts_mode: \ninteraction_mode: \nts_scope: teamspeak_only\nlast_updated_at: \n{}\n\n## Auto Notes (managed)\n{}\n{}\n",
        user_uid,
        user_name,
        AUTO_PROFILE_START,
        AUTO_PROFILE_END,
        AUTO_NOTES_START,
        AUTO_NOTES_END,
    )
}

fn ensure_seed_file(path: &Path, content: &str) {
    if path.exists() {
        return;
    }

    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            warn!(path = %path.display(), error = %e, "Failed to create prompt workspace directory");
            return;
        }
    }

    if let Err(e) = fs::write(path, content) {
        warn!(path = %path.display(), error = %e, "Failed to create prompt workspace seed file");
    }
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }

    let mut out = String::new();
    for (idx, ch) in input.chars().enumerate() {
        if idx >= max_chars {
            break;
        }
        out.push(ch);
    }
    out
}

pub fn ensure_workspace_files(workspace_dir: &str, user_uid: &str, user_name: &str) -> PathBuf {
    let root = PathBuf::from(workspace_dir);
    if let Err(e) = fs::create_dir_all(&root) {
        warn!(path = %root.display(), error = %e, "Failed to create prompt workspace root");
    }

    let normalized_uid = normalize_uid(user_uid);
    let user_file = user_profile_path(workspace_dir, &normalized_uid);

    ensure_seed_file(&root.join("AGENTS.md"), AGENTS_TEMPLATE);
    ensure_seed_file(&root.join("SOUL.md"), SOUL_TEMPLATE);
    ensure_seed_file(&root.join("TOOLS.md"), TOOLS_TEMPLATE);
    ensure_seed_file(&root.join("MEMORY.md"), MEMORY_TEMPLATE);
    ensure_seed_file(&user_file, &user_template(&normalized_uid, user_name));

    user_file
}

pub fn load_workspace_context(
    workspace_dir: &str,
    user_uid: &str,
    user_name: &str,
    max_chars_per_file: usize,
    max_total_chars: usize,
) -> String {
    if max_chars_per_file == 0 || max_total_chars == 0 {
        return String::new();
    }

    let root = PathBuf::from(workspace_dir);
    let user_file = ensure_workspace_files(workspace_dir, user_uid, user_name);

    let mut files: Vec<(String, PathBuf)> = vec![
        ("AGENTS.md".to_string(), root.join("AGENTS.md")),
        ("SOUL.md".to_string(), root.join("SOUL.md")),
        ("TOOLS.md".to_string(), root.join("TOOLS.md")),
        ("MEMORY.md".to_string(), root.join("MEMORY.md")),
        ("USER.md".to_string(), user_file),
    ];

    let today = chrono::Local::now().date_naive();
    for day in [today, today - chrono::Days::new(1)] {
        let date_str = day.format("%Y-%m-%d").to_string();
        let path = root.join("memory").join(format!("{}.md", date_str));
        if path.exists() {
            files.push((format!("memory/{}.md", date_str), path));
        }
    }

    let mut rendered = String::new();
    let mut used_chars = 0usize;

    for (label, path) in files {
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let trimmed = content.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut section_content = truncate_chars(trimmed, max_chars_per_file);
        if trimmed.chars().count() > max_chars_per_file {
            section_content.push_str("\n\n[FILE TRUNCATED]");
        }

        let section = format!(
            "=== {} ===\nPath: {}\n{}\n\n",
            label,
            path.display(),
            section_content
        );
        let section_chars = section.chars().count();

        if used_chars + section_chars > max_total_chars {
            let remaining = max_total_chars.saturating_sub(used_chars);
            if remaining > 0 {
                rendered.push_str(&truncate_chars(&section, remaining));
            }
            break;
        }

        rendered.push_str(&section);
        used_chars += section_chars;
    }

    rendered.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::load_workspace_context;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir_path() -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("ts3-ai-bot-workspace-test-{}", stamp))
    }

    #[test]
    fn creates_seed_files_and_user_profile() {
        let path = temp_dir_path();
        let path_str = path.to_string_lossy().to_string();

        let ctx = load_workspace_context(
            &path_str,
            "VBOROBYGV1unsuWVHzZFIPzAADY=",
            "OwnerUser",
            2000,
            20000,
        );

        assert!(path.join("AGENTS.md").exists());
        assert!(path.join("SOUL.md").exists());
        assert!(path.join("TOOLS.md").exists());
        assert!(path.join("MEMORY.md").exists());
        assert!(ctx.contains("=== USER.md ==="));
        assert!(ctx.contains("OwnerUser"));

        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn respects_total_limit() {
        let path = temp_dir_path();
        let path_str = path.to_string_lossy().to_string();

        let ctx = load_workspace_context(&path_str, "uid", "name", 5000, 180);
        assert!(ctx.chars().count() <= 180);

        let _ = fs::remove_dir_all(path);
    }
}
