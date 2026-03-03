use anyhow::{bail, Context, Result};
use std::borrow::Cow;
use std::iter;
use tracing::{info, warn};
use tsclientlib::messages::{c2s, OutMessageTrait};
use tsclientlib::{Connection, OutCommandExt};
use tsproto_types::{ChannelId, ClientId, Reason};

/// Find a client by their name in the current server state.
pub fn find_client_by_name(con: &Connection, name: &str) -> Result<(u16, u64)> {
    let state = con
        .get_state()
        .context("Failed to get connection state for client lookup")?;

    let name_lower = name.to_lowercase();

    for (client_id, client) in &state.clients {
        // Fallback to nickname if name doesn't match, though name should be reliable
        if client.name.to_lowercase() == name_lower {
            return Ok((client_id.0, client.database_id.0));
        }
    }

    // Try partial match if exact match fails
    for (client_id, client) in &state.clients {
        if client.name.to_lowercase().contains(&name_lower) {
            return Ok((client_id.0, client.database_id.0));
        }
    }

    warn!(client = name, "Client not found");
    bail!(
        "Could not find a user named '{}'. Please check the name and try again.",
        name
    )
}

/// Kick a client from the server or channel.
pub fn kick_client(
    con: &mut Connection,
    client_name: &str,
    reason_msg: Option<&str>,
) -> Result<()> {
    let (client_id, _) = find_client_by_name(con, client_name)?;
    let reason_str = reason_msg.unwrap_or("Kicked by AI Support Agent");

    // reason: 5 is Server kick. 4 is Channel kick. We'll do server kick.
    let msg = c2s::OutClientKickMessage::new(&mut iter::once(c2s::OutClientKickPart {
        client_id: ClientId(client_id),
        reason: Reason::KickServer,
        reason_message: Some(Cow::Borrowed(reason_str)),
    }));

    msg.to_packet()
        .send(con)
        .context("Failed to send kick command")?;

    info!(client = client_name, "Kicked client");
    Ok(())
}

/// Move a client to a specific channel.
pub fn move_client(con: &mut Connection, client_name: &str, channel_name: &str) -> Result<()> {
    let (client_id, _) = find_client_by_name(con, client_name)?;
    let channel_id = crate::channels::find_channel_by_name(con, channel_name)?;

    let msg = c2s::OutClientMoveMessage::new(&mut iter::once(c2s::OutClientMovePart {
        client_id: ClientId(client_id),
        channel_id: ChannelId(channel_id),
        channel_password: None,
    }));

    msg.to_packet()
        .send(con)
        .context("Failed to send move command")?;

    info!(client = client_name, channel = channel_name, "Moved client");
    Ok(())
}

/// Move the bot itself to a specific channel and stay there.
pub fn move_bot_to_channel(con: &mut Connection, channel_name: &str) -> Result<()> {
    let channel_id = crate::channels::find_channel_by_name(con, channel_name)?;

    let own_client_id = {
        let state = con.get_state().context("Failed to get state")?;
        let own = state
            .clients
            .get(&state.own_client)
            .context("Bot not found")?;
        if own.channel.0 == channel_id {
            info!(channel = channel_name, "Bot is already in target channel");
            return Ok(());
        }
        state.own_client
    };

    let move_msg = c2s::OutClientMoveMessage::new(&mut iter::once(c2s::OutClientMovePart {
        client_id: own_client_id,
        channel_id: ChannelId(channel_id),
        channel_password: None,
    }));

    move_msg
        .to_packet()
        .send(con)
        .context("Failed to move bot to target channel")?;

    info!(channel = channel_name, "Moved bot to channel");
    Ok(())
}

/// Poke a client with a message.
pub fn poke_client(con: &mut Connection, client_name: &str, message: &str) -> Result<()> {
    let (client_id, _) = find_client_by_name(con, client_name)?;

    let msg =
        c2s::OutClientPokeRequestMessage::new(&mut iter::once(c2s::OutClientPokeRequestPart {
            client_id: ClientId(client_id),
            message: Cow::Borrowed(message),
        }));

    msg.to_packet()
        .send(con)
        .context("Failed to send poke command")?;

    info!(client = client_name, message = message, "Poked client");
    Ok(())
}

/// Add a server group to a client.
pub fn set_server_group(
    con: &mut Connection,
    client_name: &str,
    server_group_id: u64,
) -> Result<()> {
    let (_client_id, db_id) = find_client_by_name(con, client_name)?;

    let msg = c2s::OutServerGroupAddClientMessage::new(&mut iter::once(
        c2s::OutServerGroupAddClientPart {
            server_group_id: tsproto_types::ServerGroupId(server_group_id),
            client_db_id: tsproto_types::ClientDbId(db_id),
        },
    ));

    msg.to_packet()
        .send(con)
        .context("Failed to send server group add command")?;

    info!(
        client = client_name,
        sgid = server_group_id,
        "Added server group to client"
    );
    Ok(())
}

/// Remove a server group from a client.
pub fn remove_server_group(
    con: &mut Connection,
    client_name: &str,
    server_group_id: u64,
) -> Result<()> {
    let (_client_id, db_id) = find_client_by_name(con, client_name)?;

    let msg = c2s::OutServerGroupDelClientMessage::new(&mut iter::once(
        c2s::OutServerGroupDelClientPart {
            server_group_id: tsproto_types::ServerGroupId(server_group_id),
            client_db_id: tsproto_types::ClientDbId(db_id),
        },
    ));

    msg.to_packet()
        .send(con)
        .context("Failed to send server group remove command")?;

    info!(
        client = client_name,
        sgid = server_group_id,
        "Removed server group from client"
    );
    Ok(())
}

/// Ban a client from the server.
pub fn ban_client(
    con: &mut Connection,
    client_name: &str,
    reason_msg: Option<&str>,
    duration_seconds: Option<u64>,
) -> Result<()> {
    let (client_id, _) = find_client_by_name(con, client_name)?;
    let reason_str = reason_msg.unwrap_or("Banned by AI Support Agent");
    let duration = duration_seconds.unwrap_or(0); // 0 = permanent

    let msg = c2s::OutBanClientMessage::new(&mut iter::once(c2s::OutBanClientPart {
        client_id: ClientId(client_id),
        time: Some(time::Duration::seconds(duration as i64)),
        ban_reason: Some(Cow::Borrowed(reason_str)),
    }));

    msg.to_packet()
        .send(con)
        .context("Failed to send ban command")?;

    info!(
        client = client_name,
        duration_secs = duration,
        "Banned client"
    );
    Ok(())
}

/// Send a private text message to a specific user by name.
pub fn send_message_to(con: &mut Connection, target_name: &str, message: &str) -> Result<()> {
    let (client_id, _) = find_client_by_name(con, target_name)?;

    use tsclientlib::messages::c2s::{OutSendTextMessageMessage, OutSendTextMessagePart};

    let msg = OutSendTextMessageMessage::new(&mut iter::once(OutSendTextMessagePart {
        target: tsproto_types::TextMessageTargetMode::Client,
        target_client_id: Some(ClientId(client_id)),
        message: Cow::Borrowed(message),
    }));

    msg.send(con).context("Failed to send private message")?;

    info!(target = target_name, "Sent private message to client");
    Ok(())
}

/// Send a message to a specific channel by name.
pub fn send_channel_message(con: &mut Connection, channel_name: &str, message: &str) -> Result<()> {
    let channel_id = crate::channels::find_channel_by_name(con, channel_name)?;

    // First move the bot to that channel so channel message goes there
    // Actually, TS3 channel messages go to the bot's current channel
    // We'll send as targetmode=2 which sends to the bot's current channel
    // If we want to send to a different channel, we'd need to move there first
    // For simplicity, we send to channel by targetmode=2 targeting the bot's channel
    use tsclientlib::messages::c2s::{OutSendTextMessageMessage, OutSendTextMessagePart};

    // Check if bot is in the target channel
    let state = con.get_state().context("Failed to get state")?;
    let own = state
        .clients
        .get(&state.own_client)
        .context("Bot not found")?;

    if own.channel.0 != channel_id {
        // We need to move to that channel first, send message, then move back
        let prev_channel_id = own.channel.0;
        // Move bot to target channel
        let move_msg = c2s::OutClientMoveMessage::new(&mut iter::once(c2s::OutClientMovePart {
            client_id: con
                .get_state()
                .ok()
                .map(|s| s.own_client)
                .unwrap_or(ClientId(0)),
            channel_id: ChannelId(channel_id),
            channel_password: None,
        }));
        move_msg
            .to_packet()
            .send(con)
            .context("Failed to move bot to target channel")?;

        // Send channel message
        let msg = OutSendTextMessageMessage::new(&mut iter::once(OutSendTextMessagePart {
            target: tsproto_types::TextMessageTargetMode::Channel,
            target_client_id: None,
            message: Cow::Borrowed(message),
        }));
        msg.send(con).context("Failed to send channel message")?;

        // Move bot back
        let move_back = c2s::OutClientMoveMessage::new(&mut iter::once(c2s::OutClientMovePart {
            client_id: con
                .get_state()
                .ok()
                .map(|s| s.own_client)
                .unwrap_or(ClientId(0)),
            channel_id: ChannelId(prev_channel_id),
            channel_password: None,
        }));
        move_back
            .to_packet()
            .send(con)
            .context("Failed to move bot back")?;
    } else {
        let msg = OutSendTextMessageMessage::new(&mut iter::once(OutSendTextMessagePart {
            target: tsproto_types::TextMessageTargetMode::Channel,
            target_client_id: None,
            message: Cow::Borrowed(message),
        }));
        msg.send(con).context("Failed to send channel message")?;
    }

    info!(channel = channel_name, "Sent channel message");
    Ok(())
}

/// Add a ban by IP, UID, or Name.
pub fn ban_add(
    con: &mut Connection,
    ip: Option<&str>,
    uid: Option<&str>,
    name: Option<&str>,
    reason: Option<&str>,
    duration_seconds: Option<u64>,
) -> Result<()> {
    use tsclientlib::OutCommandExt;
    use tsproto_packets::packets::{Direction, Flags, OutCommand, PacketType};

    let mut packet = OutCommand::new(
        Direction::C2S,
        Flags::empty(),
        PacketType::Command,
        "banadd",
    );

    if let Some(ip_str) = ip {
        packet.write_arg("ip", &ip_str);
    }
    if let Some(name_str) = name {
        packet.write_arg("name", &name_str);
    }
    if let Some(uid_str) = uid {
        packet.write_arg("uid", &uid_str);
    }
    if let Some(secs) = duration_seconds {
        packet.write_arg("time", &secs);
    }
    if let Some(reason_str) = reason {
        packet.write_arg("banreason", &reason_str);
    }

    packet.send(con).context("Failed to send ban add command")?;

    info!(
        ip = ip.unwrap_or("-"),
        uid = uid.unwrap_or("-"),
        name = name.unwrap_or("-"),
        "Added ban"
    );
    Ok(())
}

/// Remove a specific ban by its ID.
pub fn ban_del(con: &mut Connection, ban_id: u64) -> Result<()> {
    let msg = c2s::OutBanDelMessage::new(&mut iter::once(c2s::OutBanDelPart {
        ban_id: ban_id as u32,
    }));

    msg.to_packet()
        .send(con)
        .context("Failed to send ban delete command")?;

    info!(ban_id = ban_id, "Deleted ban");
    Ok(())
}

/// Remove all bans.
pub fn ban_del_all(con: &mut Connection) -> Result<()> {
    let msg = c2s::OutBanDelAllMessage::new();

    msg.send(con)
        .context("Failed to send ban delete all command")?;

    info!("Deleted all bans");
    Ok(())
}

/// Edit a client's properties (description, talker status).
pub fn client_edit(
    con: &mut Connection,
    client_name: &str,
    description: Option<&str>,
    is_talker: Option<bool>,
) -> Result<()> {
    let (client_id, _) = find_client_by_name(con, client_name)?;

    let msg = c2s::OutClientEditMessage::new(&mut iter::once(c2s::OutClientEditPart {
        client_id: ClientId(client_id),
        description: description.map(Cow::Borrowed),
        talk_power_granted: is_talker,
    }));

    msg.to_packet()
        .send(con)
        .context("Failed to send client edit command")?;

    info!(client = client_name, "Edited client properties");
    Ok(())
}
