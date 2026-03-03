use tsclientlib::{Connection, MessageTarget};
use tsproto_types::ClientId;

// ─── Server Snapshot ────────────────────────────────────────

/// Compact info about an online user, suitable for prompt injection.
#[derive(Debug, Clone)]
pub struct UserInfo {
    pub name: String,
    pub channel_name: String,
    pub channel_id: u64,
    // Status
    pub is_away: bool,
    pub away_message: Option<String>,
    pub input_muted: bool,
    pub output_muted: bool,
    pub is_recording: bool,
    // Identity
    pub country_code: String,
    pub is_priority_speaker: bool,
    pub is_channel_commander: bool,
    pub server_groups: Vec<String>,
    pub channel_group: String,
    pub database_id: u64,
    pub description: String,
    // Optional data (from clientgetvariables)
    pub platform: Option<String>,
    pub version: Option<String>,
    pub connections_total: Option<u32>,
    // Connection data (from getconnectioninfo, admin-only)
    pub ping_ms: Option<f64>,
    pub idle_secs: Option<i64>,
    pub client_address: Option<String>,
    pub packetloss_total: Option<f32>,
    pub connected_secs: Option<i64>,
}

/// Compact info about a channel in the tree.
#[derive(Debug, Clone)]
pub struct ChannelInfo {
    pub id: u64,
    pub name: String,
    pub client_count: usize,
    pub is_permanent: bool,
    pub topic: Option<String>,
    pub max_clients: Option<i32>,
    pub has_password: bool,
    pub needed_talk_power: Option<i32>,
}

/// Snapshot of the full server state at the moment an AI request is made.
#[derive(Debug, Clone)]
pub struct ServerSnapshot {
    pub server_name: String,
    pub bot_name: String,
    pub bot_channel: String,
    pub bot_uid: String,
    pub platform: String,
    pub version: String,
    pub max_clients: u16,
    // Server stats (from OptionalServerData/ConnectionServerData)
    pub uptime_secs: Option<i64>,
    pub connections_total: Option<u64>,
    pub packetloss_total: Option<f32>,
    pub avg_ping_ms: Option<f64>,
    pub online_users: Vec<UserInfo>,
    pub channel_tree: Vec<ChannelInfo>,
}

impl ServerSnapshot {
    /// Build from the live ts-bookkeeping connection state.
    pub fn from_connection(con: &Connection) -> Option<Self> {
        let state = con.get_state().ok()?;

        // Bot's own info
        let own = state.clients.get(&state.own_client)?;
        let bot_name = own.name.to_string();
        let bot_channel = state
            .channels
            .get(&own.channel)
            .map(|ch| ch.name.to_string())
            .unwrap_or_else(|| format!("(ID {})", own.channel.0));
        let bot_uid = own
            .uid
            .as_ref()
            .map(|u| {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.encode(&u.0)
            })
            .unwrap_or_default();

        // Server info
        let server_name = state.server.name.to_string();
        let platform = state.server.platform.to_string();
        let version = state.server.version.to_string();
        let max_clients = state.server.max_clients;

        // Server stats from OptionalServerData
        let (uptime_secs, connections_total, packetloss_total, avg_ping_ms) =
            match &state.server.optional_data {
                Some(opt) => (
                    Some(opt.uptime.whole_seconds()),
                    Some(opt.connection_count_total),
                    Some(opt.total_packetloss),
                    Some(opt.total_ping.as_seconds_f64() * 1000.0),
                ),
                None => (None, None, None, None),
            };

        // Helper to resolve server group IDs to names
        let resolve_groups =
            |group_ids: &std::collections::HashSet<tsproto_types::ServerGroupId>| -> Vec<String> {
                group_ids
                    .iter()
                    .filter_map(|gid| state.server_groups.get(gid).map(|g| g.name.to_string()))
                    .collect()
            };

        // Helper to resolve channel group ID to name
        let resolve_channel_group = |cgid: tsproto_types::ChannelGroupId| -> String {
            state
                .channel_groups
                .get(&cgid)
                .map(|g| g.name.to_string())
                .unwrap_or_else(|| format!("Group {}", cgid.0))
        };

        // Online users (skip the bot itself)
        let mut online_users = Vec::new();
        for client in state.clients.values() {
            if client.id == state.own_client {
                continue;
            }
            let channel_name = state
                .channels
                .get(&client.channel)
                .map(|ch| ch.name.to_string())
                .unwrap_or_else(|| "?".into());

            // OptionalClientData
            let (platform, version, connections_total) = match &client.optional_data {
                Some(opt) => (
                    Some(opt.platform.to_string()),
                    Some(opt.version.to_string()),
                    Some(opt.connections_total),
                ),
                None => (None, None, None),
            };

            // ConnectionClientData (needs getconnectioninfo)
            let (ping_ms, idle_secs, client_address, packetloss_total, connected_secs) =
                match &client.connection_data {
                    Some(cd) => (
                        cd.ping.map(|d| d.as_seconds_f64() * 1000.0),
                        Some(cd.idle_time.whole_seconds()),
                        cd.client_address.map(|a| a.to_string()),
                        cd.server_to_client_packetloss_total,
                        cd.connected_time.map(|d| d.whole_seconds()),
                    ),
                    None => (None, None, None, None, None),
                };

            online_users.push(UserInfo {
                name: client.name.to_string(),
                channel_name,
                channel_id: client.channel.0,
                is_away: client.away_message.is_some(),
                away_message: client.away_message.clone(),
                input_muted: client.input_muted,
                output_muted: client.output_muted,
                is_recording: client.is_recording,
                country_code: client.country_code.to_string(),
                is_priority_speaker: client.is_priority_speaker,
                is_channel_commander: client.is_channel_commander,
                server_groups: resolve_groups(&client.server_groups),
                channel_group: resolve_channel_group(client.channel_group),
                database_id: client.database_id.0,
                description: client.description.to_string(),
                platform,
                version,
                connections_total,
                ping_ms,
                idle_secs,
                client_address,
                packetloss_total,
                connected_secs,
            });
        }

        // Channel tree
        let mut channel_tree: Vec<ChannelInfo> = state
            .channels
            .iter()
            .map(|(cid, ch)| {
                let client_count = state.clients.values().filter(|c| c.channel == *cid).count();
                let is_permanent = matches!(ch.channel_type, tsclientlib::ChannelType::Permanent);
                ChannelInfo {
                    id: cid.0,
                    name: ch.name.to_string(),
                    client_count,
                    is_permanent,
                    topic: ch
                        .topic
                        .as_ref()
                        .filter(|t| !t.is_empty())
                        .map(|t| t.to_string()),
                    max_clients: ch.max_clients.and_then(|m| match m {
                        tsclientlib::MaxClients::Unlimited => None,
                        tsclientlib::MaxClients::Inherited => None,
                        tsclientlib::MaxClients::Limited(n) => Some(n as i32),
                    }),
                    has_password: ch.has_password.unwrap_or(false),
                    needed_talk_power: ch.needed_talk_power.filter(|&tp| tp > 0),
                }
            })
            .collect();
        channel_tree.sort_by_key(|c| c.id);

        Some(Self {
            server_name,
            bot_name,
            bot_channel,
            bot_uid,
            platform,
            version,
            max_clients,
            uptime_secs,
            connections_total,
            packetloss_total,
            avg_ping_ms,
            online_users,
            channel_tree,
        })
    }

    /// Render as compact text for the system prompt.
    /// `admin_view` controls whether admin-only fields like client IP, ping, idle are shown.
    pub fn to_prompt_text(&self, admin_view: bool) -> String {
        let mut out = String::new();

        out.push_str(&format!(
            "=== SERVER STATE ===\nServer: {} | Platform: {} | Version: {}\nMax Clients: {} | Online: {} users\nBot: \"{}\" (UID: {}) in channel \"{}\"",
            self.server_name,
            self.platform,
            self.version,
            self.max_clients,
            self.online_users.len(),
            self.bot_name,
            self.bot_uid,
            self.bot_channel
        ));

        // Server stats line
        let mut stats = Vec::new();
        if let Some(uptime) = self.uptime_secs {
            let days = uptime / 86400;
            let hours = (uptime % 86400) / 3600;
            let mins = (uptime % 3600) / 60;
            if days > 0 {
                stats.push(format!("Uptime: {}d {}h {}m", days, hours, mins));
            } else {
                stats.push(format!("Uptime: {}h {}m", hours, mins));
            }
        }
        if let Some(total_conns) = self.connections_total {
            stats.push(format!("Total Connections: {}", total_conns));
        }
        if let Some(ping) = self.avg_ping_ms {
            stats.push(format!("Avg Ping: {:.1}ms", ping));
        }
        if let Some(pl) = self.packetloss_total {
            stats.push(format!("Packetloss: {:.2}%", pl * 100.0));
        }
        if !stats.is_empty() {
            out.push_str(&format!("\n{}", stats.join(" | ")));
        }

        // Online users — cap at 30
        out.push_str("\n\n=== ONLINE USERS ===\n");
        for (i, u) in self.online_users.iter().enumerate() {
            if i >= 30 {
                out.push_str(&format!("... and {} more\n", self.online_users.len() - 30));
                break;
            }

            // Build status tags
            let mut tags = Vec::new();
            if u.is_away {
                if let Some(ref msg) = u.away_message {
                    if !msg.is_empty() {
                        tags.push(format!("AFK: {}", msg));
                    } else {
                        tags.push("AFK".to_string());
                    }
                } else {
                    tags.push("AFK".to_string());
                }
            }
            if u.input_muted {
                tags.push("mic-off".to_string());
            }
            if u.output_muted {
                tags.push("sound-off".to_string());
            }
            if u.is_recording {
                tags.push("RECORDING".to_string());
            }
            if u.is_priority_speaker {
                tags.push("priority".to_string());
            }
            if u.is_channel_commander {
                tags.push("commander".to_string());
            }

            let tag_str = if tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", tags.join(", "))
            };

            let country = if u.country_code.is_empty() {
                String::new()
            } else {
                format!(" ({})", u.country_code)
            };
            let platform = u.platform.as_deref().unwrap_or("");
            let platform_str = if platform.is_empty() {
                String::new()
            } else {
                format!(" | {}", platform)
            };
            let groups = if u.server_groups.is_empty() {
                String::new()
            } else {
                format!(" | Groups: {}", u.server_groups.join(", "))
            };
            let ch_group = format!(" | ChGroup: {}", u.channel_group);
            let desc = if u.description.is_empty() {
                String::new()
            } else {
                format!(" | Desc: \"{}\"", u.description)
            };
            let ver = u
                .version
                .as_deref()
                .map(|v| format!(" | v{}", v))
                .unwrap_or_default();
            let conns = u
                .connections_total
                .map(|c| format!(" | Connections: {}", c))
                .unwrap_or_default();

            out.push_str(&format!(
                "- {}{}{} in \"{}\" (ID {}, DBID {}){}{}{}{}{}{}\n",
                u.name,
                country,
                tag_str,
                u.channel_name,
                u.channel_id,
                u.database_id,
                platform_str,
                groups,
                ch_group,
                desc,
                ver,
                conns
            ));

            // Admin-only connection data
            if admin_view {
                let mut admin_parts = Vec::new();
                if let Some(ping) = u.ping_ms {
                    admin_parts.push(format!("ping={:.1}ms", ping));
                }
                if let Some(idle) = u.idle_secs {
                    admin_parts.push(format!("idle={}s", idle));
                }
                if let Some(ref addr) = u.client_address {
                    admin_parts.push(format!("ip={}", addr));
                }
                if let Some(pl) = u.packetloss_total {
                    admin_parts.push(format!("packetloss={:.2}%", pl * 100.0));
                }
                if let Some(ct) = u.connected_secs {
                    let hours = ct / 3600;
                    let mins = (ct % 3600) / 60;
                    admin_parts.push(format!("online={}h{}m", hours, mins));
                }
                if !admin_parts.is_empty() {
                    out.push_str(&format!("  ↳ [ADMIN] {}\n", admin_parts.join(" | ")));
                }
            }
        }

        // Channel tree — cap at 40
        out.push_str("\n=== CHANNELS ===\n");
        for (i, ch) in self.channel_tree.iter().enumerate() {
            if i >= 40 {
                out.push_str(&format!(
                    "... and {} more channels\n",
                    self.channel_tree.len() - 40
                ));
                break;
            }
            let ch_type = if ch.is_permanent { "perm" } else { "temp" };
            let topic = ch
                .topic
                .as_deref()
                .map(|t| format!(" topic=\"{}\"", t))
                .unwrap_or_default();
            let max = ch
                .max_clients
                .map(|m| format!(" max={}", m))
                .unwrap_or_default();
            let pw = if ch.has_password { " 🔒" } else { "" };
            let tp = ch
                .needed_talk_power
                .map(|t| format!(" tp={}", t))
                .unwrap_or_default();
            out.push_str(&format!(
                "- \"{}\" (ID {}, {} users, {}{}{}{}{})\n",
                ch.name, ch.id, ch.client_count, ch_type, topic, max, pw, tp
            ));
        }

        out
    }
}

// ─── Invoker Context ────────────────────────────────────────

/// Where the message originated.
#[derive(Debug, Clone)]
pub enum MessageSource {
    Direct,
    Channel { channel_name: String },
    Server,
}

impl std::fmt::Display for MessageSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Direct => write!(f, "Direct Message (private chat)"),
            Self::Channel { channel_name } => {
                write!(f, "Channel message in \"{}\"", channel_name)
            }
            Self::Server => write!(f, "Server-wide message"),
        }
    }
}

/// Per-request context about the invoker.
#[derive(Debug, Clone)]
pub struct InvokerContext {
    pub name: String,
    pub uid: String,
    pub channel_name: String,
    pub channel_id: u64,
    pub message_source: MessageSource,
    // Enriched data
    pub country_code: String,
    pub server_groups: Vec<String>,
    pub channel_group: String,
    pub is_away: bool,
    pub away_message: Option<String>,
    pub input_muted: bool,
    pub platform: Option<String>,
    pub description: String,
    pub database_id: u64,
}

impl InvokerContext {
    /// Build from the event invoker and connection state.
    pub fn from_event(
        con: &Connection,
        invoker_id: ClientId,
        invoker_name: &str,
        invoker_uid: &str,
        target: &MessageTarget,
    ) -> Self {
        let mut channel_name = String::new();
        let mut channel_id: u64 = 0;
        let mut country_code = String::new();
        let mut server_groups = Vec::new();
        let mut channel_group = String::new();
        let mut is_away = false;
        let mut away_message = None;
        let mut input_muted = false;
        let mut platform = None;
        let mut description = String::new();
        let mut database_id: u64 = 0;

        if let Ok(state) = con.get_state() {
            if let Some(client) = state.clients.get(&invoker_id) {
                channel_id = client.channel.0;
                if let Some(ch) = state.channels.get(&client.channel) {
                    channel_name = ch.name.to_string();
                }
                country_code = client.country_code.to_string();
                is_away = client.away_message.is_some();
                away_message = client.away_message.clone();
                input_muted = client.input_muted;
                description = client.description.to_string();
                database_id = client.database_id.0;

                // Resolve server group names
                for gid in &client.server_groups {
                    if let Some(g) = state.server_groups.get(gid) {
                        server_groups.push(g.name.to_string());
                    }
                }

                // Resolve channel group name
                if let Some(cg) = state.channel_groups.get(&client.channel_group) {
                    channel_group = cg.name.to_string();
                }

                if let Some(ref opt) = client.optional_data {
                    platform = Some(opt.platform.to_string());
                }
            }
        }

        let message_source = match target {
            MessageTarget::Client(_) => MessageSource::Direct,
            MessageTarget::Channel => MessageSource::Channel {
                channel_name: channel_name.clone(),
            },
            MessageTarget::Server => MessageSource::Server,
            _ => MessageSource::Direct,
        };

        Self {
            name: invoker_name.to_string(),
            uid: invoker_uid.to_string(),
            channel_name,
            channel_id,
            message_source,
            country_code,
            server_groups,
            channel_group,
            is_away,
            away_message,
            input_muted,
            platform,
            description,
            database_id,
        }
    }

    /// Render as text for the system prompt.
    pub fn to_prompt_text(&self) -> String {
        let country = if self.country_code.is_empty() {
            String::new()
        } else {
            format!(" | Country: {}", self.country_code)
        };
        let groups = if self.server_groups.is_empty() {
            String::new()
        } else {
            format!("\nServer Groups: {}", self.server_groups.join(", "))
        };
        let ch_group = if self.channel_group.is_empty() {
            String::new()
        } else {
            format!(" | ChGroup: {}", self.channel_group)
        };
        let platform = self
            .platform
            .as_deref()
            .map(|p| format!(" | Platform: {}", p))
            .unwrap_or_default();
        let desc = if self.description.is_empty() {
            String::new()
        } else {
            format!("\nDescription: {}", self.description)
        };
        let mut status = Vec::new();
        if self.is_away {
            status.push(
                self.away_message
                    .as_deref()
                    .map(|m| format!("AFK: {}", m))
                    .unwrap_or_else(|| "AFK".to_string()),
            );
        }
        if self.input_muted {
            status.push("mic-off".to_string());
        }
        let status_str = if status.is_empty() {
            String::new()
        } else {
            format!("\nStatus: {}", status.join(", "))
        };

        format!(
            "=== INVOKER ===\nName: {} | UID: {} | DBID: {} | Channel: \"{}\" (ID {}){}{}{}\nMessage type: {}{}{}{}",
            self.name, self.uid, self.database_id, self.channel_name, self.channel_id,
            country, platform, ch_group, self.message_source, groups, desc, status_str
        )
    }
}
