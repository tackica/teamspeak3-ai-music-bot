use anyhow::{Context, Result};
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::Path;

/// Bot configuration loaded from a TOML file.
#[derive(Debug, Deserialize)]
pub struct BotConfig {
    /// TeamSpeak 3 server address.
    #[serde(default = "default_address")]
    pub server_address: String,

    /// Bot display name in TeamSpeak.
    #[serde(default = "default_bot_name")]
    pub bot_name: String,

    /// Default channel to join on connect.
    pub channel: Option<String>,

    /// Path to the identity private key file.
    #[serde(default = "default_key_file")]
    pub key_file: String,

    /// Disconnect message.
    #[serde(default = "default_disconnect_message")]
    pub disconnect_message: String,

    /// Rate limit: max responses per second.
    #[serde(default = "default_rate_limit")]
    pub rate_limit: u8,

    /// AI API URL (OpenAI-compatible endpoint).
    #[serde(default = "default_ai_api_url")]
    pub ai_api_url: String,

    /// AI API Key (Bearer token for authentication).
    #[serde(default = "default_ai_api_key")]
    pub ai_api_key: String,

    /// Primary LLM model name.
    #[serde(default = "default_model")]
    pub default_model: String,

    /// Fallback API URL (e.g. for a local Ollama instance).
    #[serde(default = "default_fallback_api_url")]
    pub fallback_api_url: String,

    /// Fallback API Key.
    #[serde(default = "default_fallback_api_key")]
    pub fallback_api_key: String,

    /// Fallback LLM model name.
    #[serde(default = "default_fallback_model")]
    pub fallback_model: String,

    /// AI request timeout in seconds.
    #[serde(default = "default_ai_timeout")]
    pub ai_timeout_secs: u64,

    /// List of TeamSpeak Unique IDs that are considered administrators for this bot.
    #[serde(default = "default_admin_uids")]
    pub admin_uids: Vec<String>,

    /// List of TeamSpeak Unique IDs that are considered moderators.
    #[serde(default = "default_moderator_uids")]
    pub moderator_uids: Vec<String>,

    /// Path to the audit log file.
    #[serde(default = "default_audit_log_file")]
    pub audit_log_file: String,

    /// Map of Server Groups that standard users are allowed to assign to themselves.
    #[serde(default = "default_allowed_server_groups")]
    pub allowed_server_groups: std::collections::HashMap<String, u64>,

    /// Directory containing AGENTS.md / SOUL.md / TOOLS.md and per-user USER.md files.
    #[serde(default = "default_prompt_workspace_dir")]
    pub prompt_workspace_dir: String,

    /// Maximum number of characters loaded from each workspace prompt file.
    #[serde(default = "default_prompt_file_max_chars")]
    pub prompt_file_max_chars: usize,

    /// Maximum total characters injected from all workspace prompt files.
    #[serde(default = "default_prompt_total_max_chars")]
    pub prompt_total_max_chars: usize,

    /// Enable automatic trust-scoped profile learning from incoming messages.
    #[serde(default = "default_auto_learning_enabled")]
    pub auto_learning_enabled: bool,

    /// Maximum number of managed auto-notes retained in each USER.md profile.
    #[serde(default = "default_auto_learning_note_limit")]
    pub auto_learning_note_limit: usize,

    /// Path to the ticket storage JSON file.
    #[serde(default = "default_tickets_file")]
    pub tickets_file: String,

    /// Path to the identity history storage JSON file.
    #[serde(default = "default_identities_file")]
    pub identities_file: String,

    /// Path to the radio station storage JSON file.
    #[serde(default = "default_radios_file")]
    pub radios_file: String,

    /// TeamSpeak channel group ID used for channel admin assignment.
    #[serde(default = "default_channel_admin_group_id")]
    pub channel_admin_group_id: u64,

    /// Optional channel ID used as ordering anchor for newly created channels.
    #[serde(default = "default_channel_order_anchor_id")]
    pub channel_order_anchor_id: Option<u64>,

    /// Channel ID where long admin outputs can be placed as channel description.
    #[serde(default = "default_code_output_channel_id")]
    pub code_output_channel_id: u64,

    /// Absolute or relative path to the Piper binary.
    #[serde(default = "default_piper_binary_path")]
    pub piper_binary_path: String,

    /// Directory containing Piper voice model files.
    #[serde(default = "default_piper_voice_dir")]
    pub piper_voice_dir: String,

    /// Binary name or absolute path for ffmpeg.
    #[serde(default = "default_ffmpeg_binary_path")]
    pub ffmpeg_binary_path: String,

    /// Binary name or absolute path for yt-dlp.
    #[serde(default = "default_yt_dlp_binary_path")]
    pub yt_dlp_binary_path: String,

    /// Initial volume (0-100) when starting music/radio playback.
    #[serde(default = "default_music_start_volume")]
    pub music_start_volume: u8,
}

// ── Defaults ────────────────────────────────────────────────

fn default_address() -> String {
    "localhost".into()
}
fn default_bot_name() -> String {
    "AI Support Agent".into()
}
fn default_key_file() -> String {
    "identity.key".into()
}
fn default_disconnect_message() -> String {
    "AI Support Agent shutting down. Goodbye!".into()
}
fn default_rate_limit() -> u8 {
    2
}
fn default_ai_api_url() -> String {
    "https://integrate.api.nvidia.com/v1/chat/completions".into()
}
fn default_ai_api_key() -> String {
    "".into()
}
fn default_model() -> String {
    "kimi-k2.5:cloud".into()
}
fn default_fallback_api_url() -> String {
    "http://localhost:11434/v1/chat/completions".into()
}
fn default_fallback_api_key() -> String {
    "".into()
}
fn default_fallback_model() -> String {
    "minimax-m2.5:cloud".into()
}
fn default_ai_timeout() -> u64 {
    60
}
fn default_admin_uids() -> Vec<String> {
    Vec::new()
}
fn default_moderator_uids() -> Vec<String> {
    Vec::new()
}
fn default_audit_log_file() -> String {
    "audit.log".into()
}

fn default_allowed_server_groups() -> std::collections::HashMap<String, u64> {
    let mut map = std::collections::HashMap::new();
    map.insert("CS 1.6".into(), 56);
    map.insert("CS:2".into(), 57);
    map.insert("Valorant".into(), 180);
    map.insert("LOL".into(), 58);
    map.insert("DOTA".into(), 62);
    map.insert("FORTNITE".into(), 60);
    map.insert("Minecraft".into(), 112);
    map.insert("PUBG".into(), 61);
    map.insert("AMONG US".into(), 346);
    map.insert("BEBO".into(), 471);
    map.insert("PAYDAY".into(), 162);
    map.insert("RageMP".into(), 835);
    map.insert("SAMP".into(), 67);
    map.insert("WOT".into(), 69);
    map.insert("ZULA".into(), 59);
    map.insert("Esea".into(), 176);
    map.insert("Faceit".into(), 168);
    map
}

fn default_prompt_workspace_dir() -> String {
    "workspace".into()
}

fn default_prompt_file_max_chars() -> usize {
    6000
}

fn default_prompt_total_max_chars() -> usize {
    24000
}

fn default_auto_learning_enabled() -> bool {
    true
}

fn default_auto_learning_note_limit() -> usize {
    12
}

fn default_tickets_file() -> String {
    "tickets.json".into()
}

fn default_identities_file() -> String {
    "identities.json".into()
}

fn default_radios_file() -> String {
    "radios.json".into()
}

fn default_channel_admin_group_id() -> u64 {
    24
}

fn default_channel_order_anchor_id() -> Option<u64> {
    Some(328)
}

fn default_code_output_channel_id() -> u64 {
    51494
}

fn default_piper_binary_path() -> String {
    "piper/piper/piper".into()
}

fn default_piper_voice_dir() -> String {
    "piper/voices".into()
}

fn default_ffmpeg_binary_path() -> String {
    "ffmpeg".into()
}

fn default_yt_dlp_binary_path() -> String {
    "yt-dlp".into()
}

fn default_music_start_volume() -> u8 {
    5
}

impl Default for BotConfig {
    fn default() -> Self {
        Self {
            server_address: default_address(),
            bot_name: default_bot_name(),
            channel: None,
            key_file: default_key_file(),
            disconnect_message: default_disconnect_message(),
            rate_limit: default_rate_limit(),
            ai_api_url: default_ai_api_url(),
            ai_api_key: default_ai_api_key(),
            default_model: default_model(),
            fallback_api_url: default_fallback_api_url(),
            fallback_api_key: default_fallback_api_key(),
            fallback_model: default_fallback_model(),
            ai_timeout_secs: default_ai_timeout(),
            admin_uids: default_admin_uids(),
            moderator_uids: default_moderator_uids(),
            audit_log_file: default_audit_log_file(),
            allowed_server_groups: default_allowed_server_groups(),
            prompt_workspace_dir: default_prompt_workspace_dir(),
            prompt_file_max_chars: default_prompt_file_max_chars(),
            prompt_total_max_chars: default_prompt_total_max_chars(),
            auto_learning_enabled: default_auto_learning_enabled(),
            auto_learning_note_limit: default_auto_learning_note_limit(),
            tickets_file: default_tickets_file(),
            identities_file: default_identities_file(),
            radios_file: default_radios_file(),
            channel_admin_group_id: default_channel_admin_group_id(),
            channel_order_anchor_id: default_channel_order_anchor_id(),
            code_output_channel_id: default_code_output_channel_id(),
            piper_binary_path: default_piper_binary_path(),
            piper_voice_dir: default_piper_voice_dir(),
            ffmpeg_binary_path: default_ffmpeg_binary_path(),
            yt_dlp_binary_path: default_yt_dlp_binary_path(),
            music_start_volume: default_music_start_volume(),
        }
    }
}

/// Load configuration from a TOML file at the given path.
/// Falls back to defaults if the file doesn't exist.
pub fn load_config(path: &str) -> Result<BotConfig> {
    let path = Path::new(path);
    if !path.exists() {
        tracing::warn!("Config file not found at {:?}, using defaults", path);
        return Ok(apply_env_overrides(BotConfig::default()));
    }

    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file: {:?}", path))?;

    let config: BotConfig =
        toml::from_str(&content).with_context(|| "Failed to parse config file")?;

    Ok(apply_env_overrides(config))
}

fn apply_env_overrides(mut config: BotConfig) -> BotConfig {
    if let Ok(value) = env::var("TS3_BOT_AI_API_KEY") {
        if !value.trim().is_empty() {
            config.ai_api_key = value;
        }
    }

    if let Ok(value) = env::var("TS3_BOT_FALLBACK_API_KEY") {
        if !value.trim().is_empty() {
            config.fallback_api_key = value;
        }
    }

    if let Ok(value) = env::var("TS3_BOT_SERVER_ADDRESS") {
        if !value.trim().is_empty() {
            config.server_address = value;
        }
    }

    if let Ok(value) = env::var("TS3_BOT_CHANNEL") {
        if !value.trim().is_empty() {
            config.channel = Some(value);
        }
    }

    if let Ok(value) = env::var("TS3_BOT_FFMPEG_PATH") {
        if !value.trim().is_empty() {
            config.ffmpeg_binary_path = value;
        }
    }

    if let Ok(value) = env::var("TS3_BOT_YT_DLP_PATH") {
        if !value.trim().is_empty() {
            config.yt_dlp_binary_path = value;
        }
    }

    config
}
