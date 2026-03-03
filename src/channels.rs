use std::borrow::Cow;
use std::iter;

use anyhow::{bail, Context, Result};
use tracing::{info, warn};

use tsclientlib::messages::c2s;
use tsclientlib::messages::OutMessageTrait;
use tsclientlib::{ChannelGroupId, ChannelId, ClientDbId, Connection, OutCommandExt};

/// Create a new channel on the TeamSpeak server.
///
/// # Arguments
/// * `con` — The active TS3 connection
/// * `name` — Channel name (1–100 characters)
/// * `password` — Optional password for the channel
/// * `permanent` — If `true`, create as permanent; otherwise semi-permanent
/// * `order_anchor_channel_id` — Optional channel ID to place new channel above
pub fn create_channel(
    con: &mut Connection,
    name: &str,
    password: Option<&str>,
    permanent: bool,
    order_anchor_channel_id: Option<u64>,
) -> Result<()> {
    // Validate inputs
    // Example issue: model may create "john_doe_room" instead of "john doe room".
    // TS3 allows spaces in channel names. If the AI hallucinates underscores, we let it be,
    // but the system prompt is adjusted to prevent it. We don't modify the name here to preserve intent.
    if name.is_empty() || name.len() > 100 {
        bail!("Channel name must be between 1 and 100 characters");
    }

    info!(
        channel = name,
        permanent,
        has_password = password.is_some(),
        "Creating channel"
    );

    // Determine order: optionally place above a configured anchor channel.
    let mut order = None;
    if let (Some(anchor_id), Ok(state)) = (order_anchor_channel_id, con.get_state()) {
        if let Some(anchor_channel) = state.channels.get(&ChannelId(anchor_id)) {
            // Setting our order to the same as the target channel places us above it.
            order = Some(anchor_channel.order);
            info!(
                channel = name,
                above_id = anchor_id,
                "Setting channel order to place above configured anchor"
            );
        }
    }

    let msg = c2s::OutChannelCreateMessage::new(&mut iter::once(c2s::OutChannelCreatePart {
        name: Cow::Borrowed(name),
        is_permanent: if permanent { Some(true) } else { None },
        is_semi_permanent: if !permanent { Some(true) } else { None },
        codec: Some(tsclientlib::Codec::OpusVoice),
        codec_quality: Some(7),
        parent_id: None,
        topic: None,
        description: None,
        password: password.map(Cow::Borrowed),
        max_clients: None,
        max_family_clients: None,
        order,
        has_password: password.map(|_| true),
        is_unencrypted: None,
        delete_delay: None,
        is_max_clients_unlimited: Some(true),
        is_max_family_clients_unlimited: Some(true),
        inherits_max_family_clients: None,
        phonetic_name: None,
        is_default: None,
    }));

    msg.to_packet()
        .send(con)
        .context("Failed to send channel create command")?;

    info!(channel = name, "Channel creation command sent");
    Ok(())
}

/// Set the channel group for a user in a specific channel.
/// Usually used to grant Channel Admin to the creator.
pub fn set_channel_admin(
    con: &mut Connection,
    channel_name: &str,
    client_db_id: u64,
    channel_admin_group_id: u64,
) -> Result<()> {
    let channel_id = find_channel_by_name(con, channel_name)?;

    info!(
        channel = channel_name,
        client_db_id,
        group = channel_admin_group_id,
        "Setting channel group"
    );

    let msg = c2s::OutSetClientChannelGroupMessage::new(&mut iter::once(
        c2s::OutSetClientChannelGroupPart {
            channel_group: ChannelGroupId(channel_admin_group_id),
            channel_id: ChannelId(channel_id),
            client_db_id: ClientDbId(client_db_id),
        },
    ));

    msg.to_packet()
        .send(con)
        .context("Failed to send set channel group command")?;

    Ok(())
}

/// Edit an existing channel, specifically to change its permanence.
///
/// # Arguments
/// * `con` — The active TS3 connection
/// * `channel_name` — Name of the channel to modify
/// * `set_permanent` — If `true`, make the channel permanent
pub fn edit_channel_permanent(
    con: &mut Connection,
    channel_name: &str,
    set_permanent: bool,
) -> Result<()> {
    info!(
        channel = channel_name,
        set_permanent, "Editing channel permanence"
    );

    // Find the channel by name
    let channel_id = find_channel_by_name(con, channel_name)?;

    let msg = c2s::OutChannelEditMessage::new(&mut iter::once(c2s::OutChannelEditPart {
        channel_id: ChannelId(channel_id),
        is_permanent: Some(set_permanent),
        is_semi_permanent: Some(!set_permanent),
        // Leave everything else unchanged
        order: None,
        name: None,
        topic: None,
        is_default: None,
        has_password: None,
        password: None,
        codec: None,
        codec_quality: None,
        needed_talk_power: None,
        max_clients: None,
        max_family_clients: None,
        codec_latency_factor: None,
        is_unencrypted: None,
        delete_delay: None,
        is_max_clients_unlimited: None,
        is_max_family_clients_unlimited: None,
        inherits_max_family_clients: None,
        phonetic_name: None,
        description: None,
    }));

    msg.to_packet()
        .send(con)
        .context("Failed to send channel edit command")?;

    info!(channel = channel_name, "Channel edit command sent");
    Ok(())
}

/// Edit an existing channel's description.
///
/// # Arguments
/// * `con` — The active TS3 connection
/// * `channel_id` — ID of the channel to modify
/// * `description` — The new text description
pub fn edit_channel_description(
    con: &mut Connection,
    channel_id: u64,
    description: &str,
) -> Result<()> {
    info!(channel_id, "Editing channel description");

    let msg = c2s::OutChannelEditMessage::new(&mut iter::once(c2s::OutChannelEditPart {
        channel_id: ChannelId(channel_id),
        description: Some(Cow::Borrowed(description)),
        // Leave everything else unchanged
        order: None,
        name: None,
        topic: None,
        is_default: None,
        has_password: None,
        password: None,
        codec: None,
        codec_quality: None,
        needed_talk_power: None,
        max_clients: None,
        max_family_clients: None,
        codec_latency_factor: None,
        is_unencrypted: None,
        delete_delay: None,
        is_max_clients_unlimited: None,
        is_max_family_clients_unlimited: None,
        inherits_max_family_clients: None,
        phonetic_name: None,
        is_permanent: None,
        is_semi_permanent: None,
    }));

    msg.to_packet()
        .send(con)
        .context("Failed to send channel edit description command")?;

    info!(channel_id, "Channel edit description command sent");
    Ok(())
}

/// Delete an existing channel.
pub fn delete_channel(con: &mut Connection, name: &str) -> Result<()> {
    let channel_id = find_channel_by_name(con, name)?;

    // Force delete (1) to delete even if it has clients inside
    let msg = c2s::OutChannelDeleteMessage::new(&mut iter::once(c2s::OutChannelDeletePart {
        channel_id: ChannelId(channel_id),
        force: true,
    }));

    msg.to_packet()
        .send(con)
        .context("Failed to send channel delete command")?;

    info!(channel = name, "Channel deleted");
    Ok(())
}

/// Find a channel by its name in the current server state.
/// Returns the channel ID (as u64) or an error if not found.
pub fn find_channel_by_name(con: &Connection, name: &str) -> Result<u64> {
    let state = con
        .get_state()
        .context("Failed to get connection state for channel lookup")?;

    let name_lower = name.to_lowercase();

    // 1. Try exact match
    for (channel_id, channel) in &state.channels {
        if channel.name.to_lowercase() == name_lower {
            return Ok(channel_id.0);
        }
    }

    // 2. Try substring matching (fuzzy)
    let mut candidates = Vec::new();
    for (channel_id, channel) in &state.channels {
        let channel_name_lower = channel.name.to_lowercase();
        if let Some(pos) = channel_name_lower.find(&name_lower) {
            candidates.push((channel_id.0, channel.name.clone(), pos, channel.name.len()));
        }
    }

    if !candidates.is_empty() {
        // Prioritize match appearing earliest in the name, then shortest channel name
        candidates.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.3.cmp(&b.3)));
        let best_match = &candidates[0];
        info!(
            target = name,
            matched = %best_match.1,
            "Fuzzy matched channel"
        );
        return Ok(best_match.0);
    }

    warn!(channel = name, "Channel not found even with fuzzy matching");
    bail!(
        "Could not find a channel named '{}'. Please check the exact name and try again.",
        name
    )
}

/// Move a channel to a new parent channel.
pub fn channel_move(con: &mut Connection, channel_name: &str, parent_name: &str) -> Result<()> {
    let channel_id = find_channel_by_name(con, channel_name)?;
    let parent_id = find_channel_by_name(con, parent_name)?;

    let msg = c2s::OutChannelMoveMessage::new(&mut iter::once(c2s::OutChannelMovePart {
        channel_id: ChannelId(channel_id),
        parent_id: ChannelId(parent_id),
        order: None,
    }));

    msg.to_packet()
        .send(con)
        .context("Failed to move channel")?;

    info!(
        channel = channel_name,
        parent = parent_name,
        "Moved channel"
    );
    Ok(())
}

/// Subscribe to a channel.
pub fn channel_subscribe(con: &mut Connection, channel_name: &str) -> Result<()> {
    let channel_id = find_channel_by_name(con, channel_name)?;

    let msg = c2s::OutChannelSubscribeMessage::new(&mut iter::once(c2s::OutChannelSubscribePart {
        channel_id: ChannelId(channel_id),
    }));

    msg.to_packet()
        .send(con)
        .context("Failed to subscribe to channel")?;

    info!(channel = channel_name, "Subscribed to channel");
    Ok(())
}

/// Unsubscribe from a channel.
pub fn channel_unsubscribe(con: &mut Connection, channel_name: &str) -> Result<()> {
    let channel_id = find_channel_by_name(con, channel_name)?;

    let msg =
        c2s::OutChannelUnsubscribeMessage::new(&mut iter::once(c2s::OutChannelUnsubscribePart {
            channel_id: ChannelId(channel_id),
        }));

    msg.to_packet()
        .send(con)
        .context("Failed to unsubscribe from channel")?;

    info!(channel = channel_name, "Unsubscribed from channel");
    Ok(())
}
