use std::fs::OpenOptions;
use std::io::Write;
use tracing::{info, warn};

/// Result of an audited action.
#[derive(Debug, Clone)]
pub enum AuditResult {
    Success,
    Denied(String),
    Error(String),
}

impl std::fmt::Display for AuditResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Success => write!(f, "Success"),
            Self::Denied(r) => write!(f, "Denied ({})", r),
            Self::Error(e) => write!(f, "Error ({})", e),
        }
    }
}

/// A single audit log entry.
pub struct AuditEntry {
    pub invoker_name: String,
    pub invoker_uid: String,
    pub action: String,
    pub target: Option<String>,
    pub result: AuditResult,
}

/// Simple file-based audit logger.
pub struct AuditLogger {
    file_path: String,
}

impl AuditLogger {
    pub fn new(file_path: &str) -> Self {
        Self {
            file_path: file_path.to_string(),
        }
    }

    /// Write an audit entry to both the log file and tracing.
    pub fn log(&self, entry: AuditEntry) {
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let target_str = entry
            .target
            .as_deref()
            .map(|t| format!(" -> target: \"{}\"", t))
            .unwrap_or_default();

        let line = format!(
            "[{}] {} by \"{}\" (UID: {}){} | RESULT: {}\n",
            now, entry.action, entry.invoker_name, entry.invoker_uid, target_str, entry.result
        );

        // Log to tracing at appropriate level
        match &entry.result {
            AuditResult::Success => {
                info!(
                    action = %entry.action,
                    invoker = %entry.invoker_name,
                    uid = %entry.invoker_uid,
                    "AUDIT: action executed successfully"
                );
            }
            AuditResult::Denied(reason) => {
                warn!(
                    action = %entry.action,
                    invoker = %entry.invoker_name,
                    uid = %entry.invoker_uid,
                    reason = %reason,
                    "AUDIT: action denied"
                );
            }
            AuditResult::Error(err) => {
                warn!(
                    action = %entry.action,
                    invoker = %entry.invoker_name,
                    uid = %entry.invoker_uid,
                    error = %err,
                    "AUDIT: action failed"
                );
            }
        }

        // Append to file
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file_path)
        {
            Ok(mut file) => {
                if let Err(e) = file.write_all(line.as_bytes()) {
                    warn!(error = %e, "Failed to write audit log entry");
                }
            }
            Err(e) => {
                warn!(error = %e, path = %self.file_path, "Failed to open audit log file");
            }
        }
    }
}

/// Helper: extract a human-readable action name from a BotAction for audit logging.
pub fn action_name(action: &crate::actions::BotAction) -> &'static str {
    use crate::actions::BotAction;
    match action {
        BotAction::Reply { .. } => "REPLY",
        BotAction::CreateChannel { .. } => "CREATE_CHANNEL",
        BotAction::EditChannel { .. } => "EDIT_CHANNEL",
        BotAction::DeleteChannel { .. } => "DELETE_CHANNEL",
        BotAction::EditChannelDescription { .. } => "SET_CHANNEL_DESCRIPTION",
        BotAction::SetChannelAdmin { .. } => "SET_CHANNEL_ADMIN",
        BotAction::KickClient { .. } => "KICK_CLIENT",
        BotAction::MoveClient { .. } => "MOVE_CLIENT",
        BotAction::MoveClientReturn { .. } => "MOVE_CLIENT_RETURN",
        BotAction::SetServerGroup { .. } => "SET_SERVER_GROUP",
        BotAction::RemoveServerGroup { .. } => "REMOVE_SERVER_GROUP",
        BotAction::PokeClient { .. } => "POKE_CLIENT",
        BotAction::JoinUserChannel => "JOIN_USER_CHANNEL",
        BotAction::BanClient { .. } => "BAN_CLIENT",
        BotAction::SendMessage { .. } => "SEND_MESSAGE",
        BotAction::BanAdd { .. } => "BAN_ADD",
        BotAction::BanDel { .. } => "BAN_DEL",
        BotAction::BanDelAll => "BAN_DEL_ALL",
        BotAction::BanList => "BAN_LIST",
        BotAction::ClientEdit { .. } => "CLIENT_EDIT",
        BotAction::ChannelMoveAction { .. } => "CHANNEL_MOVE",
        BotAction::ChannelSubscribe { .. } => "CHANNEL_SUBSCRIBE",
        BotAction::ChannelUnsubscribe { .. } => "CHANNEL_UNSUBSCRIBE",
        BotAction::SendChannelMessage { .. } => "SEND_CHANNEL_MESSAGE",
        BotAction::PlayTTS { .. } => "PLAY_TTS",
        BotAction::PlayMusic { .. } => "PLAY_MUSIC",
        BotAction::SetVolume { .. } => "SET_VOLUME",
    }
}

/// Helper: extract the target name from a BotAction, if applicable.
pub fn action_target(action: &crate::actions::BotAction) -> Option<String> {
    use crate::actions::BotAction;
    match action {
        BotAction::KickClient { client_name, .. } => Some(client_name.clone()),
        BotAction::BanClient { client_name, .. } => Some(client_name.clone()),
        BotAction::MoveClient {
            client_name,
            channel_name,
        } => Some(format!("{} -> {}", client_name, channel_name)),
        BotAction::MoveClientReturn { client_name } => Some(client_name.clone()),
        BotAction::PokeClient { client_name, .. } => Some(client_name.clone()),
        BotAction::SendMessage { target_name, .. } => Some(target_name.clone()),
        BotAction::CreateChannel { channel_name, .. } => Some(channel_name.clone()),
        BotAction::EditChannel { channel_name, .. } => Some(channel_name.clone()),
        BotAction::DeleteChannel { channel_name } => Some(channel_name.clone()),
        BotAction::SetChannelAdmin {
            channel_name,
            client_name,
        } => Some(format!(
            "{} in {}",
            client_name.as_deref().unwrap_or("self"),
            channel_name
        )),
        BotAction::SetServerGroup {
            client_name,
            server_group_id,
        } => Some(format!("{} +group {}", client_name, server_group_id)),
        BotAction::RemoveServerGroup {
            client_name,
            server_group_id,
        } => Some(format!("{} -group {}", client_name, server_group_id)),
        BotAction::EditChannelDescription { channel_id, .. } => {
            Some(format!("channel {}", channel_id))
        }
        BotAction::BanAdd { ip, uid, name, .. } => {
            let parts: Vec<&str> = [ip.as_deref(), uid.as_deref(), name.as_deref()]
                .iter()
                .filter_map(|o| *o)
                .collect();
            Some(format!("ban: {}", parts.join(", ")))
        }
        BotAction::BanDel { ban_id } => Some(format!("ban #{}", ban_id)),
        BotAction::BanDelAll => Some("all bans".to_string()),
        BotAction::BanList => Some("list bans".to_string()),
        BotAction::ClientEdit { client_name, .. } => Some(client_name.clone()),
        BotAction::ChannelMoveAction {
            channel_name,
            parent_channel_name,
        } => Some(format!("{} -> {}", channel_name, parent_channel_name)),
        BotAction::ChannelSubscribe { channel_name } => Some(channel_name.clone()),
        BotAction::ChannelUnsubscribe { channel_name } => Some(channel_name.clone()),
        BotAction::SendChannelMessage { channel_name, .. } => Some(channel_name.clone()),
        BotAction::PlayTTS { text } => Some(text.clone()),
        BotAction::PlayMusic { url } => Some(url.clone()),
        BotAction::SetVolume { volume } => Some(format!("{}%", volume)),
        BotAction::Reply { .. } | BotAction::JoinUserChannel => None,
    }
}
