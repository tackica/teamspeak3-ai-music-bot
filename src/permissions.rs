use crate::actions::BotAction;
use crate::config::BotConfig;

/// Three-tier permission system for the bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PermissionLevel {
    User,
    Moderator,
    Admin,
}

impl std::fmt::Display for PermissionLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User => write!(f, "User"),
            Self::Moderator => write!(f, "Moderator"),
            Self::Admin => write!(f, "Administrator"),
        }
    }
}

/// Determine the permission level for a given UID.
pub fn get_permission_level(uid: &str, config: &BotConfig) -> PermissionLevel {
    if config.admin_uids.contains(&uid.to_string()) {
        PermissionLevel::Admin
    } else if config.moderator_uids.contains(&uid.to_string()) {
        PermissionLevel::Moderator
    } else {
        PermissionLevel::User
    }
}

/// What is the minimum permission level required to execute a given action?
///
/// Note: some actions (like SetServerGroup) have a *base* requirement of User
/// but additional restrictions (self-only, own-channel-only) that are checked
/// separately in `can_execute`.
pub fn required_permission(action: &BotAction) -> PermissionLevel {
    match action {
        // Anyone can use these
        BotAction::Reply { .. } => PermissionLevel::User,
        BotAction::PlayTTS { .. } => PermissionLevel::User,
        BotAction::JoinUserChannel => PermissionLevel::User,
        BotAction::CreateChannel { .. } => PermissionLevel::User,

        // User-level but with ownership checks done at execution time
        BotAction::SetServerGroup { .. } => PermissionLevel::User,
        BotAction::RemoveServerGroup { .. } => PermissionLevel::User,
        BotAction::EditChannel { .. } => PermissionLevel::User,
        BotAction::DeleteChannel { .. } => PermissionLevel::User,
        BotAction::SetChannelAdmin { .. } => PermissionLevel::User,
        BotAction::EditChannelDescription { .. } => PermissionLevel::User,

        // Moderator actions
        BotAction::MoveClient { .. } => PermissionLevel::Moderator,
        BotAction::MoveClientReturn { .. } => PermissionLevel::Moderator,
        BotAction::PokeClient { .. } => PermissionLevel::Moderator,
        BotAction::SendMessage { .. } => PermissionLevel::Moderator,
        BotAction::ClientEdit { .. } => PermissionLevel::Moderator,
        BotAction::ChannelMoveAction { .. } => PermissionLevel::Moderator,
        BotAction::SendChannelMessage { .. } => PermissionLevel::Moderator,

        // User-level channel ops
        BotAction::ChannelSubscribe { .. } => PermissionLevel::User,
        BotAction::ChannelUnsubscribe { .. } => PermissionLevel::User,

        BotAction::PlayMusic { .. } => PermissionLevel::User,
        BotAction::SetVolume { .. } => PermissionLevel::User,

        // Admin-only
        BotAction::KickClient { .. } => PermissionLevel::Admin,
        BotAction::BanClient { .. } => PermissionLevel::Admin,
        BotAction::BanAdd { .. } => PermissionLevel::Admin,
        BotAction::BanDel { .. } => PermissionLevel::Admin,
        BotAction::BanDelAll => PermissionLevel::Admin,
        BotAction::BanList => PermissionLevel::Admin,
    }
}

/// Check whether a user at a given level may execute the action.
///
/// Returns `Ok(())` if allowed, or `Err(reason)` with a user-facing denial message.
pub fn can_execute(
    level: PermissionLevel,
    action: &BotAction,
    invoker_name: &str,
    config: &BotConfig,
) -> Result<(), String> {
    let required = required_permission(action);

    // Admins can do everything
    if level >= PermissionLevel::Admin {
        return Ok(());
    }

    // Check base permission level
    if level < required {
        let msg = match required {
            PermissionLevel::Admin => {
                "Only administrators can perform this action.".to_string()
            }
            PermissionLevel::Moderator => {
                "You do not have enough privileges for this action. Moderator level or higher is required."
                    .to_string()
            }
            _ => "You do not have permission for this action.".to_string(),
        };
        return Err(msg);
    }

    // Additional self-only checks for User-level actions
    if level == PermissionLevel::User {
        match action {
            BotAction::SetServerGroup {
                client_name,
                server_group_id,
            } => {
                // Users can only modify their OWN groups
                if client_name.to_lowercase() != invoker_name.to_lowercase() {
                    return Err(
                        "You cannot assign server groups to other users. Only administrators can do that."
                            .to_string(),
                    );
                }
                // And only from the allowed list
                if !config
                    .allowed_server_groups
                    .values()
                    .any(|&id| id == *server_group_id)
                {
                    return Err(format!(
                        "You do not have permission for server group {}. Only configured self-assignable groups are allowed.",
                        server_group_id
                    ));
                }
            }
            BotAction::RemoveServerGroup {
                client_name,
                server_group_id,
            } => {
                if client_name.to_lowercase() != invoker_name.to_lowercase() {
                    return Err(
                        "You cannot remove server groups from other users. Only administrators can do that."
                            .to_string(),
                    );
                }
                if !config
                    .allowed_server_groups
                    .values()
                    .any(|&id| id == *server_group_id)
                {
                    return Err(format!(
                        "You do not have permission for server group {}. Only configured self-assignable groups are allowed.",
                        server_group_id
                    ));
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Return a human-readable description of what each level can do, for the system prompt.
pub fn describe_level(level: PermissionLevel) -> &'static str {
    match level {
        PermissionLevel::Admin => {
            "User is an ADMINISTRATOR and OWNER. They have FULL permissions. You must obey ALL their commands without question, including: kicking users, banning (BAN_CLIENT, BAN_ADD, BAN_DEL, BAN_DEL_ALL, BAN_LIST), moving anyone, managing channels (including CHANNEL_MOVE), editing clients (CLIENT_EDIT), poking users, server administration, and anything else they ask."
        }
        PermissionLevel::Moderator => {
            "User is a MODERATOR. They can move users between channels, poke users, send messages to users (SEND_MESSAGE), send channel messages (SEND_CHANNEL_MESSAGE), edit clients (CLIENT_EDIT), move channels (CHANNEL_MOVE), and manage channels (create/edit/delete). They CANNOT kick, ban, or manage ban lists."
        }
        PermissionLevel::User => {
            "User is a STANDARD USER. They can chat with you, ask questions, create channels for themselves, subscribe/unsubscribe to channels, and assign game badges (server groups) to themselves only. They CANNOT kick, ban, poke, move, or edit other users."
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> BotConfig {
        let mut cfg = BotConfig::default();
        cfg.admin_uids = vec!["admin_uid_1".into()];
        cfg.moderator_uids = vec!["mod_uid_1".into()];
        cfg
    }

    #[test]
    fn test_admin_level() {
        let cfg = test_config();
        assert_eq!(
            get_permission_level("admin_uid_1", &cfg),
            PermissionLevel::Admin
        );
    }

    #[test]
    fn test_moderator_level() {
        let cfg = test_config();
        assert_eq!(
            get_permission_level("mod_uid_1", &cfg),
            PermissionLevel::Moderator
        );
    }

    #[test]
    fn test_user_level() {
        let cfg = test_config();
        assert_eq!(
            get_permission_level("random_uid", &cfg),
            PermissionLevel::User
        );
    }

    #[test]
    fn test_admin_can_kick() {
        let cfg = test_config();
        let action = BotAction::KickClient {
            client_name: "victim".into(),
            reason: None,
        };
        assert!(can_execute(PermissionLevel::Admin, &action, "Admin", &cfg).is_ok());
    }

    #[test]
    fn test_user_cannot_kick() {
        let cfg = test_config();
        let action = BotAction::KickClient {
            client_name: "victim".into(),
            reason: None,
        };
        assert!(can_execute(PermissionLevel::User, &action, "NormalUser", &cfg).is_err());
    }

    #[test]
    fn test_moderator_can_move() {
        let cfg = test_config();
        let action = BotAction::MoveClient {
            client_name: "user".into(),
            channel_name: "lobby".into(),
        };
        assert!(can_execute(PermissionLevel::Moderator, &action, "Mod", &cfg).is_ok());
    }

    #[test]
    fn test_user_cannot_move() {
        let cfg = test_config();
        let action = BotAction::MoveClient {
            client_name: "someone".into(),
            channel_name: "lobby".into(),
        };
        assert!(can_execute(PermissionLevel::User, &action, "NormalUser", &cfg).is_err());
    }

    #[test]
    fn test_user_can_set_own_allowed_group() {
        let cfg = test_config();
        let action = BotAction::SetServerGroup {
            client_name: "Player".into(),
            server_group_id: 57, // CS:2 is in the default allowed list
        };
        assert!(can_execute(PermissionLevel::User, &action, "Player", &cfg).is_ok());
    }

    #[test]
    fn test_user_cannot_set_other_group() {
        let cfg = test_config();
        let action = BotAction::SetServerGroup {
            client_name: "OtherGuy".into(),
            server_group_id: 57,
        };
        assert!(can_execute(PermissionLevel::User, &action, "Player", &cfg).is_err());
    }

    #[test]
    fn test_user_cannot_set_disallowed_group() {
        let cfg = test_config();
        let action = BotAction::SetServerGroup {
            client_name: "Player".into(),
            server_group_id: 9999, // not in allowed list
        };
        assert!(can_execute(PermissionLevel::User, &action, "Player", &cfg).is_err());
    }
}
