mod actions;
mod ai;
mod audio;
mod audit;
mod channels;
mod clients;
mod config;
mod context;
mod identity;
mod learning;
mod permissions;
mod prompt_workspace;
mod prompts;
mod tickets;

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use dashmap::DashMap;
use futures::prelude::*;
use tracing::{debug, error, info, warn};

use tsclientlib::events::{Event, PropertyId};
use tsclientlib::{
    Connection, DisconnectOptions, Identity, MessageTarget, OutCommandExt, Reason, StreamItem,
};

use crate::actions::{get_reply_text, parse_ai_response, BotAction};
use crate::ai::{AiClient, ChatMessage};
use crate::config::load_config;

// ─── CLI Arguments ──────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "ts3-ai-bot",
    about = "TeamSpeak 3 AI Support Agent powered by Ollama"
)]
struct Args {
    /// Path to the configuration file
    #[arg(short, long, default_value = "config.toml")]
    config: String,

    /// Verbosity level for packet logging
    ///
    /// 0 = nothing, 1 = commands, 2 = packets, 3 = UDP packets
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

// ─── Rate Limiter ───────────────────────────────────────────

struct RateLimiter {
    timestamps: Vec<Instant>,
    max_per_second: u8,
}

impl RateLimiter {
    fn new(max_per_second: u8) -> Self {
        Self {
            timestamps: Vec::new(),
            max_per_second,
        }
    }

    /// Returns `true` if the action is allowed (not rate-limited).
    fn check_and_record(&mut self) -> bool {
        let now = Instant::now();
        let one_second = Duration::from_secs(1);

        // Remove timestamps older than 1 second
        self.timestamps
            .retain(|t| now.duration_since(*t) <= one_second);

        if self.timestamps.len() >= self.max_per_second as usize {
            false
        } else {
            self.timestamps.push(now);
            true
        }
    }
}

// ─── Pending Channel Setup ──────────────────────────────────

struct PendingCreation {
    channel_name: String,
    invoker_id: tsproto_types::ClientId,
    created_at: Instant,
}

const GLOBAL_AI_QUEUE_CAPACITY: usize = 200;

#[derive(Debug, Clone)]
struct QueuedAiRequest {
    request_id: u64,
    reply_target: MessageTarget,
    invoker_id: tsproto_types::ClientId,
    perm_level: permissions::PermissionLevel,
    invoker_name: String,
    invoker_uid: String,
    system_prompt: String,
    user_message: String,
}

#[derive(Debug)]
struct AiActionResult {
    request_id: u64,
    reply_target: MessageTarget,
    invoker_id: tsproto_types::ClientId,
    perm_level: permissions::PermissionLevel,
    invoker_name: String,
    invoker_uid: String,
    bot_actions: Vec<BotAction>,
}

// ─── Main ───────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    real_main().await
}

async fn real_main() -> Result<()> {
    // Initialize logging with a default filter of INFO
    // Can be overridden with RUST_LOG env var
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    eprintln!("═══════════════════════════════════════════════");
    eprintln!("  TS3 AI Support Agent Bot — Starting up...");
    eprintln!("═══════════════════════════════════════════════");

    // Parse CLI args
    let args = Args::parse();

    // Load configuration
    let cfg = load_config(&args.config)?;
    info!(
        address = %cfg.server_address,
        bot_name = %cfg.bot_name,
        model = %cfg.default_model,
        fallback = %cfg.fallback_model,
        "Configuration loaded"
    );

    // ── Identity key management ─────────────────────────────
    let key_path = Path::new(&cfg.key_file);
    let private_key = match fs::read(key_path) {
        Ok(data) => {
            info!("Loaded existing identity key from {:?}", key_path);
            tsproto_types::crypto::EccKeyPrivP256::import(&data)?
        }
        Err(_) => {
            info!("No identity key found, generating a new one");
            let key = tsproto_types::crypto::EccKeyPrivP256::create();

            // Save for future runs
            if let Some(parent) = key_path.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent).ok();
                }
            }
            if let Err(e) = fs::write(key_path, key.to_short()) {
                warn!(error = %e, "Failed to save identity key — identity will change on next run");
            } else {
                info!("Identity key saved to {:?}", key_path);
            }

            key
        }
    };

    let identity = Identity::new(private_key, 0);

    // ── Build connection ────────────────────────────────────
    let mut con_config = Connection::build(cfg.server_address.as_str())
        .identity(identity)
        .name(cfg.bot_name.clone())
        .log_commands(args.verbose >= 1)
        .log_packets(args.verbose >= 2)
        .log_udp_packets(args.verbose >= 3)
        .input_hardware_enabled(true)
        .output_hardware_enabled(true);

    if let Some(ref channel) = cfg.channel {
        con_config = con_config.channel(channel.clone());
    }

    // ── Connect to server ───────────────────────────────────
    info!(
        "Connecting to TeamSpeak server at {}...",
        cfg.server_address
    );
    let mut con = con_config.connect()?;

    // Wait for initial book events (server state sync)
    let r = con
        .events()
        .try_filter(|e| future::ready(matches!(e, StreamItem::BookEvents(_))))
        .next()
        .await;
    if let Some(r) = r {
        r?;
    }

    info!("✓ Connected to TeamSpeak server successfully!");

    // Subscribe to all channels so we can see all users across the server
    {
        use tsclientlib::messages::c2s::OutChannelSubscribeAllMessage;
        use tsclientlib::messages::OutMessageTrait;
        if let Err(e) = OutChannelSubscribeAllMessage::new()
            .to_packet()
            .send(&mut con)
        {
            warn!("Failed to subscribe to all channels: {}", e);
        }
    }

    // Print welcome message
    if let Ok(state) = con.get_state() {
        let welcome = sanitize(&state.server.welcome_message);
        if !welcome.is_empty() {
            info!("Server welcome: {}", welcome);
        }
    }

    // Move the bot to the configured channel (supports channel ID or name)
    if let Some(ref channel) = cfg.channel {
        // Try to parse as numeric channel ID first
        if let Ok(channel_id) = channel.parse::<u64>() {
            info!(channel_id, "Moving bot to configured channel by ID");
            if let Ok(state) = con.get_state() {
                let own_client_id = state.own_client;
                let msg = tsclientlib::messages::c2s::OutClientMoveMessage::new(
                    &mut std::iter::once(tsclientlib::messages::c2s::OutClientMovePart {
                        client_id: own_client_id,
                        channel_id: tsclientlib::ChannelId(channel_id),
                        channel_password: None,
                    }),
                );
                use tsclientlib::messages::OutMessageTrait;
                if let Err(e) = msg.to_packet().send(&mut con) {
                    warn!(error = %e, "Failed to move bot to configured channel");
                } else {
                    info!(channel_id, "Bot moved to configured channel");
                }
            }
        } else {
            // It's a channel name — tsclientlib handles this during connect
            info!(channel = %channel, "Channel was set by name during connection");
        }
    }

    // ── Create the AI client ────────────────────────────────
    let ai_client = Arc::new(AiClient::new(
        cfg.ai_api_url.clone(),
        cfg.ai_api_key.clone(),
        cfg.fallback_api_url.clone(),
        cfg.fallback_api_key.clone(),
        cfg.default_model.clone(),
        cfg.fallback_model.clone(),
        cfg.ai_timeout_secs,
    ));

    // Conversation history: invoker_uid -> Vec<ChatMessage>
    let history_store = Arc::new(DashMap::<String, Vec<ChatMessage>>::new());

    let mut rate_limiter = RateLimiter::new(cfg.rate_limit);

    // Initialize local ticket system
    let ticket_store = Arc::new(tokio::sync::RwLock::new(tickets::TicketStore::new(
        &cfg.tickets_file,
    )));

    // Initialize identity tracking system
    let identity_store = Arc::new(tokio::sync::RwLock::new(identity::IdentityStore::new(
        &cfg.identities_file,
    )));

    // Initialize audit logger
    let audit_logger = Arc::new(audit::AuditLogger::new(&cfg.audit_log_file));

    // Runtime configuration snapshots used by async tasks
    let radios_file = cfg.radios_file.clone();
    let tts_config = audio::TtsConfig {
        piper_path: cfg.piper_binary_path.clone(),
        voice_dir: cfg.piper_voice_dir.clone(),
        ffmpeg_path: cfg.ffmpeg_binary_path.clone(),
        yt_dlp_path: cfg.yt_dlp_binary_path.clone(),
        music_start_volume: cfg.music_start_volume,
    };

    // ── Main event loop ─────────────────────────────────────
    let (ai_tx, mut ai_rx) = tokio::sync::mpsc::channel::<AiActionResult>(100);
    let (ai_queue_tx, mut ai_queue_rx) =
        tokio::sync::mpsc::channel::<QueuedAiRequest>(GLOBAL_AI_QUEUE_CAPACITY);
    let (audio_tx, mut audio_rx) =
        tokio::sync::mpsc::channel::<tsproto_packets::packets::OutPacket>(500);

    // List of channels waiting for their creation event to be processed
    let mut pending_creations = Vec::<PendingCreation>::new();

    // Track where users were before being moved (for MOVE_CLIENT_RETURN)
    let mut pre_move_channels: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    // Global AI queue metrics
    let pending_ai_requests = Arc::new(AtomicUsize::new(0));
    let next_request_id = Arc::new(AtomicU64::new(1));

    let admin_uids_for_alerts = cfg.admin_uids.clone();

    // Music bot state
    let mut current_music_stop_flag: Option<Arc<AtomicBool>> = None;

    {
        let ai_client_worker = Arc::clone(&ai_client);
        let history_store_worker = Arc::clone(&history_store);
        let ai_tx_worker = ai_tx.clone();
        let pending_ai_requests_worker = Arc::clone(&pending_ai_requests);
        tokio::spawn(async move {
            while let Some(request) = ai_queue_rx.recv().await {
                info!(
                    request_id = request.request_id,
                    invoker = %request.invoker_name,
                    uid = %request.invoker_uid,
                    "Processing queued AI request"
                );

                let mut updated_history = history_store_worker
                    .entry(request.invoker_uid.clone())
                    .or_default()
                    .clone();
                updated_history.push(ChatMessage::user(request.user_message.clone()));

                let bot_actions = match ai_client_worker
                    .chat(&request.system_prompt, &updated_history)
                    .await
                {
                    Ok(ai_response) => {
                        info!(
                            request_id = request.request_id,
                            invoker = %request.invoker_name,
                            uid = %request.invoker_uid,
                            response = %ai_response,
                            "AI response sent to user"
                        );

                        updated_history.push(ChatMessage::assistant(ai_response.clone()));
                        let bot_actions = parse_ai_response(&ai_response);

                        if updated_history.len() > 20 {
                            let start = updated_history.len() - 20;
                            updated_history = updated_history[start..].to_vec();
                        }

                        history_store_worker.insert(request.invoker_uid.clone(), updated_history);
                        bot_actions
                    }
                    Err(e) => {
                        error!(
                            request_id = request.request_id,
                            invoker = %request.invoker_name,
                            uid = %request.invoker_uid,
                            error = %e,
                            "AI processing failed"
                        );
                        vec![BotAction::Reply {
                            message: "I'm having trouble processing your request right now. Please try again in a moment.".to_string(),
                        }]
                    }
                };

                pending_ai_requests_worker.fetch_sub(1, Ordering::SeqCst);

                if ai_tx_worker
                    .send(AiActionResult {
                        request_id: request.request_id,
                        reply_target: request.reply_target,
                        invoker_id: request.invoker_id,
                        perm_level: request.perm_level,
                        invoker_name: request.invoker_name,
                        invoker_uid: request.invoker_uid,
                        bot_actions,
                    })
                    .await
                    .is_err()
                {
                    error!("Failed to deliver AI action result from queue worker");
                    break;
                }
            }
            warn!("Global AI queue worker stopped");
        });
    }

    info!("Listening for messages...");

    let mut stats_interval = tokio::time::interval(std::time::Duration::from_secs(60));

    loop {
        let mut events = con.events();
        tokio::select! {
            _ = stats_interval.tick() => {
                drop(events);
                use tsclientlib::OutCommandExt;
                let packet = tsproto_packets::packets::OutCommand::new(
                    tsproto_packets::packets::Direction::C2S,
                    tsproto_packets::packets::Flags::empty(),
                    tsproto_packets::packets::PacketType::Command,
                    "servergetvariables",
                );
                if let Err(e) = packet.send(&mut con) {
                    tracing::error!(error = %e, "Failed to request server variables for stats");
                }
            }

            // Stream generated TTS audio into the TeamSpeak channel
            Some(audio_packet) = audio_rx.recv() => {
                drop(events);
                if let Err(e) = con.send_audio(audio_packet) {
                    tracing::error!(error = %e, "Failed to send audio packet");
                }
            }

            // Graceful shutdown on Ctrl+C
            _ = tokio::signal::ctrl_c() => {
                info!("Shutdown signal received");
                break;
            }

            // Execute bot actions received from queued AI worker
            Some(ai_result) = ai_rx.recv() => {
                drop(events);

                let AiActionResult {
                    request_id,
                    reply_target,
                    invoker_id,
                    perm_level,
                    invoker_name,
                    invoker_uid,
                    bot_actions,
                } = ai_result;

                let resolved_invoker_id = resolve_client_id_by_uid(&con, &invoker_uid).unwrap_or(invoker_id);
                let reply_target = match reply_target {
                    MessageTarget::Client(_) => MessageTarget::Client(resolved_invoker_id),
                    other => other,
                };
                let invoker_id = resolved_invoker_id;

                info!(
                    request_id,
                    invoker = %invoker_name,
                    uid = %invoker_uid,
                    "Applying queued AI actions"
                );

                let is_admin = perm_level >= permissions::PermissionLevel::Admin;
                let mut unauthorized_action_attempted = false;

                for action in &bot_actions {
                    match action {
                        BotAction::Reply { .. } => {
                            // Replies are collected and sent at the end
                        }
                        _ => {
                            // Permission check via the permissions module
                            if let Err(reason) = permissions::can_execute(perm_level, action, &invoker_name, &cfg) {
                                // Log the denial
                                audit_logger.log(audit::AuditEntry {
                                    invoker_name: invoker_name.clone(),
                                    invoker_uid: invoker_uid.clone(),
                                    action: audit::action_name(action).to_string(),
                                    target: audit::action_target(action),
                                    result: audit::AuditResult::Denied(reason.clone()),
                                });
                                send_private_reply(&mut con, invoker_id, &reason);
                                alert_admins(&mut con, &admin_uids_for_alerts, &format!(
                                    "WARNING: User [b]{}[/b] ({}) attempted action [b]{}[/b] without sufficient permissions.",
                                    invoker_name, perm_level, audit::action_name(action)
                                ));
                                unauthorized_action_attempted = true;
                                break;
                            }
                        }
                    }

                    // Execute the action
                    match action {
                        BotAction::Reply { .. } => {}
                        BotAction::CreateChannel { channel_name, password, permanent } => {
                            if let Err(e) = channels::create_channel(
                                &mut con,
                                channel_name,
                                password.as_deref(),
                                *permanent,
                                cfg.channel_order_anchor_id,
                            ) {
                                error!(error = %e, "Failed to create channel");
                                audit_logger.log(audit::AuditEntry {
                                    invoker_name: invoker_name.clone(),
                                    invoker_uid: invoker_uid.clone(),
                                    action: "CREATE_CHANNEL".into(),
                                    target: Some(channel_name.clone()),
                                    result: audit::AuditResult::Error(e.to_string()),
                                });
                                send_reply(
                                    &mut con,
                                    reply_target,
                                    &format!(
                                        "Sorry, I couldn't create the channel '{}': {}",
                                        channel_name, e
                                    ),
                                );
                            } else {
                                audit_logger.log(audit::AuditEntry {
                                    invoker_name: invoker_name.clone(),
                                    invoker_uid: invoker_uid.clone(),
                                    action: "CREATE_CHANNEL".into(),
                                    target: Some(channel_name.clone()),
                                    result: audit::AuditResult::Success,
                                });
                                pending_creations.push(PendingCreation {
                                    channel_name: channel_name.clone(),
                                    invoker_id,
                                    created_at: Instant::now(),
                                });
                            }
                        }
                        BotAction::EditChannel { channel_name, set_permanent } => {
                            // Additional own-channel check for non-admins
                            if !is_admin {
                                let mut is_channel_admin = false;
                                let mut is_in_channel = false;
                                if let Ok(state) = con.get_state() {
                                    if let Some(c) = state.clients.get(&invoker_id) {
                                        if let Some(ch) = state.channels.get(&c.channel) {
                                            if ch.name.to_lowercase() == channel_name.to_lowercase() {
                                                is_in_channel = true;
                                                is_channel_admin = c.channel_group.0 == cfg.channel_admin_group_id;
                                            }
                                        }
                                    }
                                }
                                if !is_in_channel || !is_channel_admin {
                                    send_private_reply(&mut con, invoker_id, "You do not have permission to edit this channel. You must be inside the channel and have Channel Admin.");
                                    unauthorized_action_attempted = true;
                                    break;
                                }
                            }

                            if let Err(e) = channels::edit_channel_permanent(
                                &mut con,
                                channel_name,
                                *set_permanent,
                            ) {
                                error!(error = %e, "Failed to edit channel");
                                send_reply(&mut con, reply_target, &format!("Sorry, I couldn't modify the channel '{}': {}", channel_name, e));
                            } else {
                                audit_logger.log(audit::AuditEntry {
                                    invoker_name: invoker_name.clone(),
                                    invoker_uid: invoker_uid.clone(),
                                    action: "EDIT_CHANNEL".into(),
                                    target: Some(channel_name.clone()),
                                    result: audit::AuditResult::Success,
                                });
                            }
                        }
                        BotAction::DeleteChannel { channel_name } => {
                            // Own-channel check for non-admins
                            if !is_admin {
                                let mut is_channel_admin = false;
                                let mut is_in_channel = false;
                                if let Ok(state) = con.get_state() {
                                    if let Some(c) = state.clients.get(&invoker_id) {
                                        if let Some(ch) = state.channels.get(&c.channel) {
                                            if ch.name.to_lowercase() == channel_name.to_lowercase() {
                                                is_in_channel = true;
                                                is_channel_admin = c.channel_group.0 == cfg.channel_admin_group_id;
                                            }
                                        }
                                    }
                                }
                                if !is_in_channel || !is_channel_admin {
                                    send_private_reply(&mut con, invoker_id, "You do not have permission to delete this channel. You must be inside the channel and have Channel Admin.");
                                    unauthorized_action_attempted = true;
                                    break;
                                }
                            }

                            if let Err(e) = channels::delete_channel(&mut con, channel_name) {
                                error!(error = %e, "Failed to delete channel");
                                send_reply(&mut con, reply_target, &format!("Sorry, I couldn't delete the channel '{}': {}", channel_name, e));
                            } else {
                                audit_logger.log(audit::AuditEntry {
                                    invoker_name: invoker_name.clone(),
                                    invoker_uid: invoker_uid.clone(),
                                    action: "DELETE_CHANNEL".into(),
                                    target: Some(channel_name.clone()),
                                    result: audit::AuditResult::Success,
                                });
                            }
                        }
                        BotAction::EditChannelDescription { channel_id, description } => {
                            if !is_admin {
                                let mut is_channel_admin = false;
                                let mut is_in_channel = false;
                                if let Ok(state) = con.get_state() {
                                    if let Some(c) = state.clients.get(&invoker_id) {
                                        if c.channel.0 == *channel_id {
                                            is_in_channel = true;
                                            is_channel_admin = c.channel_group.0 == cfg.channel_admin_group_id;
                                        }
                                    }
                                }
                                if !is_in_channel || !is_channel_admin {
                                    unauthorized_action_attempted = true;
                                    break;
                                }
                            }

                            if let Err(e) = channels::edit_channel_description(&mut con, *channel_id, description) {
                                error!(error = %e, "Failed to edit channel description");
                                send_reply(&mut con, reply_target, &format!("Sorry, I couldn't update the description of channel '{}': {}", channel_id, e));
                            }
                        }
                        BotAction::SetChannelAdmin { channel_name, client_name } => {
                            if !is_admin {
                                let mut is_channel_admin = false;
                                let mut is_in_channel = false;
                                if let Ok(state) = con.get_state() {
                                    if let Some(c) = state.clients.get(&invoker_id) {
                                        if let Some(ch) = state.channels.get(&c.channel) {
                                            if ch.name.to_lowercase() == channel_name.to_lowercase() {
                                                is_in_channel = true;
                                                is_channel_admin = c.channel_group.0 == cfg.channel_admin_group_id;
                                            }
                                        }
                                    }
                                }
                                if !is_in_channel || !is_channel_admin {
                                    send_private_reply(&mut con, invoker_id, "You do not have permission to assign Channel Admin for this channel.");
                                    unauthorized_action_attempted = true;
                                    break;
                                }
                            }

                            let target_db_id_res = if let Some(name) = client_name {
                                crate::clients::find_client_by_name(&con, name).map(|(_, db_id)| db_id)
                            } else {
                                match con.get_state() {
                                    Ok(state) => match state.clients.get(&invoker_id) {
                                        Some(c) => Ok(c.database_id.0),
                                        None => Err(anyhow::anyhow!("Could not find your database ID in state.")),
                                    },
                                    Err(e) => Err(anyhow::anyhow!(e)),
                                }
                            };

                            match target_db_id_res {
                                Ok(db_id) => {
                                    if let Err(e) = channels::set_channel_admin(
                                        &mut con,
                                        channel_name,
                                        db_id,
                                        cfg.channel_admin_group_id,
                                    ) {
                                        error!(error = %e, "Failed to set channel admin");
                                        send_reply(&mut con, reply_target, &format!("Sorry, I couldn't set channel admin in '{}': {}", channel_name, e));
                                    }
                                }
                                Err(e) => {
                                    send_reply(&mut con, reply_target, &format!("Sorry, I couldn't identify the user to grant admin: {}", e));
                                }
                            }
                        }
                        BotAction::KickClient { client_name, reason } => {
                            // Permission already checked above
                            match clients::kick_client(&mut con, client_name, reason.as_deref()) {
                                Ok(()) => {
                                    audit_logger.log(audit::AuditEntry {
                                        invoker_name: invoker_name.clone(),
                                        invoker_uid: invoker_uid.clone(),
                                        action: "KICK_CLIENT".into(),
                                        target: Some(client_name.clone()),
                                        result: audit::AuditResult::Success,
                                    });
                                }
                                Err(e) => {
                                    error!(error = %e, "Failed to kick client");
                                    send_reply(&mut con, reply_target, &format!("Sorry, I couldn't kick '{}': {}", client_name, e));
                                }
                            }
                        }
                        BotAction::BanClient { client_name, reason, duration_seconds } => {
                            match clients::ban_client(&mut con, client_name, reason.as_deref(), *duration_seconds) {
                                Ok(()) => {
                                    audit_logger.log(audit::AuditEntry {
                                        invoker_name: invoker_name.clone(),
                                        invoker_uid: invoker_uid.clone(),
                                        action: "BAN_CLIENT".into(),
                                        target: Some(client_name.clone()),
                                        result: audit::AuditResult::Success,
                                    });
                                }
                                Err(e) => {
                                    error!(error = %e, "Failed to ban client");
                                    send_reply(&mut con, reply_target, &format!("Sorry, I couldn't ban '{}': {}", client_name, e));
                                }
                            }
                        }
                        BotAction::MoveClient { client_name, channel_name } => {
                            // Save the target's current channel before moving
                            if let Ok(state) = con.get_state() {
                                for client in state.clients.values() {
                                    if client.name.to_lowercase() == client_name.to_lowercase() {
                                        if let Some(ch) = state.channels.get(&client.channel) {
                                            info!(client = %client_name, from = %ch.name, to = %channel_name, "Saving pre-move channel");
                                            pre_move_channels.insert(client_name.to_lowercase(), ch.name.clone());
                                        }
                                        break;
                                    }
                                }
                            }
                            match clients::move_client(&mut con, client_name, channel_name) {
                                Ok(()) => {
                                    audit_logger.log(audit::AuditEntry {
                                        invoker_name: invoker_name.clone(),
                                        invoker_uid: invoker_uid.clone(),
                                        action: "MOVE_CLIENT".into(),
                                        target: Some(format!("{} -> {}", client_name, channel_name)),
                                        result: audit::AuditResult::Success,
                                    });
                                }
                                Err(e) => {
                                    error!(error = %e, "Failed to move client");
                                    send_reply(&mut con, reply_target, &format!("Sorry, I couldn't move '{}': {}", client_name, e));
                                }
                            }
                        }
                        BotAction::MoveClientReturn { client_name } => {
                            let key = client_name.to_lowercase();
                            if let Some(original_channel) = pre_move_channels.get(&key) {
                                info!(client = %client_name, return_to = %original_channel, "Returning client to original channel");
                                if let Err(e) = clients::move_client(&mut con, client_name, original_channel) {
                                    error!(error = %e, "Failed to return client");
                                    send_reply(&mut con, reply_target, &format!("Sorry, I couldn't return '{}': {}", client_name, e));
                                } else {
                                    pre_move_channels.remove(&key);
                                }
                            } else {
                                warn!(client = %client_name, "No saved pre-move channel for client");
                                send_reply(&mut con, reply_target, &format!("I don't know where '{}' was before. Tell me which channel to move them back to.", client_name));
                            }
                        }
                        BotAction::JoinUserChannel => {
                            let target_channel_res = match con.get_state() {
                                Ok(state) => match state.clients.get(&invoker_id) {
                                    Some(c) => Ok((state.own_client, c.channel)),
                                    None => {
                                        let client_ids: Vec<_> = state.clients.keys().collect();
                                        warn!(?invoker_id, ?client_ids, "Client not found in state");
                                        Err(anyhow::anyhow!("Could not find your client state (ID: {:?}) in our current server view.", invoker_id))
                                    }
                                },
                                Err(e) => Err(anyhow::anyhow!(e)),
                            };

                            match target_channel_res {
                                Ok((own_client_id, target_channel_id)) => {
                                    let msg = tsclientlib::messages::c2s::OutClientMoveMessage::new(&mut std::iter::once(
                                        tsclientlib::messages::c2s::OutClientMovePart {
                                            client_id: own_client_id,
                                            channel_id: target_channel_id,
                                            channel_password: None,
                                        }
                                    ));
                                    use tsclientlib::messages::OutMessageTrait;
                                    if let Err(e) = msg.to_packet().send(&mut con) {
                                        error!(error = %e, "Failed to move bot to user channel");
                                    }
                                }
                                Err(e) => {
                                    send_reply(&mut con, reply_target, &format!("Sorry, I couldn't find what channel you are in: {}", e));
                                }
                            }
                        }
                        BotAction::SetServerGroup { client_name, server_group_id } => {
                            // Self-only and allowed-group checks already passed via permissions module
                            if let Err(e) = clients::set_server_group(&mut con, client_name, *server_group_id) {
                                error!(error = %e, "Failed to set server group");
                                send_private_reply(&mut con, invoker_id, &format!("I could not assign that server group: {}", e));
                            } else {
                                audit_logger.log(audit::AuditEntry {
                                    invoker_name: invoker_name.clone(),
                                    invoker_uid: invoker_uid.clone(),
                                    action: "SET_SERVER_GROUP".into(),
                                    target: Some(format!("{} +group {}", client_name, server_group_id)),
                                    result: audit::AuditResult::Success,
                                });
                            }
                        }
                        BotAction::RemoveServerGroup { client_name, server_group_id } => {
                            if let Err(e) = clients::remove_server_group(&mut con, client_name, *server_group_id) {
                                error!(error = %e, "Failed to remove server group");
                                send_private_reply(&mut con, invoker_id, &format!("I could not remove that server group: {}", e));
                            } else {
                                audit_logger.log(audit::AuditEntry {
                                    invoker_name: invoker_name.clone(),
                                    invoker_uid: invoker_uid.clone(),
                                    action: "REMOVE_SERVER_GROUP".into(),
                                    target: Some(format!("{} -group {}", client_name, server_group_id)),
                                    result: audit::AuditResult::Success,
                                });
                            }
                        }
                        BotAction::PokeClient { client_name, message } => {
                            match clients::poke_client(&mut con, client_name, message) {
                                Ok(()) => {
                                    audit_logger.log(audit::AuditEntry {
                                        invoker_name: invoker_name.clone(),
                                        invoker_uid: invoker_uid.clone(),
                                        action: "POKE_CLIENT".into(),
                                        target: Some(client_name.clone()),
                                        result: audit::AuditResult::Success,
                                    });
                                }
                                Err(e) => {
                                    error!(error = %e, "Failed to poke client");
                                    send_reply(&mut con, reply_target, &format!("Sorry, I couldn't poke '{}': {}", client_name, e));
                                }
                            }
                        }
                        BotAction::SendMessage { target_name, message } => {
                            match clients::send_message_to(&mut con, target_name, message) {
                                Ok(()) => {
                                    audit_logger.log(audit::AuditEntry {
                                        invoker_name: invoker_name.clone(),
                                        invoker_uid: invoker_uid.clone(),
                                        action: "SEND_MESSAGE".into(),
                                        target: Some(target_name.clone()),
                                        result: audit::AuditResult::Success,
                                    });
                                }
                                Err(e) => {
                                    error!(error = %e, "Failed to send message");
                                    send_reply(&mut con, reply_target, &format!("Sorry, I couldn't message '{}': {}", target_name, e));
                                }
                            }
                        }
                        BotAction::SendChannelMessage { channel_name, message } => {
                            match clients::send_channel_message(&mut con, channel_name, message) {
                                Ok(()) => {
                                    audit_logger.log(audit::AuditEntry {
                                        invoker_name: invoker_name.clone(),
                                        invoker_uid: invoker_uid.clone(),
                                        action: "SEND_CHANNEL_MESSAGE".into(),
                                        target: Some(channel_name.clone()),
                                        result: audit::AuditResult::Success,
                                    });
                                }
                                Err(e) => {
                                    error!(error = %e, "Failed to send channel message");
                                    send_reply(&mut con, reply_target, &format!("Sorry, I couldn't send a message to channel '{}': {}", channel_name, e));
                                }
                            }
                        }
                        BotAction::MoveBotChannel { channel_name } => {
                            match clients::move_bot_to_channel(&mut con, channel_name) {
                                Ok(()) => {
                                    audit_logger.log(audit::AuditEntry {
                                        invoker_name: invoker_name.clone(),
                                        invoker_uid: invoker_uid.clone(),
                                        action: "MOVE_BOT_CHANNEL".into(),
                                        target: Some(channel_name.clone()),
                                        result: audit::AuditResult::Success,
                                    });
                                }
                                Err(e) => {
                                    error!(error = %e, "Failed to move bot channel");
                                    send_reply(&mut con, reply_target, &format!("Sorry, I couldn't move to channel '{}': {}", channel_name, e));
                                }
                            }
                        }
                        BotAction::PlayTTS { text } => {
                            info!("AI requested TTS: {}", text);
                            let tts_text_clone = text.clone();
                            let tts_tx = audio_tx.clone();
                            let tts_config_clone = tts_config.clone();

                            tokio::spawn(async move {
                                match audio::fetch_tts_pcm(&tts_text_clone, &tts_config_clone).await {
                                    Ok(pcm) => {
                                        if let Err(e) = audio::stream_to_ts(pcm, tts_tx, 0) {
                                            error!("Failed to stream TTS audio: {}", e);
                                        }
                                    }
                                    Err(e) => error!("Failed to fetch TTS: {}", e),
                                }
                            });
                        }
                        BotAction::BanAdd { ip, uid, name, reason, duration_seconds } => {
                            match clients::ban_add(&mut con, ip.as_deref(), uid.as_deref(), name.as_deref(), reason.as_deref(), *duration_seconds) {
                                Ok(()) => {
                                    audit_logger.log(audit::AuditEntry {
                                        invoker_name: invoker_name.clone(),
                                        invoker_uid: invoker_uid.clone(),
                                        action: "BAN_ADD".into(),
                                        target: Some(format!("ip={} uid={} name={}", ip.as_deref().unwrap_or("-"), uid.as_deref().unwrap_or("-"), name.as_deref().unwrap_or("-"))),
                                        result: audit::AuditResult::Success,
                                    });
                                    send_reply(&mut con, reply_target, "Ban added successfully.");
                                }
                                Err(e) => {
                                    error!(error = %e, "Failed to add ban");
                                    send_reply(&mut con, reply_target, &format!("Error while adding ban: {}", e));
                                }
                            }
                        }
                        BotAction::BanDel { ban_id } => {
                            match clients::ban_del(&mut con, *ban_id) {
                                Ok(()) => {
                                    audit_logger.log(audit::AuditEntry {
                                        invoker_name: invoker_name.clone(),
                                        invoker_uid: invoker_uid.clone(),
                                        action: "BAN_DEL".into(),
                                        target: Some(format!("ban #{}", ban_id)),
                                        result: audit::AuditResult::Success,
                                    });
                                    send_reply(&mut con, reply_target, &format!("Ban #{} removed.", ban_id));
                                }
                                Err(e) => {
                                    error!(error = %e, "Failed to delete ban");
                                    send_reply(&mut con, reply_target, &format!("Error while removing ban #{}: {}", ban_id, e));
                                }
                            }
                        }
                        BotAction::BanDelAll => {
                            match clients::ban_del_all(&mut con) {
                                Ok(()) => {
                                    audit_logger.log(audit::AuditEntry {
                                        invoker_name: invoker_name.clone(),
                                        invoker_uid: invoker_uid.clone(),
                                        action: "BAN_DEL_ALL".into(),
                                        target: Some("all bans".into()),
                                        result: audit::AuditResult::Success,
                                    });
                                    send_reply(&mut con, reply_target, "All bans were removed.");
                                }
                                Err(e) => {
                                    error!(error = %e, "Failed to delete all bans");
                                    send_reply(&mut con, reply_target, &format!("Error while removing all bans: {}", e));
                                }
                            }
                        }
                        BotAction::BanList => {
                            // BanList requires a server query response — for now, reply that we sent the request
                            send_reply(&mut con, reply_target, "Ban list request has been sent to the server. This feature is currently limited because it requires an asynchronous server response.");
                        }
                        BotAction::ClientEdit { client_name, description, is_talker } => {
                            match clients::client_edit(&mut con, client_name, description.as_deref(), *is_talker) {
                                Ok(()) => {
                                    audit_logger.log(audit::AuditEntry {
                                        invoker_name: invoker_name.clone(),
                                        invoker_uid: invoker_uid.clone(),
                                        action: "CLIENT_EDIT".into(),
                                        target: Some(client_name.clone()),
                                        result: audit::AuditResult::Success,
                                    });
                                }
                                Err(e) => {
                                    error!(error = %e, "Failed to edit client");
                                    send_reply(&mut con, reply_target, &format!("Error while editing client '{}': {}", client_name, e));
                                }
                            }
                        }
                        BotAction::ChannelMoveAction { channel_name, parent_channel_name } => {
                            match channels::channel_move(&mut con, channel_name, parent_channel_name) {
                                Ok(()) => {
                                    audit_logger.log(audit::AuditEntry {
                                        invoker_name: invoker_name.clone(),
                                        invoker_uid: invoker_uid.clone(),
                                        action: "CHANNEL_MOVE".into(),
                                        target: Some(format!("{} -> {}", channel_name, parent_channel_name)),
                                        result: audit::AuditResult::Success,
                                    });
                                }
                                Err(e) => {
                                    error!(error = %e, "Failed to move channel");
                                    send_reply(&mut con, reply_target, &format!("Error while moving channel '{}': {}", channel_name, e));
                                }
                            }
                        }
                        BotAction::ChannelSubscribe { channel_name } => {
                            match channels::channel_subscribe(&mut con, channel_name) {
                                Ok(()) => {
                                    send_reply(&mut con, reply_target, &format!("Subscribed to channel '{}'.", channel_name));
                                }
                                Err(e) => {
                                    error!(error = %e, "Failed to subscribe to channel");
                                    send_reply(&mut con, reply_target, &format!("Error while subscribing to channel '{}': {}", channel_name, e));
                                }
                            }
                        }
                        BotAction::ChannelUnsubscribe { channel_name } => {
                            match channels::channel_unsubscribe(&mut con, channel_name) {
                                Ok(()) => {
                                    send_reply(&mut con, reply_target, &format!("Unsubscribed from channel '{}'.", channel_name));
                                }
                                Err(e) => {
                                    error!(error = %e, "Failed to unsubscribe from channel");
                                    send_reply(&mut con, reply_target, &format!("Error while unsubscribing from channel '{}': {}", channel_name, e));
                                }
                            }
                        }
                        BotAction::PlayMusic { url } => {
                            info!("AI requested Music: {}", url);
                            if let Some(old_flag) = current_music_stop_flag.take() {
                                old_flag.store(true, Ordering::SeqCst);
                            }
                            let new_stop_flag = Arc::new(AtomicBool::new(false));
                            current_music_stop_flag = Some(new_stop_flag.clone());

                            let music_tx = audio_tx.clone();
                            let url_clone = url.clone();
                            let tts_config_clone = tts_config.clone();
                            tokio::spawn(async move {
                                if let Err(e) = audio::stream_url_to_ts(url_clone, music_tx, new_stop_flag, tts_config_clone).await {
                                    error!("Failed to start music stream: {}", e);
                                }
                            });
                        }
                        BotAction::SetVolume { volume } => {
                            audio::set_volume(*volume);
                        }
                    }
                }

                // Send the text reply as a PRIVATE MESSAGE to the invoker
                let reply_text = get_reply_text(&bot_actions);
                if !reply_text.is_empty() && !unauthorized_action_attempted {
                    send_private_reply(&mut con, invoker_id, &reply_text);
                }
            }

            // Process incoming events
            event = events.next() => {
                drop(events);

                match event {
                    Some(Ok(StreamItem::BookEvents(book_events))) => {
                        for e in &book_events {
                            debug!(event = ?e, "Book event received");
                            if let Event::Message { target, invoker, message } = e {
                                // Skip messages from ourselves
                                if let Ok(state) = con.get_state() {
                                    if invoker.id == state.own_client {
                                        continue;
                                    }
                                }

                                // Rate limiting
                                if !rate_limiter.check_and_record() {
                                    warn!(
                                        invoker = %invoker.name,
                                        "Rate limit exceeded, ignoring message"
                                    );
                                    continue;
                                }

                                info!(
                                    invoker = %invoker.name,
                                    message = %message,
                                    target = ?target,
                                    "Received message"
                                );

                                // Compute the correct reply target:
                                // - For private messages: target is the bot's own ID,
                                //   so we need to reply TO the invoker instead
                                // - For channel/server messages: reply to the same target
                                let reply_target = match target {
                                    MessageTarget::Client(_) => {
                                        // Private message — reply to the sender
                                        MessageTarget::Client(invoker.id)
                                    }
                                    other => *other,
                                };

                                // Prepare the message for processing
                                let invoker_name = invoker.name.to_string();
                                let raw_message = message.to_string();

                                let mut is_admin = false;
                                let mut user_uid = "unknown".to_string();
                                let mut uid_found = false;

                                // Helper: convert raw UID bytes to Base64 string (matching config.toml format)
                                let uid_to_base64 = |raw_bytes: &[u8]| -> String {
                                    base64::engine::general_purpose::STANDARD.encode(raw_bytes)
                                };

                                // Try to get UID from server state
                                if let Ok(state) = con.get_state() {
                                    if let Some(client) = state.clients.get(&invoker.id) {
                                        if let Some(ref uid_buf) = client.uid {
                                            user_uid = uid_to_base64(&uid_buf.0);
                                            uid_found = true;
                                        }
                                    }
                                }

                                if cfg.admin_uids.contains(&user_uid) {
                                    is_admin = true;
                                    debug!(invoker = %invoker_name, uid = %user_uid, "User authenticated as ADMIN");
                                } else {
                                    debug!(invoker = %invoker_name, uid = %user_uid, "User is standard user");
                                }

                                if cfg.auto_learning_enabled && uid_found {
                                    let trust_tier = if is_admin {
                                        learning::TrustTier::Owner
                                    } else {
                                        learning::TrustTier::Member
                                    };

                                    match learning::auto_learn_from_message(
                                        &cfg.prompt_workspace_dir,
                                        &user_uid,
                                        &invoker_name,
                                        trust_tier,
                                        &raw_message,
                                        cfg.auto_learning_note_limit,
                                    ) {
                                        Ok(outcome) => {
                                            if let Some(reason) = outcome.blocked_reason {
                                                warn!(uid = %user_uid, invoker = %invoker_name, reason = %reason, "Auto-learning blocked by guardrail");
                                                audit_logger.log(audit::AuditEntry {
                                                    invoker_name: invoker_name.clone(),
                                                    invoker_uid: user_uid.clone(),
                                                    action: "MEMORY_AUTO_UPDATE".into(),
                                                    target: Some("blocked".into()),
                                                    result: audit::AuditResult::Denied(reason),
                                                });
                                            } else if outcome.has_updates() {
                                                let summary = outcome.summary();
                                                info!(uid = %user_uid, invoker = %invoker_name, summary = %summary, "Auto-learning updated user profile");
                                                audit_logger.log(audit::AuditEntry {
                                                    invoker_name: invoker_name.clone(),
                                                    invoker_uid: user_uid.clone(),
                                                    action: "MEMORY_AUTO_UPDATE".into(),
                                                    target: Some(summary),
                                                    result: audit::AuditResult::Success,
                                                });
                                            }
                                        }
                                        Err(e) => {
                                            warn!(uid = %user_uid, invoker = %invoker_name, error = %e, "Auto-learning failed");
                                            audit_logger.log(audit::AuditEntry {
                                                invoker_name: invoker_name.clone(),
                                                invoker_uid: user_uid.clone(),
                                                action: "MEMORY_AUTO_UPDATE".into(),
                                                target: None,
                                                result: audit::AuditResult::Error(e.to_string()),
                                            });
                                        }
                                    }
                                }

                                // Handle local $ticket command system purely in Rust
                                if raw_message.starts_with("$ticket") {
                                    let content = raw_message.trim_start_matches("$ticket").trim();
                                    let parts: Vec<&str> = content.split_whitespace().collect();

                                    let mut store = ticket_store.write().await;

                                    if content.is_empty() {
                                        if is_admin {
                                            send_private_reply(&mut con, invoker.id, "Available commands: `$ticket list`, `$ticket read <id>`, `$ticket reply <id> <text>`, `$ticket close <id>`\nTo create a ticket, type: `$ticket <message>`");
                                        } else {
                                            send_private_reply(&mut con, invoker.id, "To open a support ticket, type:\n`$ticket Describe your problem here`\nManage your tickets with: `$ticket list`, `$ticket read <id>`, `$ticket reply <id> <text>`, `$ticket close <id>`");
                                        }
                                    } else if parts[0] == "list" {
                                        let open_tickets = store.get_open_tickets();
                                        let mut visible_tickets = Vec::new();
                                        for t in open_tickets {
                                            if is_admin || t.creator_uid == user_uid {
                                                visible_tickets.push(t);
                                            }
                                        }

                                        if visible_tickets.is_empty() {
                                            send_private_reply(&mut con, invoker.id, "No active tickets found.");
                                        } else {
                                            let mut reply = String::from("Active Tickets:\n");
                                            for t in visible_tickets {
                                                let claim_info = t.claimed_by.as_ref().map(|a| format!(" (Claimed by: {})", a)).unwrap_or_default();
                                                let status_str = match t.status {
                                                    tickets::TicketStatus::Open => "[Open]",
                                                    tickets::TicketStatus::Answered => "[Waiting for your reply]",
                                                    _ => "",
                                                };
                                                reply.push_str(&format!("- ID: **{}** {} | From: {}{}\n", t.id, status_str, t.creator_name, claim_info));
                                            }
                                            send_private_reply(&mut con, invoker.id, &reply);
                                        }
                                    } else if parts[0] == "read" && parts.len() > 1 {
                                        if let Ok(id) = parts[1].parse::<u64>() {
                                            if let Some(t) = store.get_ticket(id) {
                                                if is_admin || t.creator_uid == user_uid {
                                                    let status_str = if t.status == tickets::TicketStatus::Open { "Open" } else { "Closed" };
                                                    let response_text = t.response.unwrap_or_else(|| "No response yet.".to_string());
                                                    let reply = format!("**Ticket #{}** ({})\nUser: {}\nDate: {}\n\n**Message:**\n{}\n\n**Response:**\n{}", t.id, status_str, t.creator_name, t.created_at, t.content, response_text);
                                                    send_private_reply(&mut con, invoker.id, &reply);
                                                } else {
                                                    send_private_reply(&mut con, invoker.id, "You do not have access to this ticket.");
                                                }
                                            } else {
                                                send_private_reply(&mut con, invoker.id, "Ticket not found.");
                                            }
                                        }
                                    } else if parts[0] == "reply" && parts.len() > 2 {
                                        if let Ok(id) = parts[1].parse::<u64>() {
                                            if let Some(t) = store.get_ticket(id) {
                                                if is_admin || t.creator_uid == user_uid {
                                                    let response = parts[2..].join(" ");
                                                    match store.reply_ticket(id, response.clone(), is_admin) {
                                                        Ok(Some(ticket)) => {
                                                            send_private_reply(&mut con, invoker.id, &format!("Reply saved for ticket #{}.", id));

                                                            if is_admin {
                                                                let mut target_cid = None;
                                                                if let Ok(state) = con.get_state() {
                                                                    for (cid, client) in &state.clients {
                                                                        if let Some(ref uid_buf) = client.uid {
                                                                            let current_uid = uid_to_base64(&uid_buf.0);
                                                                            if current_uid == ticket.creator_uid {
                                                                                target_cid = Some(*cid);
                                                                                break;
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                                if let Some(cid) = target_cid {
                                                                    send_private_reply(&mut con, cid, &format!("Admin replied to your ticket #{}!\n\nResponse:\n{}", id, response));
                                                                }
                                                            } else {
                                                                let alert_msg = format!("User replied to ticket #{}\nUser: {}\nResponse: {}", id, invoker_name, response);
                                                                let mut admin_cids = Vec::new();
                                                                if let Ok(state) = con.get_state() {
                                                                    for (cid, client) in &state.clients {
                                                                        if let Some(ref uid_buf) = client.uid {
                                                                            let current_uid = uid_to_base64(&uid_buf.0);
                                                                            if cfg.admin_uids.contains(&current_uid) {
                                                                                admin_cids.push(*cid);
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                                for cid in admin_cids {
                                                                    send_private_reply(&mut con, cid, &alert_msg);
                                                                }
                                                            }
                                                        },
                                                        _ => send_private_reply(&mut con, invoker.id, "Failed to save the reply.")
                                                    }
                                                } else {
                                                    send_private_reply(&mut con, invoker.id, "You do not have access to this ticket.");
                                                }
                                            } else {
                                                send_private_reply(&mut con, invoker.id, "Ticket not found.");
                                            }
                                        }
                                    } else if parts[0] == "claim" && parts.len() > 1 && is_admin {
                                        if let Ok(id) = parts[1].parse::<u64>() {
                                            match store.claim_ticket(id, invoker_name.clone()) {
                                                Ok(Some(_ticket)) => {
                                                    send_private_reply(&mut con, invoker.id, &format!("You claimed ticket #{}.", id));

                                                    let alert_msg = format!("Admin {} claimed ticket #{}", invoker_name, id);
                                                    let mut admin_cids = Vec::new();
                                                    if let Ok(state) = con.get_state() {
                                                        for (cid, client) in &state.clients {
                                                            if client.id != invoker.id {
                                                                if let Some(ref uid_buf) = client.uid {
                                                                    let current_uid = uid_to_base64(&uid_buf.0);
                                                                    if cfg.admin_uids.contains(&current_uid) {
                                                                        admin_cids.push(*cid);
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                    for cid in admin_cids {
                                                        send_private_reply(&mut con, cid, &alert_msg);
                                                    }
                                                }
                                                _ => send_private_reply(&mut con, invoker.id, "Ticket not found."),
                                            }
                                        }
                                    } else if parts[0] == "history" && parts.len() > 1 && is_admin {
                                        let target_name = parts[1..].join(" ");
                                        let history_tickets = store.get_user_history(&target_name);

                                        if history_tickets.is_empty() {
                                            send_private_reply(&mut con, invoker.id, &format!("No ticket history found for user: {}", target_name));
                                        } else {
                                            let mut reply = format!("Ticket history for {}:\n\n", target_name);
                                            for t in history_tickets.iter().take(5) {
                                                reply.push_str(&format!("**#{}:** {}\n", t.id, t.content));
                                            }
                                            if history_tickets.len() > 5 {
                                                reply.push_str(&format!("... and {} more tickets.\n", history_tickets.len() - 5));
                                            }
                                            send_private_reply(&mut con, invoker.id, &reply);
                                        }
                                    } else if parts[0] == "close" && parts.len() > 1 {
                                        if let Ok(id) = parts[1].parse::<u64>() {
                                            if let Some(t) = store.get_ticket(id) {
                                                if is_admin || t.creator_uid == user_uid {
                                                    match store.close_ticket(id) {
                                                        Ok(true) => send_private_reply(&mut con, invoker.id, &format!("Ticket #{} has been closed.", id)),
                                                        _ => send_private_reply(&mut con, invoker.id, "Failed to close ticket."),
                                                    }
                                                } else {
                                                    send_private_reply(&mut con, invoker.id, "You do not have permission to close this ticket.");
                                                }
                                            } else {
                                                send_private_reply(&mut con, invoker.id, "Ticket not found.");
                                            }
                                        }
                                    } else {
                                        // Fallback: This is not a known command -> Treat as creating a new ticket

                                        // Restrict users to 1 open ticket at a time to prevent spam
                                        let has_open_ticket = if !is_admin {
                                            store.get_open_tickets().iter().any(|t| t.creator_uid == user_uid)
                                        } else {
                                            false
                                        };

                                        if has_open_ticket {
                                            send_private_reply(&mut con, invoker.id, "You already have one open ticket. Please wait for an admin reply or close it with `$ticket close <id>` before opening a new ticket.");
                                        } else {
                                            match store.create_ticket(user_uid.clone(), invoker_name.clone(), content.to_string()) {
                                                Ok(id) => {
                                                    send_private_reply(&mut con, invoker.id, &format!("Your ticket has been created successfully as **#{}**. Admins have been notified and will respond soon.", id));

                                                    let alert_msg = format!("NEW TICKET #{}\nUser: {}\nIssue: {}", id, invoker_name, content);
                                                    let mut admin_cids = Vec::new();
                                                    if let Ok(state) = con.get_state() {
                                                        for (cid, client) in &state.clients {
                                                            if client.id != invoker.id { // don't notify the admin if they created it
                                                                if let Some(ref uid_buf) = client.uid {
                                                                    let current_uid = uid_to_base64(&uid_buf.0);
                                                                    if cfg.admin_uids.contains(&current_uid) {
                                                                        admin_cids.push(*cid);
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                    for cid in admin_cids {
                                                        send_private_reply(&mut con, cid, &alert_msg);
                                                    }
                                                }
                                                Err(e) => {
                                                    error!(error = %e, "Failed to create ticket");
                                                    send_private_reply(&mut con, invoker.id, "An error occurred while creating your ticket. Please try again later.");
                                                }
                                            }
                                        }
                                    }
                                    continue;
                                }

                                // Record name for Identity Tracking on message
                                if uid_found {
                                    let mut id_store = identity_store.write().await;
                                    let _ = id_store.record_name(&user_uid, &invoker_name);
                                }

                                // $identity command
                                if raw_message.starts_with("$identity") && is_admin {
                                    let content = raw_message.trim_start_matches("$identity").trim();
                                    if content.is_empty() {
                                        send_private_reply(&mut con, invoker.id, "Usage: `$identity <uid>`");
                                    } else {
                                        let uid_to_check = content;
                                        let id_store = identity_store.read().await;
                                        if let Some(history) = id_store.get_history(uid_to_check) {
                                            let history_str = history.join(" -> ");
                                            send_private_reply(&mut con, invoker.id, &format!("Identity for UID: {}\n\nName history:\n`UID -> {}`", uid_to_check, history_str));
                                        } else {
                                            send_private_reply(&mut con, invoker.id, &format!("No records found for UID: {}", uid_to_check));
                                        }
                                    }
                                    continue;
                                }

                                // $memory command (admin only)
                                if raw_message.starts_with("$memory") {
                                    if !is_admin {
                                        send_private_reply(&mut con, invoker.id, "Only administrators can use `$memory`.");
                                        continue;
                                    }

                                    let content = raw_message.trim_start_matches("$memory").trim();
                                    if content.is_empty() || content.eq_ignore_ascii_case("help") {
                                        send_private_reply(
                                            &mut con,
                                            invoker.id,
                                            "Usage:\n- `$memory show <uid>` - shows USER.md and recent auto-learning entries for that user.",
                                        );
                                        continue;
                                    }

                                    let mut parts = content.split_whitespace();
                                    match parts.next().unwrap_or_default().to_lowercase().as_str() {
                                        "show" => {
                                            let uid_to_check = parts.collect::<Vec<_>>().join(" ");
                                            if uid_to_check.trim().is_empty() {
                                                send_private_reply(&mut con, invoker.id, "Usage: `$memory show <uid>`");
                                            } else {
                                                let report = build_memory_show_report(
                                                    &cfg.prompt_workspace_dir,
                                                    uid_to_check.trim(),
                                                );
                                                send_private_reply(&mut con, invoker.id, &report);
                                            }
                                        }
                                        _ => {
                                            send_private_reply(&mut con, invoker.id, "Unknown `$memory` sub-command. Use: `$memory show <uid>`");
                                        }
                                    }
                                    continue;
                                }

                                // ====== ADMIN-ONLY AUDIO COMMANDS ======
                                let admin_denied_msg = "Only administrators can use this command.";

                                // $play <url> — Admin only
                                if raw_message.starts_with("$play ") {
                                    if !is_admin {
                                        send_private_reply(&mut con, invoker.id, admin_denied_msg);
                                        continue;
                                    }
                                    let url = raw_message.trim_start_matches("$play ").trim().to_string();
                                    info!(url = %url, invoker = %invoker_name, "Manual play requested");

                                    // Stop previous
                                    if let Some(old_flag) = current_music_stop_flag.take() {
                                        old_flag.store(true, Ordering::SeqCst);
                                    }
                                    let new_stop_flag = Arc::new(AtomicBool::new(false));
                                    current_music_stop_flag = Some(new_stop_flag.clone());
                                    let music_tx = audio_tx.clone();
                                    let url_clone = url.clone();
                                    let tts_config_clone = tts_config.clone();

                                    tokio::spawn(async move {
                                        if let Err(e) = audio::stream_url_to_ts(url_clone, music_tx, new_stop_flag, tts_config_clone).await {
                                            error!("Failed to start music stream: {}", e);
                                        }
                                    });
                                    send_private_reply(&mut con, invoker.id, "Starting music playback...");
                                    continue;
                                }

                                // $stop — Admin only
                                if raw_message == "$stop" {
                                    if !is_admin {
                                        send_private_reply(&mut con, invoker.id, admin_denied_msg);
                                        continue;
                                    }
                                    if let Some(old_flag) = current_music_stop_flag.take() {
                                        old_flag.store(true, Ordering::SeqCst);
                                    }
                                    send_private_reply(&mut con, invoker.id, "Music stopped.");
                                    continue;
                                }

                                // $vol — Admin only
                                if raw_message.starts_with("$vol") {
                                    if !is_admin {
                                        send_private_reply(&mut con, invoker.id, admin_denied_msg);
                                        continue;
                                    }
                                    let content = raw_message.trim_start_matches("$vol").trim();
                                    if let Ok(v) = content.parse::<u8>() {
                                        audio::set_volume(v);
                                        send_private_reply(&mut con, invoker.id, &format!("Volume set to {}%.", v));
                                    } else {
                                        send_private_reply(&mut con, invoker.id, &format!("Current volume: {}%. Use `$vol [0-100]` to change it.", audio::get_volume()));
                                    }
                                    continue;
                                }

                                // ====== RADIO PLAYLIST COMMANDS (Admin only) ======

                                // $radios — List all radio stations (anyone can see)
                                if raw_message == "$radios" {
                                    let radios = load_radios(&radios_file);
                                    if radios.is_empty() {
                                        send_private_reply(&mut con, invoker.id, "No saved radio stations. Admin can add one with `$addradio name url`.");
                                    } else {
                                        let mut list = String::from("Radio stations:\n");
                                        for name in radios.keys() {
                                            list.push_str(&format!("  • {}\n", name));
                                        }
                                        list.push_str("\nUse: `$radio name` to play.");
                                        send_private_reply(&mut con, invoker.id, &list);
                                    }
                                    continue;
                                }

                                // $radio <name> — Play a saved radio station (Admin only)
                                if raw_message.starts_with("$radio ") {
                                    if !is_admin {
                                        send_private_reply(&mut con, invoker.id, admin_denied_msg);
                                        continue;
                                    }
                                    let name = raw_message.trim_start_matches("$radio ").trim();
                                    let radios = load_radios(&radios_file);

                                    // Case-insensitive search
                                    let found = radios.iter().find(|(k, _)| k.to_lowercase() == name.to_lowercase());

                                    if let Some((station_name, url)) = found {
                                        info!(station = %station_name, url = %url, invoker = %invoker_name, "Radio play requested");

                                        // Stop previous
                                        if let Some(old_flag) = current_music_stop_flag.take() {
                                            old_flag.store(true, Ordering::SeqCst);
                                        }
                                        let new_stop_flag = Arc::new(AtomicBool::new(false));
                                        current_music_stop_flag = Some(new_stop_flag.clone());
                                        let music_tx = audio_tx.clone();
                                        let url_clone = url.clone();
                                        let tts_config_clone = tts_config.clone();

                                        tokio::spawn(async move {
                                            if let Err(e) = audio::stream_url_to_ts(url_clone, music_tx, new_stop_flag, tts_config_clone).await {
                                                error!("Failed to start radio stream: {}", e);
                                            }
                                        });
                                        send_private_reply(&mut con, invoker.id, &format!("Playing radio: {}", station_name));
                                    } else {
                                        let available: Vec<String> = radios.keys().cloned().collect();
                                        send_private_reply(&mut con, invoker.id, &format!("Radio '{}' does not exist.\nAvailable stations: {}", name, available.join(", ")));
                                    }
                                    continue;
                                }

                                // $addradio <name> <url> — Admin only
                                if raw_message.starts_with("$addradio ") {
                                    if !is_admin {
                                        send_private_reply(&mut con, invoker.id, admin_denied_msg);
                                        continue;
                                    }
                                    let args = raw_message.trim_start_matches("$addradio ").trim();
                                    // Parse: name can be quoted or single word, url is the last token
                                    if let Some((name, url)) = parse_radio_args(args) {
                                        let mut radios = load_radios(&radios_file);
                                        radios.insert(name.clone(), url.clone());
                                        save_radios(&radios_file, &radios);
                                        send_private_reply(&mut con, invoker.id, &format!("Radio '{}' added with URL: {}", name, url));
                                    } else {
                                        send_private_reply(&mut con, invoker.id, "Format: `$addradio \"Station Name\" https://link`");
                                    }
                                    continue;
                                }

                                // $delradio <name> — Admin only
                                if raw_message.starts_with("$delradio ") {
                                    if !is_admin {
                                        send_private_reply(&mut con, invoker.id, admin_denied_msg);
                                        continue;
                                    }
                                    let name = raw_message.trim_start_matches("$delradio ").trim();
                                    let mut radios = load_radios(&radios_file);
                                    let found_key = radios.keys().find(|k| k.to_lowercase() == name.to_lowercase()).cloned();
                                    if let Some(key) = found_key {
                                        radios.remove(&key);
                                        save_radios(&radios_file, &radios);
                                        send_private_reply(&mut con, invoker.id, &format!("Radio '{}' removed.", key));
                                    } else {
                                        send_private_reply(&mut con, invoker.id, &format!("Radio '{}' does not exist.", name));
                                    }
                                    continue;
                                }

                                // $editradio <name> <new_url> — Admin only
                                if raw_message.starts_with("$editradio ") {
                                    if !is_admin {
                                        send_private_reply(&mut con, invoker.id, admin_denied_msg);
                                        continue;
                                    }
                                    let args = raw_message.trim_start_matches("$editradio ").trim();
                                    if let Some((name, url)) = parse_radio_args(args) {
                                        let mut radios = load_radios(&radios_file);
                                        let found_key = radios.keys().find(|k| k.to_lowercase() == name.to_lowercase()).cloned();
                                        if let Some(key) = found_key {
                                            radios.insert(key.clone(), url.clone());
                                            save_radios(&radios_file, &radios);
                                            send_private_reply(&mut con, invoker.id, &format!("Radio '{}' updated with URL: {}", key, url));
                                        } else {
                                            send_private_reply(&mut con, invoker.id, &format!("Radio '{}' does not exist. Use `$addradio` to add it.", name));
                                        }
                                    } else {
                                        send_private_reply(&mut con, invoker.id, "Format: `$editradio \"Station Name\" https://new-link`");
                                    }
                                    continue;
                                }

                                // Check for trigger command "$ask" or "$tts" (required for AI message types)
                                let (should_process, user_message) = if raw_message.starts_with("$ask ") {
                                    (true, raw_message.trim_start_matches("$ask ").to_string())
                                } else if raw_message == "$ask" {
                                    (true, "".to_string())
                                } else if raw_message.starts_with("$tts ") || raw_message == "$tts" || raw_message.starts_with("$tts") {
                                    // Admin check for $tts
                                    if !is_admin {
                                        send_private_reply(&mut con, invoker.id, admin_denied_msg);
                                        continue;
                                    }
                                    // Passes the full $tts command to the AI so the system prompt can trigger PLAY_TTS
                                    (true, raw_message)
                                } else {
                                    // If this is a private message and it doesn't have $ask, $tts, or $ticket, auto-reply with instructions
                                    if let MessageTarget::Client(_) = reply_target {
                                        send_private_reply(&mut con, invoker.id, "You can use me like this:\n- Create a permanent room for yourself\n- Assign an allowed server group to yourself\n- Open a support ticket with `$ticket your problem`\n\nJust type `$ask` followed by your question/command, or try `$tts` for voice output.");
                                    }
                                    (false, raw_message)
                                };

                                if !should_process {
                                    debug!("Message does not start with $ask, $tts, or $ticket, ignoring");
                                    continue;
                                }

                                // Check if user is an admin based on their Unique ID from config
                                let mut user_uid = "unknown".to_string();
                                let mut uid_found = false;

                                // Helper: convert raw UID bytes to Base64 string (matching config.toml format)
                                use base64::Engine;
                                let uid_to_base64 = |raw_bytes: &[u8]| -> String {
                                    base64::engine::general_purpose::STANDARD.encode(raw_bytes)
                                };

                                // Try to get UID from server state
                                if let Ok(state) = con.get_state() {
                                    if let Some(client) = state.clients.get(&invoker.id) {
                                        if let Some(ref uid_buf) = client.uid {
                                            user_uid = uid_to_base64(&uid_buf.0);
                                            uid_found = true;
                                        }
                                    }
                                    // Fallback: search by name
                                    if !uid_found {
                                        for client in state.clients.values() {
                                            if client.name == invoker_name {
                                                if let Some(ref uid_buf) = client.uid {
                                                    user_uid = uid_to_base64(&uid_buf.0);
                                                    uid_found = true;
                                                }
                                                break;
                                            }
                                        }
                                    }
                                }

                                // Fallback: try from the invoker event
                                if !uid_found {
                                    if let Some(uid_buf) = &invoker.uid {
                                        user_uid = uid_to_base64(&uid_buf.0);
                                        uid_found = true;
                                    }
                                }

                                // Determine permission level
                                let perm_level = if uid_found {
                                    let level = permissions::get_permission_level(&user_uid, &cfg);
                                    info!(uid = %user_uid, perm = %level, "Permission check result");
                                    level
                                } else {
                                    warn!(invoker = %invoker_name, "Could not determine UID");
                                    permissions::PermissionLevel::User
                                };

                                // Build rich context for the AI
                                let server_snapshot = context::ServerSnapshot::from_connection(&con)
                                    .unwrap_or_else(|| context::ServerSnapshot {
                                        server_name: "Unknown".into(),
                                        bot_name: cfg.bot_name.clone(),
                                        bot_channel: "Unknown".into(),
                                        bot_uid: String::new(),
                                        platform: String::new(),
                                        version: String::new(),
                                        max_clients: 0,
                                        uptime_secs: None,
                                        connections_total: None,
                                        packetloss_total: None,
                                        avg_ping_ms: None,
                                        online_users: Vec::new(),
                                        channel_tree: Vec::new(),
                                    });

                                let invoker_ctx = context::InvokerContext::from_event(
                                    &con,
                                    invoker.id,
                                    &invoker_name,
                                    &user_uid,
                                    &reply_target,
                                );

                                let server_groups_text = cfg.allowed_server_groups
                                    .iter()
                                    .map(|(k, v)| format!("{}={}", k, v))
                                    .collect::<Vec<_>>()
                                    .join(", ");

                                let workspace_context = prompt_workspace::load_workspace_context(
                                    &cfg.prompt_workspace_dir,
                                    &user_uid,
                                    &invoker_name,
                                    cfg.prompt_file_max_chars,
                                    cfg.prompt_total_max_chars,
                                );

                                let perm_description = permissions::describe_level(perm_level);
                                let system_prompt = prompts::build_system_prompt(
                                    &invoker_ctx,
                                    &server_snapshot,
                                    perm_level,
                                    perm_description,
                                    &server_groups_text,
                                    &workspace_context,
                                    cfg.code_output_channel_id,
                                );

                                let request_id = next_request_id.fetch_add(1, Ordering::SeqCst);
                                let queue_position = pending_ai_requests.fetch_add(1, Ordering::SeqCst) + 1;
                                let queue_item = QueuedAiRequest {
                                    request_id,
                                    reply_target,
                                    invoker_id: invoker.id,
                                    perm_level,
                                    invoker_name: invoker_name.clone(),
                                    invoker_uid: user_uid.clone(),
                                    system_prompt,
                                    user_message,
                                };

                                match ai_queue_tx.try_send(queue_item) {
                                    Ok(()) => {
                                        info!(
                                            request_id,
                                            invoker = %invoker_name,
                                            uid = %user_uid,
                                            queue_position,
                                            "Queued AI request"
                                        );
                                        if queue_position <= 1 {
                                            send_private_reply(
                                                &mut con,
                                                invoker.id,
                                                &format!(
                                                    "Your request was queued (ID #{}) and will be processed now.",
                                                    request_id
                                                ),
                                            );
                                        } else {
                                            send_private_reply(
                                                &mut con,
                                                invoker.id,
                                                &format!(
                                                    "Your request was queued (ID #{}) and will be processed soon. Queue position: {}.",
                                                    request_id,
                                                    queue_position
                                                ),
                                            );
                                        }
                                    }
                                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                        pending_ai_requests.fetch_sub(1, Ordering::SeqCst);
                                        warn!(
                                            request_id,
                                            invoker = %invoker_name,
                                            uid = %user_uid,
                                            "Global AI queue is full"
                                        );
                                        send_private_reply(
                                            &mut con,
                                            invoker.id,
                                            "The global queue is currently full. Please try again in a few seconds.",
                                        );
                                    }
                                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                        pending_ai_requests.fetch_sub(1, Ordering::SeqCst);
                                        error!(
                                            request_id,
                                            invoker = %invoker_name,
                                            uid = %user_uid,
                                            "Global AI queue is unavailable"
                                        );
                                        send_private_reply(
                                            &mut con,
                                            invoker.id,
                                            "I cannot process requests right now. Please try again shortly.",
                                        );
                                    }
                                }
                            } else if let Event::PropertyChanged { id: PropertyId::ClientChannel(client_id), .. } = e {
                                // A client changed their channel. Let's check if they moved into OUR channel.
                                let mut to_welcome = None;
                                if let Ok(state) = con.get_state() {
                                    if let Some(client) = state.clients.get(client_id) {
                                        // Make sure it's not the bot itself moving
                                        if client.id != state.own_client {
                                            if let Some(own_client_data) = state.clients.get(&state.own_client) {
                                                // Did they just join our channel?
                                                if client.channel == own_client_data.channel {
                                                    to_welcome = Some((client.id, client.name.to_string()));
                                                }
                                            }
                                        }
                                    }
                                }

                                if let Some((id, name)) = to_welcome {
                                    let welcome_msg = format!(
                                        "Hello [b]{}[/b]!\n\n[u]How to use this bot:[/u]\n\
                                        - Create a permanent room for yourself\n\
                                        - Assign an allowed server group to yourself\n\
                                        - Open a support ticket with `$ticket your problem`\n\n\
                                        Type `$ask` followed by your question/command.",
                                        name
                                    );
                                    send_private_reply(&mut con, id, &welcome_msg);
                                }
                            } else if let Event::PropertyRemoved { id: PropertyId::Client(client_id), .. } = e {
                                // A client disconnected or left our view.
                            let mut client_name = None;
                            let mut user_uid = None;
                            if let Ok(state) = con.get_state() {
                                if let Some(c) = state.clients.get(client_id) {
                                    client_name = Some(c.name.to_lowercase());
                                    if let Some(ref uid_buf) = c.uid {
                                        use base64::Engine;
                                        user_uid = Some(base64::engine::general_purpose::STANDARD.encode(&uid_buf.0));
                                    }
                                }
                            }
                            if let Some(name) = client_name {
                                    if let Some(removed) = pre_move_channels.remove(&name) {
                                        info!(client = %name, returned_channel = %removed, "Cleaned up pre_move_channels");
                                    }
                                }
                                if let Some(uid) = user_uid {
                                    if history_store.remove(&uid).is_some() {
                                        info!(uid = %uid, "Cleaned up history_store");
                                    }
                                }
                            } else if let Event::PropertyAdded { id: PropertyId::Client(client_id), .. } = e {
                                // A client connected/entered our view
                                if let Ok(state) = con.get_state() {
                                    if let Some(client) = state.clients.get(client_id) {
                                        if client.id != state.own_client {
                                            if let Some(ref uid_buf) = client.uid {
                                                use base64::Engine;
                                                let current_uid = base64::engine::general_purpose::STANDARD.encode(&uid_buf.0);

                                                // Record name for Identity Tracking on connection
                                                {
                                                    let mut id_store = identity_store.write().await;
                                                    let _ = id_store.record_name(&current_uid, &client.name);
                                                }

                                                if let Some(own_client_data) = state.clients.get(&state.own_client) {
                                                    if client.channel == own_client_data.channel {
                                                        let welcome_msg = format!(
                                                            "Hello [b]{}[/b]!\n\n[u]How to use this bot:[/u]\n\
                                                            - Create a permanent room for yourself\n\
                                                            - Assign an allowed server group to yourself\n\
                                                            - Open a support ticket with `$ticket your problem`\n\n\
                                                            Type `$ask` followed by your question/command.",
                                                            client.name
                                                        );
                                                        send_private_reply(&mut con, *client_id, &welcome_msg);
                                                    }
                                                }

                                                let unread_tickets = {
                                                    let store = ticket_store.read().await;
                                                    store.get_unread_tickets(&current_uid)
                                                };

                                                if !unread_tickets.is_empty() {
                                                    let mut reply = String::from("You have unread admin replies:\n\n");
                                                    for t in unread_tickets {
                                                        let response_text = t.response.clone().unwrap_or_else(|| "No response.".to_string());
                                                        reply.push_str(&format!("**Ticket #{}** - Response:\n{}\n\n", t.id, response_text));
                                                    }
                                                    reply.push_str("Reply with `$ticket reply <id> <text>`.\nClose a ticket with `$ticket close <id>`.");

                                                    // Send the notification immediately
                                                    send_private_reply(&mut con, *client_id, &reply);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    Some(Ok(other)) => {
                        // Log other stream items for debugging voice capture
                        debug!("Other stream item: {:?}", std::mem::discriminant(&other));
                    }

                    Some(Err(e)) => {
                        error!(error = %e, "Connection error");
                    }

                    None => {
                        info!("Connection stream ended");
                        break;
                    }
                }

                // After processing events and updating state, check for pending channel setups
                if !pending_creations.is_empty() {
                    let mut completed = Vec::new();
                    let mut ready_to_setup = Vec::new();

                    // Step 1: Identify which pending creations are ready (Scope the immutable borrow)
                    if let Ok(state) = con.get_state() {
                        for (idx, pending) in pending_creations.iter().enumerate() {
                            // Check for timeout (15s)
                            if pending.created_at.elapsed() > Duration::from_secs(15) {
                                warn!(channel = %pending.channel_name, "Pending channel setup timed out after 15s");
                                completed.push(idx);
                                continue;
                            }

                            if state.channels.values().any(|c| c.name.to_lowercase() == pending.channel_name.to_lowercase()) {
                                info!(channel = %pending.channel_name, "Found newly created channel in state, will setup...");

                                if let Some(client) = state.clients.get(&pending.invoker_id) {
                                    ready_to_setup.push((
                                        idx,
                                        pending.channel_name.clone(),
                                        client.database_id.0,
                                        client.name.to_string()
                                    ));
                                } else {
                                    completed.push(idx); // Client gone, nothing to do
                                }
                            }
                        }
                    }

                    // Step 2: Perform mutable actions on connection
                    for (idx, channel_name, db_id, client_name) in ready_to_setup {
                        // Move the creator into the new channel first
                        if let Err(e) = clients::move_client(&mut con, &client_name, &channel_name) {
                            warn!(error = %e, channel = %channel_name, "Failed to move creator into new channel");
                        }

                        // Small delay to ensure the server registers the client in the channel
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

                        // Then set the admin group
                        if let Err(e) = channels::set_channel_admin(
                            &mut con,
                            &channel_name,
                            db_id,
                            cfg.channel_admin_group_id,
                        ) {
                            warn!(error = %e, channel = %channel_name, "Failed to set Channel Admin for creator");
                        }

                        // Post-action confirmation from Rust (replaces AI predicting this step)
                        // In pending vector, the invoker_id is available. ready_to_setup only contains idx right now,
                        // so we need the original pending object to get the invoker_id
                        let invoker = pending_creations[idx].invoker_id;
                        send_private_reply(&mut con, invoker, &format!("I created your room '{}' and moved you into it. Do you want to stay here or be moved back to your previous room?", channel_name));

                        completed.push(idx);
                    }

                    // Step 3: Remove completed items in reverse order
                    completed.sort_unstable();
                    for idx in completed.into_iter().rev() {
                        pending_creations.remove(idx);
                    }
                }
            }
        }
    }

    // ── Disconnect ──────────────────────────────────────────
    info!("Disconnecting from server...");
    con.disconnect(
        DisconnectOptions::new()
            .reason(Reason::Clientdisconnect)
            .message(cfg.disconnect_message),
    )?;
    con.events().for_each(|_| future::ready(())).await;
    info!("Disconnected. Goodbye!");

    Ok(())
}

// ─── Helpers ────────────────────────────────────────────────

fn resolve_client_id_by_uid(con: &Connection, target_uid: &str) -> Option<tsproto_types::ClientId> {
    let uid = target_uid.trim();
    if uid.is_empty() || uid == "unknown" {
        return None;
    }

    let state = con.get_state().ok()?;
    use base64::Engine;

    for (client_id, client) in state.clients.iter() {
        let Some(uid_buf) = &client.uid else {
            continue;
        };
        let current_uid = base64::engine::general_purpose::STANDARD.encode(&uid_buf.0);
        if current_uid == uid {
            return Some(*client_id);
        }
    }

    None
}

/// Send a text message reply back to the user/channel.
fn send_reply(con: &mut Connection, target: MessageTarget, message: &str) {
    // TeamSpeak has a ~8KB message limit; split if needed
    let max_len = 8000;
    let chunks: Vec<&str> = if message.len() <= max_len {
        vec![message]
    } else {
        message
            .as_bytes()
            .chunks(max_len)
            .map(|chunk| std::str::from_utf8(chunk).unwrap_or("[message truncated]"))
            .collect()
    };

    for chunk in chunks {
        match con.get_state() {
            Ok(state) => {
                if let Err(e) = state.send_message(target, chunk).send(con) {
                    error!(error = %e, "Failed to send message");
                }
            }
            Err(e) => {
                error!(error = %e, "Failed to get connection state for sending");
            }
        }
    }
}

/// Send an alert to all online administrators.
fn alert_admins(con: &mut Connection, admin_uids: &[String], message: &str) {
    let mut online_admins = Vec::new();
    if let Ok(state) = con.get_state() {
        use base64::Engine;
        for (client_id, client) in state.clients.iter() {
            if let Some(ref uid_buf) = client.uid {
                let user_uid = base64::engine::general_purpose::STANDARD.encode(&uid_buf.0);
                if admin_uids.contains(&user_uid) {
                    online_admins.push(*client_id);
                }
            }
        }
    }

    for admin_id in online_admins {
        send_private_reply(con, admin_id, message);
    }
}

/// Load radio stations from a JSON file.
fn load_radios(radios_file: &str) -> std::collections::BTreeMap<String, String> {
    let path = std::path::Path::new(radios_file);
    if !path.exists() {
        return std::collections::BTreeMap::new();
    }
    match std::fs::read_to_string(path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => std::collections::BTreeMap::new(),
    }
}

/// Save radio stations to a JSON file.
fn save_radios(radios_file: &str, radios: &std::collections::BTreeMap<String, String>) {
    if let Ok(data) = serde_json::to_string_pretty(radios) {
        let _ = std::fs::write(radios_file, data);
    }
}

/// Parse radio command args: "Name" url  OR  Name url (single word name)
fn parse_radio_args(args: &str) -> Option<(String, String)> {
    let args = args.trim();
    if let Some(stripped) = args.strip_prefix('"') {
        // Quoted name: "Some Name" url
        if let Some(end_quote) = stripped.find('"') {
            let name = stripped[..end_quote].to_string();
            let rest = stripped[end_quote + 1..].trim();
            if !rest.is_empty() {
                return Some((name, rest.to_string()));
            }
        }
    } else {
        // Unquoted: split on last space (name can be multi-word if url starts with http)
        // Find the URL part (starts with http)
        if let Some(http_pos) = args.find("http") {
            let name = args[..http_pos].trim().to_string();
            let url = args[http_pos..].trim().to_string();
            if !name.is_empty() && !url.is_empty() {
                return Some((name, url));
            }
        }
    }
    None
}

fn build_memory_show_report(workspace_dir: &str, target_uid: &str) -> String {
    let uid = target_uid.trim();
    if uid.is_empty() {
        return "UID is empty. Usage: `$memory show <uid>`".to_string();
    }

    let user_file = prompt_workspace::user_profile_path(workspace_dir, uid);
    let mut report = format!("🧠 MEMORY REPORT\nUID: {}\n", uid);

    match std::fs::read_to_string(&user_file) {
        Ok(content) => {
            report.push_str(&format!("USER.md: {}\n\n", user_file.display()));
            report.push_str("=== USER.md ===\n");
            let truncated = truncate_report_chars(&content, 5000);
            report.push_str(&truncated);
            if content.chars().count() > 5000 {
                report.push_str("\n\n[USER.md TRUNCATED]");
            }
        }
        Err(_) => {
            report.push_str(&format!(
                "USER.md not found at path: {}\n",
                user_file.display()
            ));
        }
    }

    let recent = collect_recent_memory_entries(workspace_dir, uid, 12);
    report.push_str("\n\n=== Recent memory entries ===\n");
    if recent.is_empty() {
        report.push_str("(no auto-learning entries for this UID in the last 7 days)");
    } else {
        for entry in recent {
            report.push_str("- ");
            report.push_str(&entry);
            report.push('\n');
        }
    }

    report
}

fn collect_recent_memory_entries(
    workspace_dir: &str,
    target_uid: &str,
    limit: usize,
) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }

    let marker = format!("({})", target_uid.trim());
    let memory_root = std::path::Path::new(workspace_dir).join("memory");
    let today = chrono::Local::now().date_naive();

    let mut out = Vec::new();
    for day_offset in 0..7u64 {
        let day = today - chrono::Days::new(day_offset);
        let day_str = day.format("%Y-%m-%d").to_string();
        let file = memory_root.join(format!("{}.md", day_str));

        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };

        for line in content.lines().rev() {
            let trimmed = line.trim();
            if trimmed.contains(&marker) {
                let normalized = trimmed.trim_start_matches("- ").trim();
                out.push(format!("{} | {}", day_str, normalized));
                if out.len() >= limit {
                    return out;
                }
            }
        }
    }

    out
}

fn truncate_report_chars(input: &str, max_chars: usize) -> String {
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

/// Send a private message directly to a specific client.
fn send_private_reply(con: &mut Connection, client_id: tsproto_types::ClientId, message: &str) {
    use std::borrow::Cow;
    use tsclientlib::messages::c2s::{OutSendTextMessageMessage, OutSendTextMessagePart};

    let max_len = 1024;
    let chunks: Vec<&str> = if message.len() <= max_len {
        vec![message]
    } else {
        message
            .as_bytes()
            .chunks(max_len)
            .map(|chunk| std::str::from_utf8(chunk).unwrap_or("[message truncated]"))
            .collect()
    };

    for chunk in chunks {
        let msg = OutSendTextMessageMessage::new(&mut std::iter::once(OutSendTextMessagePart {
            target: tsproto_types::TextMessageTargetMode::Client,
            target_client_id: Some(client_id),
            message: Cow::Borrowed(chunk),
        }));
        if let Err(e) = msg.send(con) {
            error!(error = %e, "Failed to send private message");
        }
    }
}

/// Sanitize a string by keeping only safe characters.
fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| {
            c.is_alphanumeric()
                || [
                    ' ', '\t', '.', ':', '-', '_', '"', '\'', '/', '(', ')', '[', ']', '{', '}',
                    '!', '?', ',', '\n',
                ]
                .contains(c)
        })
        .collect()
}
