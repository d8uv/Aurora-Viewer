//! Headless login attachment restoration using the viewer's inventory readers
//! and attachment queue. Only attachment identities and parentage are retained.

use super::*;
use crate::attach_queue::{AttachQueue, attach_item_id};
use crate::inventory::{self, InvItem};
use crate::outfit_restore::{self, restore_settled};

const MAX_OBJECTS: usize = 4096;
const MAX_OUTFIT_ITEMS: usize = 512;
const OBJECT_TIMEOUT: Duration = Duration::from_secs(30);
const RETRY_AFTER: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Object {
    full_id: Uuid,
    parent: u32,
    item: Option<Uuid>,
    full: bool,
    requested_at: Option<Instant>,
}

#[derive(Default)]
pub(super) struct Restore {
    pub cof: Option<Uuid>,
    pub first_login: bool,
    pub started: bool,
    pub fetching: bool,
    pub failed: bool,
    failure_reported: bool,
    pub done: bool,
    pub refresh_needed: bool,
    retry_ready: bool,
    ready_at: Option<Instant>,
    sent_at: Option<Instant>,
    retried: bool,
    self_local: Option<u32>,
    objects: HashMap<u32, Object>,
    items: Vec<InvItem>,
    worn: HashSet<Uuid>,
    last_arrival: Option<Instant>,
    queue: AttachQueue,
    sent_count: u32,
}

impl Restore {
    pub fn diagnostic_line(&self) -> String {
        let state = if self.failed {
            "failed"
        } else if self.done {
            "complete"
        } else if self.fetching {
            "fetching"
        } else if self.sent_at.is_some() {
            "restoring"
        } else {
            "waiting"
        };
        let worn = self.items.iter().filter(|item| self.worn.contains(&item.id)).count();
        format!(
            "outfit_restore={state} outfit_objects={} worn={} requests_sent={}",
            self.items.len(),
            worn,
            self.sent_count
        )
    }

    pub fn from_login(login: &LoginResponse) -> Self {
        let cof = inventory::parse_skeleton(&login.raw["inventory-skeleton"])
            .into_iter()
            .find(|folder| folder.type_default == 46)
            .map(|folder| folder.id);
        Self {
            cof,
            first_login: login.raw["login-flags"].at(0)["ever_logged_in"].as_str() == "N",
            ..Default::default()
        }
    }

    pub fn arrived(&mut self, now: Instant) {
        self.ready_at = Some(now);
    }

    pub fn alert(&mut self, message: &str) {
        self.queue.on_alert(message, Instant::now());
    }

    pub fn fetched(&mut self, result: Result<Vec<InvItem>, &'static str>) {
        self.fetching = false;
        match result {
            Ok(items) => {
                self.items = items;
                if self.refresh_needed {
                    self.refresh_needed = false;
                    self.retry_ready = true;
                }
            }
            Err(error) => {
                log::warn!("chat: attachment restore: {error}");
                self.failed = true;
            }
        }
    }

    fn observe(&mut self, local: u32, full_id: Uuid, pcode: u8, state: u8, parent: u32, names: Option<&str>, now: Instant) {
        if self.failed || self.done {
            return;
        }
        if pcode == 0x2f {
            if self.objects.remove(&local).is_some() {
                self.update_worn(now);
            }
            return;
        }
        if self.self_local.is_some_and(|own| parent != own) {
            if self.objects.remove(&local).is_some() {
                self.update_worn(now);
            }
            return;
        }
        if parent == 0 || state == 0 || full_id.is_nil() {
            if self.objects.remove(&local).is_some() {
                self.update_worn(now);
            }
            return;
        }
        if self.objects.len() >= MAX_OBJECTS && !self.objects.contains_key(&local) {
            self.failed = true;
            return;
        }
        let entry = self.objects.entry(local).or_default();
        if entry.full_id != full_id {
            *entry = Object {
                full_id,
                ..Default::default()
            };
        }
        entry.parent = parent;
        if let Some(item) = names.and_then(attach_item_id).filter(|id| !id.is_nil()) {
            entry.item = Some(item);
            entry.full = true;
            entry.requested_at = None;
        }
        if self.self_local == Some(parent) {
            if let Some(names) = names {
                self.queue.observe(std::iter::once(names), now);
            }
            self.update_worn(now);
        }
    }

    fn update_worn(&mut self, now: Instant) {
        let Some(own) = self.self_local else { return };
        let current: HashSet<_> = self.objects.values().filter(|o| o.parent == own).filter_map(|o| o.item).collect();
        if current.difference(&self.worn).next().is_some() || self.worn.difference(&current).next().is_some() {
            self.last_arrival = Some(now);
        }
        self.worn = current;
    }
}

pub(super) async fn fetch(
    http: &reqwest::Client,
    caps: &HashMap<String, String>,
    cof: Uuid,
    agent: Uuid,
) -> Result<Vec<InvItem>, &'static str> {
    let folders = if let Some(ais) = caps.get("InventoryAPIv3") {
        inventory::ais::fetch_folders(http, ais, agent, &[cof])
            .await
            .map_err(|_| "Current Outfit folder fetch failed")?
    } else {
        let cap = caps
            .get("FetchInventoryDescendents2")
            .ok_or("inventory folder capability unavailable")?;
        let reply = caps::post_llsd(http, cap, &inventory::fetch_request_body(&[cof], agent))
            .await
            .map_err(|_| "Current Outfit folder fetch failed")?;
        if reply.has("error") || !reply["folders"].is_array() {
            return Err("invalid Current Outfit folder reply");
        }
        inventory::parse_fetch_response(&reply)
    };
    let folder = folders
        .into_iter()
        .find(|f| f.folder_id == cof && f.owner_id == agent)
        .ok_or("Current Outfit folder missing from reply")?;
    if folder.items.len() > MAX_OUTFIT_ITEMS {
        return Err("Current Outfit folder exceeds the item limit");
    }
    let mut items: HashMap<_, _> = folder.items.into_iter().map(|item| (item.id, item)).collect();
    let mut links: Vec<_> = items.values().filter(|item| item.parent == cof).collect();
    links.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    let targets = outfit_restore::cof_object_targets(links, |id| items.get(id));
    let missing: Vec<_> = targets.iter().filter(|id| !items.contains_key(id)).copied().collect();
    if !missing.is_empty() {
        if let Some(ais) = caps.get("InventoryAPIv3") {
            let reads = inventory::ais::read_items(http, ais, agent, &missing).await;
            if !reads.failed.is_empty() {
                return Err("outfit item fetch failed");
            }
            if !reads.missing.is_empty() {
                log::warn!("chat: {} broken outfit item links", reads.missing.len());
            }
            items.extend(reads.items.into_iter().map(|item| (item.id, item)));
        } else {
            let cap = caps.get("FetchInventory2").ok_or("inventory item capability unavailable")?;
            for chunk in missing.chunks(50) {
                let reply = caps::post_llsd(http, cap, &inventory::fetch_items_body(chunk, agent))
                    .await
                    .map_err(|_| "outfit item fetch failed")?;
                let reads = inventory::item_reads(&reply, chunk);
                if !reads.failed.is_empty() {
                    return Err("invalid outfit item reply");
                }
                if !reads.missing.is_empty() {
                    log::warn!("chat: {} broken outfit item links", reads.missing.len());
                }
                items.extend(reads.items.into_iter().map(|item| (item.id, item)));
            }
        }
    }
    Ok(targets
        .into_iter()
        .filter_map(|id| items.remove(&id))
        .filter(|item| item.asset_type == 6 && !outfit_restore::coalesced(item))
        .collect())
}

impl Session {
    pub(super) fn outfit_full(&mut self, message: &ObjectUpdate) {
        if self.outfit.done || self.outfit.failed || message.region_data.region_handle != self.login.region_handle() {
            return;
        }

        let now = Instant::now();
        for b in &message.object_data {
            if b.full_id == self.login.agent_id && b.p_code == 0x2f {
                self.outfit.self_local = Some(b.id);
                self.outfit.objects.retain(|_, object| object.parent == b.id);
                self.outfit.update_worn(now);
            } else {
                if self.outfit.self_local.is_some_and(|own| b.parent_id != own) {
                    self.outfit.observe(b.id, b.full_id, b.p_code, b.state, b.parent_id, None, now);
                    continue;
                }
                let names = field_str(&b.name_value);
                self.outfit
                    .observe(b.id, b.full_id, b.p_code, b.state, b.parent_id, Some(&names), now);
            }
        }
    }

    pub(super) fn outfit_compressed(&mut self, message: &ObjectUpdateCompressed) {
        if self.outfit.done || self.outfit.failed || message.region_data.region_handle != self.login.region_handle() {
            return;
        }
        let now = Instant::now();
        for b in &message.object_data {
            let Some(h) = crate::object_header::compressed_header(&b.data) else {
                log::debug!("chat: ignoring invalid compressed object header");
                continue;
            };
            if h.full_id == self.login.agent_id && h.pcode == 0x2f {
                self.outfit.self_local = Some(h.local_id);
                self.outfit.objects.retain(|_, object| object.parent == h.local_id);
                self.outfit.update_worn(now);
            } else {
                if self.outfit.self_local.is_some_and(|own| h.parent_id != own) {
                    self.outfit.observe(h.local_id, h.full_id, h.pcode, h.state, h.parent_id, None, now);
                    continue;
                }
                let names = crate::object_header::compressed_name_values(&b.data, &h);
                self.outfit
                    .observe(h.local_id, h.full_id, h.pcode, h.state, h.parent_id, names, now);
            }
        }
    }

    pub(super) fn outfit_kill(&mut self, message: &KillObject) {
        for block in &message.object_data {
            self.outfit.objects.remove(&block.id);
            if self.outfit.self_local == Some(block.id) {
                self.outfit.self_local = None;
                self.outfit.worn.clear();
            }
        }
        self.outfit.update_worn(Instant::now());
    }

    pub(super) fn outfit_tick(&mut self, now: Instant) -> Result<(), Error> {
        if self.outfit.failed {
            if !self.outfit.failure_reported {
                self.outfit.failure_reported = true;
                log::warn!("chat: attachment restore stopped: incomplete object or inventory data");
                self.emit(Event::Notice(tr!("tui-outfit-restore-failed").into()))?;
            }
            return Ok(());
        }
        if !self.arrived || self.outfit.done || self.outfit.fetching || !self.outfit.started {
            return Ok(());
        }
        let ready_at = *self.outfit.ready_at.get_or_insert(now);
        if self.outfit.self_local.is_none() {
            if now.saturating_duration_since(ready_at) >= OBJECT_TIMEOUT {
                self.outfit.failed = true;
            }
            return Ok(());
        }
        let own = self.outfit.self_local;
        let unresolved = self
            .outfit
            .objects
            .values()
            .any(|object| Some(object.parent) == own && !object.full);
        let missing: Vec<u32> = self
            .outfit
            .objects
            .iter()
            .filter(|(_, object)| Some(object.parent) == own && !object.full && object.requested_at.is_none())
            .map(|(id, _)| *id)
            .take(255)
            .collect();
        if !missing.is_empty() && self.circuit.unacked_count() < MAX_PENDING - 8 {
            let mut request = RequestMultipleObjects::default();
            request.agent_data.agent_id = self.login.agent_id;
            request.agent_data.session_id = self.login.session_id;
            request.object_data = missing
                .iter()
                .map(|id| request_multiple_objects::ObjectData {
                    cache_miss_type: 0,
                    id: *id,
                })
                .collect();
            for id in missing {
                if let Some(object) = self.outfit.objects.get_mut(&id) {
                    object.requested_at = Some(now);
                }
            }
            self.send(&request, true);
        }
        if unresolved {
            if self.outfit.objects.values().any(|object| {
                Some(object.parent) == own
                    && object
                        .requested_at
                        .is_some_and(|at| now.saturating_duration_since(at) >= OBJECT_TIMEOUT)
            }) {
                self.outfit.failed = true;
            }
            return Ok(());
        }
        if self.outfit.sent_at.is_none() && restore_settled(now, ready_at, self.outfit.last_arrival) {
            let requests = self
                .outfit
                .items
                .iter()
                .filter(|item| !self.outfit.worn.contains(&item.id))
                .map(|item| outfit_restore::attach_request(item, self.login.agent_id))
                .collect();
            self.outfit.queue.push(requests, now);
            self.outfit.sent_at = Some(now);
        }
        if let Some(at) = self.outfit.sent_at
            && self.outfit.queue.len() == 0
            && now.saturating_duration_since(at) >= RETRY_AFTER
        {
            let missing: Vec<_> = self
                .outfit
                .items
                .iter()
                .filter(|item| !self.outfit.worn.contains(&item.id))
                .map(|item| outfit_restore::attach_request(item, self.login.agent_id))
                .collect();
            if missing.is_empty() {
                self.outfit.done = true;
            } else if self.outfit.retried {
                self.outfit.failed = true;
            } else if !self.outfit.retry_ready {
                self.outfit.refresh_needed = true;
                return Ok(());
            } else {
                self.outfit.queue.push(missing, now);
                self.outfit.retried = true;
                self.outfit.retry_ready = false;
                self.outfit.sent_at = Some(now);
            }
        }
        if self.circuit.unacked_count() < MAX_PENDING - 8
            && let Some(request) = self.outfit.queue.next(now)
            && !self.outfit.worn.contains(&request.item_id)
            && self.outfit.items.iter().any(|item| item.id == request.item_id)
        {
            let message = outfit_restore::attachment_message(request, self.login.agent_id, self.login.session_id);
            self.send(&message, true);
            self.outfit.sent_count = self.outfit.sent_count.saturating_add(1);
            self.outfit.sent_at = Some(now);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::tests::fixture;
    use aurora_msg::msgs::object_update;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn item(id: u128) -> InvItem {
        InvItem {
            id: Uuid::from_u128(id),
            asset_type: 6,
            owner: Uuid::from_u128(10),
            name: format!("item {id}"),
            ..Default::default()
        }
    }

    async fn inventory_server(replies: Vec<Llsd>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for reply in replies {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut data = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert_ne!(count, 0);
                    data.extend_from_slice(&buffer[..count]);
                    if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&data[..end]);
                        let length: usize = header
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().unwrap())
                            })
                            .unwrap_or(0);
                        if data.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8_lossy(&data).into_owned());
                let body = aurora_llsd::to_xml(&reply);
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/llsd+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                socket.write_all(&body).await.unwrap();
                crate::test_http::finish(socket);
            }
            requests
        });
        (base, server)
    }

    fn avatar_and_worn(agent: Uuid, item: Uuid) -> ObjectUpdate {
        ObjectUpdate {
            object_data: vec![
                object_update::ObjectData {
                    id: 2,
                    full_id: Uuid::from_u128(102),
                    p_code: 9,
                    parent_id: 1,
                    state: 0x20,
                    name_value: str_field(&format!("AttachItemID STRING RW SV {item}")),
                    ..Default::default()
                },
                object_update::ObjectData {
                    id: 1,
                    full_id: agent,
                    p_code: 0x2f,
                    ..Default::default()
                },
                object_update::ObjectData {
                    id: 3,
                    full_id: Uuid::from_u128(103),
                    p_code: 9,
                    parent_id: 99,
                    state: 0x20,
                    name_value: str_field(&format!("AttachItemID STRING RW SV {}", Uuid::from_u128(201))),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn restore_requests_only_missing_items_and_adds_at_the_saved_point() {
        let (mut session, _, simulator) = fixture().await;
        let now = Instant::now();
        session.arrived = true;
        session.outfit.started = true;
        session.outfit.arrived(now);
        session.outfit.fetched(Ok(vec![item(200), item(201)]));
        session.outfit_full(&avatar_and_worn(session.login.agent_id, Uuid::from_u128(200)));
        session.outfit_tick(now + Duration::from_secs(3)).unwrap();
        assert!(session.outfit.sent_at.is_none());
        session.outfit_tick(now + Duration::from_secs(5)).unwrap();
        let mut bytes = [0; 65536];
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), simulator.recv_from(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        let message = parse_packet(&bytes[..n])
            .unwrap()
            .decode::<RezMultipleAttachmentsFromInv>()
            .unwrap();
        assert!(!message.header_data.first_detach_all);
        assert_eq!(message.object_data.len(), 1);
        assert_eq!(message.object_data[0].item_id, Uuid::from_u128(201));
        assert_eq!(message.object_data[0].attachment_pt, 0x80);
        assert_eq!(message.object_data[0].owner_id, session.login.agent_id);
    }

    #[tokio::test]
    async fn retry_refreshes_outfit_and_does_not_rewear_removed_items() {
        let (mut session, _, _) = fixture().await;
        let now = Instant::now();
        session.arrived = true;
        session.outfit.started = true;
        session.outfit.arrived(now);
        session.outfit.fetched(Ok(vec![item(200), item(201)]));
        session.outfit_full(&avatar_and_worn(session.login.agent_id, Uuid::from_u128(200)));
        session.outfit_tick(now + Duration::from_secs(5)).unwrap();
        session.outfit_tick(now + Duration::from_secs(36)).unwrap();
        assert!(session.outfit.refresh_needed);
        session.outfit.fetched(Ok(vec![item(200)]));
        session.outfit_tick(now + Duration::from_secs(37)).unwrap();
        assert!(session.outfit.done);
        assert!(!session.outfit.retried);
    }

    #[tokio::test]
    async fn large_outfit_finishes_sending_before_retry_timeout() {
        let (mut session, _, _) = fixture().await;
        let now = Instant::now();
        session.arrived = true;
        session.outfit.started = true;
        session.outfit.arrived(now);
        session.outfit.fetched(Ok((200..240).map(item).collect()));
        session.outfit_full(&avatar_and_worn(session.login.agent_id, Uuid::from_u128(200)));
        for seconds in 5..=46 {
            for tenth in 0..10 {
                session
                    .outfit_tick(now + Duration::from_secs(seconds) + Duration::from_millis(tenth * 100))
                    .unwrap();
            }
        }
        assert!(session.outfit.sent_count > 24);
        assert!(!session.outfit.failed);
        assert!(!session.outfit.refresh_needed);
    }

    #[tokio::test]
    async fn missing_avatar_stops_restore_with_notice() {
        let (mut session, mut events, _) = fixture().await;
        let now = Instant::now();
        session.arrived = true;
        session.outfit.started = true;
        session.outfit.arrived(now);
        session.outfit.fetched(Ok(vec![item(201)]));
        session.outfit_tick(now + OBJECT_TIMEOUT).unwrap();
        session.outfit_tick(now + OBJECT_TIMEOUT).unwrap();
        assert!(session.outfit.failed);
        assert!(matches!(events.try_recv(), Ok(Event::Notice(_))));
        assert!(session.outfit.sent_at.is_none());
    }

    #[tokio::test]
    async fn unidentified_attachment_is_requested_and_never_guessed() {
        let (mut session, mut events, simulator) = fixture().await;
        let now = Instant::now();
        session.arrived = true;
        session.outfit.started = true;
        session.outfit.arrived(now);
        session.outfit.fetched(Ok(vec![item(201)]));
        let mut update = avatar_and_worn(session.login.agent_id, Uuid::from_u128(200));
        update.object_data[0].name_value.clear();
        session.outfit_full(&update);
        session.outfit_tick(now + Duration::from_secs(5)).unwrap();
        let mut bytes = [0; 65536];
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), simulator.recv_from(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        let request = parse_packet(&bytes[..n]).unwrap().decode::<RequestMultipleObjects>().unwrap();
        assert_eq!(request.object_data[0].id, 2);
        assert!(session.outfit.sent_at.is_none());
        session.outfit_tick(now + Duration::from_secs(36)).unwrap();
        session.outfit_tick(now + Duration::from_secs(36)).unwrap();
        assert!(session.outfit.failed);
        assert!(matches!(events.try_recv(), Ok(Event::Notice(_))));
        assert!(session.outfit.sent_at.is_none());
    }

    #[test]
    fn replacing_an_object_local_id_discards_the_old_item() {
        let mut state = Restore {
            self_local: Some(1),
            ..Default::default()
        };
        let now = Instant::now();
        let old_item = Uuid::from_u128(200);
        let names = format!("AttachItemID STRING RW SV {old_item}");
        state.observe(2, Uuid::from_u128(100), 9, 0x20, 1, Some(&names), now);
        assert!(state.worn.contains(&old_item));
        state.observe(2, Uuid::from_u128(101), 9, 0x20, 1, None, now);
        assert!(!state.worn.contains(&old_item));
        assert!(!state.objects[&2].full);
    }

    #[test]
    fn compressed_attachment_metadata_is_parsed_without_geometry() {
        let mut data = vec![0u8; 84];
        data[0..16].copy_from_slice(Uuid::from_u128(100).as_bytes());
        data[16..20].copy_from_slice(&2u32.to_le_bytes());
        data[20] = 9;
        data[21] = 0x20;
        data[64..68].copy_from_slice(&(0x20u32 | 0x100).to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.push(0);
        data.extend_from_slice(format!("AttachItemID STRING RW SV {}\0", Uuid::from_u128(200)).as_bytes());
        let header = crate::object_header::compressed_header(&data).unwrap();
        assert_eq!(header.parent_id, 1);
        let names = crate::object_header::compressed_name_values(&data, &header).unwrap();
        assert_eq!(attach_item_id(names), Some(Uuid::from_u128(200)));
        assert!(crate::object_header::compressed_name_values(&data[..data.len() - 1], &header).is_none());
        assert!(crate::object_header::compressed_header(&data[..83]).is_none());
    }

    #[tokio::test]
    async fn legacy_inventory_fetch_resolves_only_object_links() {
        let agent = Uuid::from_u128(10);
        let cof = Uuid::from_u128(11);
        let object = Uuid::from_u128(201);
        let link = llsd_map! {
            "item_id" => Uuid::from_u128(101), "parent_id" => cof,
            "type" => "link", "inv_type" => "object", "asset_id" => object
        };
        let non_object = llsd_map! {
            "item_id" => Uuid::from_u128(102), "parent_id" => cof,
            "type" => "link", "inv_type" => "wearable", "asset_id" => Uuid::from_u128(202)
        };
        let folder = llsd_map! {
            "folder_id" => cof, "owner_id" => agent, "version" => 1,
            "items" => Llsd::Array(vec![link, non_object])
        };
        let original = llsd_map! {
            "item_id" => object, "parent_id" => Uuid::from_u128(12),
            "name" => "HUD", "type" => "object", "inv_type" => "object",
            "permissions" => llsd_map! { "owner_id" => agent }
        };
        let replies = vec![
            llsd_map! { "folders" => Llsd::Array(vec![folder]) },
            llsd_map! { "items" => Llsd::Array(vec![original]) },
        ];
        let (base, server) = inventory_server(replies).await;
        let caps = HashMap::from([
            ("FetchInventoryDescendents2".into(), format!("{base}/folder")),
            ("FetchInventory2".into(), format!("{base}/items")),
        ]);
        let items = fetch(&crate::test_http_client(), &caps, cof, agent).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, object);
        let requests = server.await.unwrap();
        assert!(requests[0].starts_with("POST /folder "));
        assert!(requests[1].starts_with("POST /items "));
        assert!(requests[1].contains(&object.to_string()));
        assert!(!requests[1].contains(&Uuid::from_u128(202).to_string()));
    }

    #[tokio::test]
    async fn ais_inventory_fetch_resolves_outfit_links() {
        let agent = Uuid::from_u128(10);
        let cof = Uuid::from_u128(11);
        let object = Uuid::from_u128(201);
        let link_id = Uuid::from_u128(101);
        let link = llsd_map! {
            "item_id" => link_id, "parent_id" => cof, "name" => "HUD link",
            "type" => "link", "inv_type" => "object", "linked_id" => object
        };
        let folder = llsd_map! {
            "category_id" => cof, "parent_id" => Uuid::from_u128(12),
            "type_default" => 46, "name" => "Current Outfit", "version" => 1,
            "_embedded" => llsd_map! { "links" => llsd_map! { link_id.to_string() => link } }
        };
        let original = llsd_map! {
            "item_id" => object, "parent_id" => Uuid::from_u128(12),
            "name" => "HUD", "type" => "object", "inv_type" => "object",
            "permissions" => llsd_map! { "owner_id" => agent }
        };
        let (base, server) = inventory_server(vec![folder, original]).await;
        let caps = HashMap::from([("InventoryAPIv3".into(), base)]);
        let items = fetch(&crate::test_http_client(), &caps, cof, agent).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, object);
        let requests = server.await.unwrap();
        assert!(requests[0].starts_with(&format!("GET /category/{cof}/children?depth=0 ")));
        assert!(requests[1].starts_with(&format!("GET /item/{object} ")));
    }
}
