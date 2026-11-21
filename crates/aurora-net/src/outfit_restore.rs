//! Shared attachment requests and login restore timing.

use crate::inventory::InvItem;
use aurora_msg::msgs::{RezMultipleAttachmentsFromInv, rez_multiple_attachments_from_inv};
use aurora_msg::str_field;
use std::time::{Duration, Instant};
use uuid::Uuid;

pub const SETTLE: Duration = Duration::from_secs(4);
pub const QUIET: Duration = Duration::from_secs(4);
pub const NO_RESTORE: Duration = Duration::from_secs(12);
pub const MAX_SETTLE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct AttachRequest {
    pub item_id: Uuid,
    pub owner_id: Uuid,
    pub point: u8,
    pub add: bool,
    pub flags: u32,
    pub group_mask: u32,
    pub everyone_mask: u32,
    pub next_owner_mask: u32,
    pub name: String,
    pub desc: String,
}

/// Restore an item at its saved point without replacing its neighbours.
pub fn attach_request(item: &InvItem, agent: Uuid) -> AttachRequest {
    AttachRequest {
        item_id: item.id,
        owner_id: if item.owner.is_nil() { agent } else { item.owner },
        point: 0,
        add: true,
        flags: item.flags,
        group_mask: item.group_mask,
        everyone_mask: item.everyone_mask,
        next_owner_mask: item.next_owner_mask,
        name: item.name.clone(),
        desc: item.desc.clone(),
    }
}

pub fn coalesced(item: &InvItem) -> bool {
    item.asset_type == 6 && item.flags & 0x200000 != 0
}

pub fn cof_object_targets<'a>(
    links: impl IntoIterator<Item = &'a InvItem>,
    mut lookup: impl FnMut(&Uuid) -> Option<&'a InvItem>,
) -> Vec<Uuid> {
    let mut out = Vec::new();
    for link in links {
        if link.asset_type != 24 || link.asset_id.is_nil() {
            continue;
        }
        let is_object = match lookup(&link.asset_id) {
            Some(target) => target.asset_type == 6 && !coalesced(target),
            None => matches!(link.inv_type, 6 | 17),
        };
        if is_object && !out.contains(&link.asset_id) {
            out.push(link.asset_id);
        }
    }
    out
}

pub fn restore_settled(now: Instant, ready_at: Instant, last_arrival: Option<Instant>) -> bool {
    let waited = now.saturating_duration_since(ready_at);
    if waited < SETTLE {
        return false;
    }
    if waited >= MAX_SETTLE {
        return true;
    }
    match last_arrival {
        Some(at) => now.saturating_duration_since(at) >= QUIET,
        None => waited >= NO_RESTORE,
    }
}

pub(crate) fn attachment_message(a: AttachRequest, agent: Uuid, session: Uuid) -> RezMultipleAttachmentsFromInv {
    let mut m = RezMultipleAttachmentsFromInv::default();
    m.agent_data.agent_id = agent;
    m.agent_data.session_id = session;
    m.header_data.compound_msg_id = Uuid::new_v4();
    m.header_data.total_objects = 1;
    m.header_data.first_detach_all = false;
    m.object_data = vec![rez_multiple_attachments_from_inv::ObjectData {
        item_id: a.item_id,
        owner_id: a.owner_id,
        attachment_pt: a.point | if a.add { 0x80 } else { 0 },
        item_flags: a.flags,
        group_mask: a.group_mask,
        everyone_mask: a.everyone_mask,
        next_owner_mask: a.next_owner_mask,
        name: str_field(&a.name),
        description: str_field(&a.desc),
    }];
    m
}
