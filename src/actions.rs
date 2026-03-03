use serde::{Deserialize, Serialize};
use tracing::warn;

/// Represents an action the bot should perform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BotAction {
    /// Send a text reply back to the user.
    Reply {
        message: String,
    },

    /// Create a new channel on the server.
    CreateChannel {
        channel_name: String,
        password: Option<String>,
        permanent: bool,
    },

    /// Edit an existing channel (e.g. make it permanent).
    EditChannel {
        channel_name: String,
        set_permanent: bool,
    },

    /// Edit a channel's description.
    EditChannelDescription {
        channel_id: u64,
        description: String,
    },

    /// Delete an existing channel.
    DeleteChannel {
        channel_name: String,
    },

    /// Give Channel Admin to a specific user.
    SetChannelAdmin {
        channel_name: String,
        client_name: Option<String>,
    },

    /// Kick a user from the server.
    KickClient {
        client_name: String,
        reason: Option<String>,
    },

    /// Move a user to a specific channel.
    MoveClient {
        client_name: String,
        channel_name: String,
    },

    /// Return a user to the channel they were in before the last move.
    MoveClientReturn {
        client_name: String,
    },

    /// Add or remove a server group from a user.
    SetServerGroup {
        client_name: String,
        server_group_id: u64,
    },

    /// Remove a server group from a user.
    RemoveServerGroup {
        client_name: String,
        server_group_id: u64,
    },

    /// Poke a user with a message.
    PokeClient {
        client_name: String,
        message: String,
    },

    /// Join the channel of the user who invoked this action.
    JoinUserChannel,

    /// Ban a user from the server (admin only).
    BanClient {
        client_name: String,
        reason: Option<String>,
        duration_seconds: Option<u64>,
    },

    /// Send a message to a specific user.
    SendMessage {
        target_name: String,
        message: String,
    },

    /// Ban by IP, UID, or Name (without requiring user to be online).
    BanAdd {
        ip: Option<String>,
        uid: Option<String>,
        name: Option<String>,
        reason: Option<String>,
        duration_seconds: Option<u64>,
    },

    /// Remove a specific ban by ID.
    BanDel {
        ban_id: u64,
    },

    /// Remove all bans.
    BanDelAll,

    /// List all active bans.
    BanList,

    /// Edit client properties (description, is_talker).
    ClientEdit {
        client_name: String,
        description: Option<String>,
        is_talker: Option<bool>,
    },

    /// Move a channel to a new parent.
    ChannelMoveAction {
        channel_name: String,
        parent_channel_name: String,
    },

    /// Subscribe to a channel.
    ChannelSubscribe {
        channel_name: String,
    },

    /// Unsubscribe from a channel.
    ChannelUnsubscribe {
        channel_name: String,
    },

    /// Send a message to a channel.
    SendChannelMessage {
        channel_name: String,
        message: String,
    },
    PlayTTS {
        text: String,
    },
    /// Play music from a URL.
    PlayMusic {
        url: String,
    },
    /// Set the bot's audio volume (0-100).
    SetVolume {
        volume: u8,
    },
}

/// Raw JSON payload deserialized from the LLM's output.
#[derive(Debug, Deserialize)]
struct ActionPayload {
    action: String,

    // Fields for REPLY
    message: Option<String>,

    // Fields for CREATE_CHANNEL
    channel_name: Option<String>,
    password: Option<String>,
    permanent: Option<bool>,

    // Fields for EDIT_CHANNEL
    set_permanent: Option<bool>,

    // Fields for new extensions
    client_name: Option<String>,
    reason: Option<String>,

    // Fields for setting channel description
    channel_id: Option<u64>,
    description: Option<String>,

    // Fields for server groups
    server_group_id: Option<u64>,

    // Fields for ban
    duration_seconds: Option<u64>,

    // Fields for send_message
    target_name: Option<String>,

    // Fields for BAN_ADD
    ip: Option<String>,
    uid: Option<String>,
    name: Option<String>,

    // Fields for BAN_DEL
    ban_id: Option<u64>,

    // Fields for CLIENT_EDIT
    is_talker: Option<bool>,

    // Fields for CHANNEL_MOVE
    parent_channel_name: Option<String>,

    // Fields for PLAY_TTS
    text: Option<String>,

    // Fields for PLAY_MUSIC
    url: Option<String>,

    // Fields for SET_VOLUME
    volume: Option<u8>,
}

/// Parse the raw AI response string into a list of `BotAction`s.
///
/// The LLM is instructed to output one JSON object per line. This function:
/// 1. Extracts all JSON objects from the response.
/// 2. Deserializes each into an `ActionPayload`.
/// 3. Converts known action types into `BotAction` variants.
/// 4. Falls back to treating the entire response as a text reply if no valid JSON is found.
pub fn parse_ai_response(raw: &str) -> Vec<BotAction> {
    let mut actions = Vec::new();
    let cleaned_text = clean_response_text(raw);

    // 1. Try to parse the entire response as a single JSON array of ActionPayloads
    if let Ok(payload_array) = serde_json::from_str::<Vec<ActionPayload>>(&cleaned_text) {
        for payload in payload_array {
            if let Some(action) = convert_payload(payload) {
                actions.push(action);
            }
        }
    }

    // 2. If it wasn't an array, try extracting individual JSON objects (like {"action": ...}) from the text
    if actions.is_empty() {
        actions.extend(extract_embedded_json(&cleaned_text));
    }

    // 3. Fallback parser for model-specific tool wrappers (e.g. minimax <invoke ...>)
    if actions.is_empty() {
        actions.extend(extract_invoke_actions(&cleaned_text));
    }

    if actions.is_empty()
        && (cleaned_text.contains("<invoke") || cleaned_text.contains("tool_call"))
    {
        warn!("AI response looked like tool call format, but no actions were parsed");
    }

    // 4. Fallback: treat the entire clean text as a Reply if no JSON actions were found
    if actions.is_empty() && !cleaned_text.is_empty() {
        actions.push(BotAction::Reply {
            message: cleaned_text,
        });
    }

    actions
}

/// Convert a deserialized `ActionPayload` into a `BotAction`.
fn convert_payload(payload: ActionPayload) -> Option<BotAction> {
    match payload.action.to_uppercase().as_str() {
        "REPLY" => {
            let message = payload.message.unwrap_or_default();
            if message.is_empty() {
                warn!("REPLY action with empty message, skipping");
                return None;
            }
            Some(BotAction::Reply { message })
        }

        "CREATE_CHANNEL" => {
            let channel_name = payload.channel_name.unwrap_or_default();
            if channel_name.is_empty() {
                warn!("CREATE_CHANNEL action with no channel_name, skipping");
                return None;
            }
            // Validate channel name length
            if channel_name.len() > 100 {
                warn!(
                    name = %channel_name,
                    "Channel name exceeds 100 characters, rejecting"
                );
                return Some(BotAction::Reply {
                    message: "Sorry, channel names must be 100 characters or less.".into(),
                });
            }
            Some(BotAction::CreateChannel {
                channel_name,
                password: payload.password.filter(|p| !p.is_empty()),
                permanent: payload.permanent.unwrap_or(false),
            })
        }

        "EDIT_CHANNEL" => {
            let channel_name = payload.channel_name.unwrap_or_default();
            if channel_name.is_empty() {
                warn!("EDIT_CHANNEL action with no channel_name, skipping");
                return None;
            }
            Some(BotAction::EditChannel {
                channel_name,
                set_permanent: payload.set_permanent.unwrap_or(true),
            })
        }

        "DELETE_CHANNEL" => {
            let channel_name = payload.channel_name.unwrap_or_default();
            if channel_name.is_empty() {
                warn!("DELETE_CHANNEL action with no channel_name, skipping");
                return None;
            }
            Some(BotAction::DeleteChannel { channel_name })
        }

        "SET_CHANNEL_DESCRIPTION" => {
            let channel_id = payload.channel_id;
            let description = payload.description.unwrap_or_default();
            if let Some(id) = channel_id {
                Some(BotAction::EditChannelDescription {
                    channel_id: id,
                    description,
                })
            } else {
                warn!("SET_CHANNEL_DESCRIPTION action with no channel_id, skipping");
                None
            }
        }

        "SET_CHANNEL_ADMIN" => {
            let channel_name = payload.channel_name.unwrap_or_default();
            if channel_name.is_empty() {
                warn!("SET_CHANNEL_ADMIN action with no channel_name, skipping");
                return None;
            }
            Some(BotAction::SetChannelAdmin {
                channel_name,
                client_name: payload.client_name,
            })
        }

        "KICK_CLIENT" => {
            let client_name = payload.client_name.unwrap_or_default();
            if client_name.is_empty() {
                warn!("KICK_CLIENT action with no client_name, skipping");
                return None;
            }
            Some(BotAction::KickClient {
                client_name,
                reason: payload.reason,
            })
        }

        "MOVE_CLIENT" => {
            let client_name = payload.client_name.unwrap_or_default();
            let channel_name = payload.channel_name.unwrap_or_default();
            if client_name.is_empty() || channel_name.is_empty() {
                warn!("MOVE_CLIENT action missing fields, skipping");
                return None;
            }
            Some(BotAction::MoveClient {
                client_name,
                channel_name,
            })
        }

        "MOVE_CLIENT_RETURN" => {
            let client_name = payload.client_name.unwrap_or_default();
            if client_name.is_empty() {
                warn!("MOVE_CLIENT_RETURN action missing client_name, skipping");
                return None;
            }
            Some(BotAction::MoveClientReturn { client_name })
        }

        "SET_SERVER_GROUP" => {
            let client_name = payload.client_name.unwrap_or_default();
            let server_group_id = payload.server_group_id;
            if client_name.is_empty() {
                warn!("SET_SERVER_GROUP action missing client_name, skipping");
                return None;
            }
            if let Some(sgid) = server_group_id {
                Some(BotAction::SetServerGroup {
                    client_name,
                    server_group_id: sgid,
                })
            } else {
                warn!("SET_SERVER_GROUP action missing server_group_id, skipping");
                None
            }
        }

        "REMOVE_SERVER_GROUP" => {
            let client_name = payload.client_name.unwrap_or_default();
            let server_group_id = payload.server_group_id;
            if client_name.is_empty() {
                warn!("REMOVE_SERVER_GROUP action missing client_name, skipping");
                return None;
            }
            if let Some(sgid) = server_group_id {
                Some(BotAction::RemoveServerGroup {
                    client_name,
                    server_group_id: sgid,
                })
            } else {
                warn!("REMOVE_SERVER_GROUP action missing server_group_id, skipping");
                None
            }
        }

        "POKE_CLIENT" => {
            let client_name = payload.client_name.unwrap_or_default();
            let message = payload.message.unwrap_or_default();
            if client_name.is_empty() {
                warn!("POKE_CLIENT action with no client_name, skipping");
                return None;
            }
            Some(BotAction::PokeClient {
                client_name,
                message,
            })
        }

        "JOIN_USER_CHANNEL" => Some(BotAction::JoinUserChannel),

        "BAN_CLIENT" => {
            let client_name = payload.client_name.unwrap_or_default();
            if client_name.is_empty() {
                warn!("BAN_CLIENT action with no client_name, skipping");
                return None;
            }
            Some(BotAction::BanClient {
                client_name,
                reason: payload.reason,
                duration_seconds: payload.duration_seconds,
            })
        }

        "SEND_MESSAGE" => {
            let target_name = payload.target_name.unwrap_or_default();
            let message = payload.message.unwrap_or_default();
            if target_name.is_empty() || message.is_empty() {
                warn!("SEND_MESSAGE action missing fields, skipping");
                return None;
            }
            Some(BotAction::SendMessage {
                target_name,
                message,
            })
        }

        "BAN_ADD" => {
            let ip = payload.ip;
            let uid = payload.uid;
            let name = payload.name;
            if ip.is_none() && uid.is_none() && name.is_none() {
                warn!("BAN_ADD action needs at least one of ip/uid/name, skipping");
                return None;
            }
            Some(BotAction::BanAdd {
                ip,
                uid,
                name,
                reason: payload.reason,
                duration_seconds: payload.duration_seconds,
            })
        }

        "BAN_DEL" => {
            if let Some(ban_id) = payload.ban_id {
                Some(BotAction::BanDel { ban_id })
            } else {
                warn!("BAN_DEL action missing ban_id, skipping");
                None
            }
        }

        "BAN_DEL_ALL" => Some(BotAction::BanDelAll),

        "BAN_LIST" => Some(BotAction::BanList),

        "CLIENT_EDIT" => {
            let client_name = payload.client_name.unwrap_or_default();
            if client_name.is_empty() {
                warn!("CLIENT_EDIT action missing client_name, skipping");
                return None;
            }
            Some(BotAction::ClientEdit {
                client_name,
                description: payload.description,
                is_talker: payload.is_talker,
            })
        }

        "CHANNEL_MOVE" => {
            let channel_name = payload.channel_name.unwrap_or_default();
            let parent = payload.parent_channel_name.unwrap_or_default();
            if channel_name.is_empty() || parent.is_empty() {
                warn!("CHANNEL_MOVE action missing fields, skipping");
                return None;
            }
            Some(BotAction::ChannelMoveAction {
                channel_name,
                parent_channel_name: parent,
            })
        }

        "CHANNEL_SUBSCRIBE" => {
            let channel_name = payload.channel_name.unwrap_or_default();
            if channel_name.is_empty() {
                warn!("CHANNEL_SUBSCRIBE action missing channel_name, skipping");
                return None;
            }
            Some(BotAction::ChannelSubscribe { channel_name })
        }

        "CHANNEL_UNSUBSCRIBE" => {
            let channel_name = payload.channel_name.unwrap_or_default();
            if channel_name.is_empty() {
                warn!("CHANNEL_UNSUBSCRIBE action missing channel_name, skipping");
                return None;
            }
            Some(BotAction::ChannelUnsubscribe { channel_name })
        }

        "SEND_CHANNEL_MESSAGE" => {
            let channel_name = payload.channel_name.unwrap_or_default();
            let message = payload.message.unwrap_or_default();
            if channel_name.is_empty() || message.is_empty() {
                warn!("SEND_CHANNEL_MESSAGE action missing fields, skipping");
                return None;
            }
            Some(BotAction::SendChannelMessage {
                channel_name,
                message,
            })
        }

        "PLAY_TTS" => {
            let text = payload.text.unwrap_or_default();
            if text.is_empty() {
                warn!("PLAY_TTS action missing text, skipping");
                return None;
            }
            Some(BotAction::PlayTTS { text })
        }

        "PLAY_MUSIC" => {
            let url = payload.url.unwrap_or_default();
            if url.is_empty() {
                warn!("PLAY_MUSIC action missing url, skipping");
                return None;
            }
            Some(BotAction::PlayMusic { url })
        }

        "SET_VOLUME" => {
            if let Some(volume) = payload.volume {
                Some(BotAction::SetVolume {
                    volume: volume.clamp(0, 100),
                })
            } else {
                warn!("SET_VOLUME action missing volume, skipping");
                None
            }
        }

        other => {
            warn!(action = other, "Unknown action type, ignoring");
            None
        }
    }
}

/// Try to extract JSON objects embedded within prose text.
fn extract_embedded_json(text: &str) -> Vec<BotAction> {
    let mut actions = Vec::new();
    let mut search_from = 0;

    while search_from < text.len() {
        if let Some(start) = text[search_from..].find('{') {
            let abs_start = search_from + start;
            // Find the matching closing brace
            let mut depth = 0;
            let mut end = None;
            for (i, ch) in text[abs_start..].char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(abs_start + i + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }

            if let Some(end_pos) = end {
                let json_str = &text[abs_start..end_pos];
                if let Ok(payload) = serde_json::from_str::<ActionPayload>(json_str) {
                    if let Some(action) = convert_payload(payload) {
                        actions.push(action);
                    }
                }
                search_from = end_pos;
            } else {
                search_from = abs_start + 1;
            }
        } else {
            break;
        }
    }

    actions
}

/// Parse model-specific tool-call wrappers like:
/// <minimax:tool_call>
/// <invoke name="PLAY_TTS", "text": "Hello" }
fn extract_invoke_actions(text: &str) -> Vec<BotAction> {
    let mut actions = Vec::new();
    let mut cursor = 0usize;

    while cursor < text.len() {
        let Some(invoke_rel) = text[cursor..].find("<invoke") else {
            break;
        };
        let invoke_start = cursor + invoke_rel;

        let Some(name_marker_rel) = text[invoke_start..].find("name=\"") else {
            cursor = invoke_start + "<invoke".len();
            continue;
        };
        let name_start = invoke_start + name_marker_rel + "name=\"".len();
        let Some(name_end_rel) = text[name_start..].find('"') else {
            break;
        };
        let name_end = name_start + name_end_rel;
        let action_name = text[name_start..name_end].trim();

        let payload_start = name_end + 1;
        let next_invoke = text[payload_start..]
            .find("<invoke")
            .map(|i| payload_start + i);
        let next_close = text[payload_start..]
            .find("</invoke>")
            .map(|i| payload_start + i);
        let next_tool_marker = text[payload_start..]
            .find("<minimax:tool_call")
            .map(|i| payload_start + i);

        let payload_end = [next_invoke, next_close, next_tool_marker]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(text.len());

        let payload = &text[payload_start..payload_end];
        if let Some(action) = convert_invoke_payload(action_name, payload) {
            actions.push(action);
        }

        if payload_end <= invoke_start {
            cursor = invoke_start + "<invoke".len();
        } else {
            cursor = payload_end;
        }
    }

    actions
}

fn trim_unmatched_trailing_braces(mut s: String) -> String {
    loop {
        let open_count = s.chars().filter(|&c| c == '{').count();
        let close_count = s.chars().filter(|&c| c == '}').count();
        if close_count <= open_count {
            break;
        }

        let trimmed = s.trim_end();
        if !trimmed.ends_with('}') {
            break;
        }
        s = trimmed[..trimmed.len() - 1].to_string();
    }
    s
}

fn parse_payload_json(json_obj: &str) -> Option<BotAction> {
    serde_json::from_str::<ActionPayload>(json_obj)
        .ok()
        .and_then(convert_payload)
}

fn convert_invoke_payload(action_name: &str, payload: &str) -> Option<BotAction> {
    let action_name = action_name.trim().to_uppercase();
    if action_name.is_empty() {
        return None;
    }

    // Try embedded JSON object first (if model included explicit braces)
    if let Some(start) = payload.find('{') {
        if let Some(end) = payload.rfind('}') {
            if end > start {
                let obj = payload[start..=end].trim();

                if let Some(action) = parse_payload_json(obj) {
                    return Some(action);
                }

                if !obj.contains("\"action\"") {
                    let inner = obj
                        .trim()
                        .trim_start_matches('{')
                        .trim_end_matches('}')
                        .trim();
                    let injected = if inner.is_empty() {
                        format!(r#"{{"action":"{}"}}"#, action_name)
                    } else {
                        format!(r#"{{"action":"{}",{}}}"#, action_name, inner)
                    };
                    if let Some(action) = parse_payload_json(&injected) {
                        return Some(action);
                    }
                }
            }
        }
    }

    // Fallback: key/value fragment after invoke name (e.g. , "text": "..." }} )
    let mut body = payload.trim().trim_start_matches('>').trim().to_string();
    body = trim_unmatched_trailing_braces(body);
    let body = body.trim().trim_start_matches(',').trim().to_string();
    let body = trim_unmatched_trailing_braces(body);

    let body_inner = body
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .trim()
        .trim_end_matches(',')
        .trim()
        .to_string();

    let injected = if body_inner.is_empty() {
        format!(r#"{{"action":"{}"}}"#, action_name)
    } else {
        format!(r#"{{"action":"{}",{}}}"#, action_name, body_inner)
    };

    parse_payload_json(&injected)
}

/// Remove common LLM artifacts from a response (thinking tags, code fences, etc.).
fn clean_response_text(raw: &str) -> String {
    let mut text = raw.to_string();

    // Remove <think>...</think> blocks (deepseek-r1 uses these)
    while let Some(start) = text.find("<think>") {
        if let Some(end) = text.find("</think>") {
            text = format!("{}{}", &text[..start], &text[end + 8..]);
        } else {
            // Unclosed think tag — remove everything from <think> onward
            text = text[..start].to_string();
            break;
        }
    }

    let mut text = text.trim().to_string();

    // Remove markdown code fences if the AI wrapped the JSON
    if text.starts_with("```json") {
        text = text["```json".len()..].to_string();
    } else if text.starts_with("```") {
        text = text["```".len()..].to_string();
    }

    if text.ends_with("```") {
        text = text[..text.len() - 3].to_string();
    }

    // Remove minimax wrapper tags while preserving invoke payloads.
    text = text.replace("<minimax:tool_call>", "");
    text = text.replace("</minimax:tool_call>", "");

    text.trim().to_string()
}

/// Get only the user-facing reply messages from a list of actions.
pub fn get_reply_text(actions: &[BotAction]) -> String {
    actions
        .iter()
        .filter_map(|a| match a {
            BotAction::Reply { message } => Some(message.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_reply_action() {
        let raw = r#"{"action": "REPLY", "message": "Hello there!"}"#;
        let actions = parse_ai_response(raw);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            BotAction::Reply { message } => assert_eq!(message, "Hello there!"),
            _ => panic!("Expected Reply action"),
        }
    }

    #[test]
    fn test_parse_create_channel() {
        let raw = r#"{"action": "CREATE_CHANNEL", "channel_name": "Gaming Lounge", "permanent": true}
{"action": "REPLY", "message": "Done! Created Gaming Lounge."}"#;
        let actions = parse_ai_response(raw);
        assert_eq!(actions.len(), 2);
        match &actions[0] {
            BotAction::CreateChannel {
                channel_name,
                password,
                permanent,
            } => {
                assert_eq!(channel_name, "Gaming Lounge");
                assert_eq!(*password, None);
                assert!(*permanent);
            }
            _ => panic!("Expected CreateChannel action"),
        }
    }

    #[test]
    fn test_parse_edit_channel() {
        let raw = r#"{"action": "EDIT_CHANNEL", "channel_name": "Temp Room", "set_permanent": true}
{"action": "REPLY", "message": "Converted to permanent."}"#;
        let actions = parse_ai_response(raw);
        assert_eq!(actions.len(), 2);
        match &actions[0] {
            BotAction::EditChannel {
                channel_name,
                set_permanent,
            } => {
                assert_eq!(channel_name, "Temp Room");
                assert!(*set_permanent);
            }
            _ => panic!("Expected EditChannel action"),
        }
    }

    #[test]
    fn test_fallback_to_plain_text() {
        let raw = "I'm just a regular text response with no JSON.";
        let actions = parse_ai_response(raw);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            BotAction::Reply { message } => {
                assert_eq!(message, "I'm just a regular text response with no JSON.");
            }
            _ => panic!("Expected Reply fallback"),
        }
    }

    #[test]
    fn test_unknown_action_ignored() {
        let raw = r#"{"action": "DELETE_SERVER", "reason": "yolo"}"#;
        let actions = parse_ai_response(raw);
        // Unknown action is ignored, and since no valid actions exist,
        // the raw text is treated as a reply fallback
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            BotAction::Reply { .. } => {} // OK — fell back to text
            _ => panic!("Expected Reply fallback for unknown action"),
        }
    }

    #[test]
    fn test_clean_think_tags() {
        let raw = "<think>internal reasoning</think>Hello user!";
        let cleaned = clean_response_text(raw);
        assert_eq!(cleaned, "Hello user!");
    }

    #[test]
    fn test_embedded_json() {
        let raw = r#"Sure! Let me create that for you.
{"action": "CREATE_CHANNEL", "channel_name": "My Room", "permanent": true}
Here you go!"#;
        let actions = parse_ai_response(raw);
        // Should find the embedded CREATE_CHANNEL JSON
        assert!(actions
            .iter()
            .any(|a| matches!(a, BotAction::CreateChannel { .. })));
    }

    #[test]
    fn test_parse_minimax_invoke_tts_payload() {
        let raw = r#"
<minimax:tool_call>
<invoke name="PLAY_TTS",
    "text": "TTS is now ON! From now on I will speak everything instead of typing."
  }
}
"#;
        let actions = parse_ai_response(raw);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            BotAction::PlayTTS { text } => {
                assert!(text.starts_with("TTS is now ON"));
            }
            _ => panic!("Expected PlayTTS action"),
        }
    }

    #[test]
    fn test_parse_multiple_invoke_actions() {
        let raw = r#"
<minimax:tool_call>
<invoke name="SET_VOLUME",
    "volume": 30
  }
}
<invoke name="PLAY_TTS",
    "text": "Test, one two three"
  }
}
"#;
        let actions = parse_ai_response(raw);
        assert_eq!(actions.len(), 2);
        assert!(matches!(actions[0], BotAction::SetVolume { volume: 30 }));
        assert!(matches!(actions[1], BotAction::PlayTTS { .. }));
    }
}
