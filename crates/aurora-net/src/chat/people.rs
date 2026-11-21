//! Text-only people data. Follows LLStartUp's buddy list, LLAvatarTracker's
//! presence notifications, LLAvatarNameCache::requestNamesViaCapability,
//! LLViewerRegion::updateCoarseLocations and FSRadar::updateRadarList
//! (indra/newview/); only avatar positions are kept from object traffic.

use super::*;
use crate::social::AvatarNameData;

pub const MAX_RESIDENTS: usize = 4096;
const NAME_BATCH: usize = 64;
const NAME_TTL: Duration = Duration::from_secs(600);
const RADAR_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentName {
    pub id: Uuid,
    pub display_name: String,
    pub legacy_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Friend {
    pub id: Uuid,
    /// False also covers presence hidden by the friend's privacy settings.
    pub online: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Nearby {
    pub id: Uuid,
    /// Three-dimensional distance, approximate when using coarse locations.
    pub distance: Option<f32>,
}

struct AvatarPosition {
    id: Uuid,
    position: Vec3,
    seen: Instant,
}

struct NameEntry {
    name: ResidentName,
    seen: Instant,
    requested: Option<Instant>,
}

#[derive(Default)]
pub(super) struct People {
    pub friends: HashMap<Uuid, bool>,
    names: HashMap<Uuid, NameEntry>,
    nearby: Vec<Nearby>,
    nearby_at: Option<Instant>,
    coarse: Vec<(Uuid, u8, u8, u8)>,
    avatars: HashMap<u32, AvatarPosition>,
    radar_dirty: bool,
    pub lookup_active: bool,
    pub warned: bool,
}

fn short(text: &str) -> String {
    text.chars().take(128).collect()
}

impl People {
    pub fn offline_friend(&self, id: Uuid) -> bool {
        self.friends.get(&id) == Some(&false)
    }

    pub fn name_batch(&mut self) -> Vec<Uuid> {
        let now = Instant::now();
        self.names
            .iter_mut()
            .filter(|(_, entry)| entry.requested.is_none_or(|at| now.duration_since(at) >= NAME_TTL))
            .take(NAME_BATCH)
            .map(|(id, entry)| {
                entry.requested = Some(now);
                *id
            })
            .collect()
    }
}

impl Session {
    pub(super) fn observe_name(&mut self, id: Uuid, fallback: &str) -> Result<(), Error> {
        if id.is_nil() {
            return Ok(());
        }
        let mut changed = false;
        if !self.people.names.contains_key(&id) {
            if self.people.names.len() == MAX_RESIDENTS
                && let Some(oldest) = self.people.names.iter().min_by_key(|(_, entry)| entry.seen).map(|(id, _)| *id)
            {
                self.people.names.remove(&oldest);
            }
            self.people.names.insert(
                id,
                NameEntry {
                    name: ResidentName {
                        id,
                        display_name: String::new(),
                        legacy_name: short(fallback),
                    },
                    seen: Instant::now(),
                    requested: None,
                },
            );
            changed = !fallback.is_empty();
        }
        let entry = self.people.names.get_mut(&id).expect("observed name");
        entry.seen = Instant::now();
        if entry.name.legacy_name.is_empty() && !fallback.is_empty() {
            entry.name.legacy_name = short(fallback);
            changed = true;
        }
        // Empty names are represented by UUIDs in the UI until resolved.
        if changed {
            let name = entry.name.clone();
            self.emit(Event::Names(vec![name]))?;
        }
        Ok(())
    }

    pub(super) fn legacy_name(&mut self, id: Uuid, legacy: &str) -> Result<(), Error> {
        if let Some(entry) = self.people.names.get_mut(&id) {
            entry.name.legacy_name = short(legacy.trim());
            let name = entry.name.clone();
            self.emit(Event::Names(vec![name]))?;
        }
        Ok(())
    }

    pub(super) fn display_name(&mut self, name: AvatarNameData) -> Result<(), Error> {
        if let Some(entry) = self.people.names.get_mut(&name.id) {
            entry.name.display_name = short(&name.display_name);
            let legacy = format!("{} {}", name.legacy_first, name.legacy_last);
            if !legacy.trim().is_empty() {
                entry.name.legacy_name = short(legacy.trim());
            } else if entry.name.legacy_name.is_empty() {
                entry.name.legacy_name = short(&name.username);
            }
            entry.requested = Some(Instant::now());
            let name = entry.name.clone();
            self.emit(Event::Names(vec![name]))?;
        }
        Ok(())
    }

    pub(super) fn load_friends(&mut self) -> Result<(), Error> {
        let ids: Vec<_> = self.login.raw["buddy-list"]
            .as_array()
            .iter()
            .map(|b| b["buddy_id"].as_uuid())
            .collect();
        for id in ids {
            if !id.is_nil() {
                if self.people.friends.len() == MAX_RESIDENTS && !self.people.friends.contains_key(&id) {
                    return Err(Error::Session(tr!("tui-resident-limit").into()));
                }
                self.people.friends.insert(id, false);
                self.observe_name(id, "")?;
            }
        }
        self.emit_friends()
    }

    pub(super) fn emit_friends(&self) -> Result<(), Error> {
        let mut friends: Vec<_> = self
            .people
            .friends
            .iter()
            .map(|(id, online)| Friend { id: *id, online: *online })
            .collect();
        friends.sort_by_key(|friend| friend.id);
        self.emit(Event::Friends(friends))
    }

    pub(super) fn friend_presence(&mut self, ids: impl Iterator<Item = Uuid>, online: bool) -> Result<(), Error> {
        let mut changed = false;
        for id in ids {
            if let Some(previous) = self.people.friends.get_mut(&id)
                && *previous != online
            {
                *previous = online;
                changed = true;
            }
        }
        if changed {
            self.emit_friends()?;
        }
        Ok(())
    }

    pub(super) fn legacy_lookup(&mut self, ids: &[Uuid]) {
        let mut message = UUIDNameRequest::default();
        for id in ids {
            message
                .uuid_name_block
                .push(aurora_msg::msgs::uuid_name_request::UUIDNameBlock { id: *id });
        }
        self.send(&message, true);
    }

    fn nearby_snapshot(&mut self, positions: Vec<(Uuid, u8, u8, u8)>) -> Result<(), Error> {
        self.people.coarse = positions;
        self.people.nearby_at = Some(Instant::now());
        self.publish_nearby()
    }

    fn publish_nearby(&mut self) -> Result<(), Error> {
        self.people.radar_dirty = false;
        let precise: HashMap<_, _> = self.people.avatars.values().map(|p| (p.id, p.position)).collect();
        let origin = precise
            .get(&self.login.agent_id)
            .copied()
            .or_else(|| {
                self.people
                    .coarse
                    .iter()
                    .find(|(id, ..)| *id == self.login.agent_id)
                    .and_then(|(_, x, y, z)| self.coarse_position(*x, *y, *z))
            })
            .or_else(|| self.arrived.then_some(self.position).filter(|p| p.is_finite()));
        let mut nearby = Vec::new();
        let mut seen = HashSet::new();
        for &(id, x, y, z) in &self.people.coarse {
            if !id.is_nil() && id != self.login.agent_id && seen.insert(id) {
                let position = precise.get(&id).copied().or_else(|| self.coarse_position(x, y, z));
                let distance = position.zip(origin).map(|(p, o)| p.distance(o)).filter(|d| d.is_finite());
                nearby.push(Nearby { id, distance });
            }
        }
        for (id, position) in precise {
            if id != self.login.agent_id && seen.insert(id) && nearby.len() < MAX_RESIDENTS {
                let distance = origin.map(|o| position.distance(o)).filter(|d| d.is_finite());
                nearby.push(Nearby { id, distance });
            }
        }
        nearby.sort_by(|a, b| {
            a.distance
                .unwrap_or(f32::INFINITY)
                .total_cmp(&b.distance.unwrap_or(f32::INFINITY))
                .then(a.id.cmp(&b.id))
        });
        if nearby != self.people.nearby {
            for entry in &nearby {
                self.observe_name(entry.id, "")?;
            }
            self.people.nearby = nearby.clone();
            self.emit(Event::Nearby(nearby))?;
        }
        Ok(())
    }

    fn coarse_position(&self, x: u8, y: u8, z: u8) -> Option<Vec3> {
        (!matches!(z, 0 | 255)).then(|| social::coarse_position(x, y, z, (self.login.region_size_x, self.login.region_size_y)))
    }

    pub(super) fn avatar_positions(&mut self, message: &ObjectUpdate) {
        if message.region_data.region_handle != self.login.region_handle() {
            return;
        }
        for block in &message.object_data {
            // LLViewerObject::processUpdateMessage: avatar (0x2f), optional
            // foot plane, then XYZ. Parent-relative poses need a world model.
            if block.p_code != 0x2f || block.parent_id != 0 || block.full_id.is_nil() {
                self.forget_avatar_position(block.id);
                continue;
            }
            let offset = match block.object_data.len() {
                60 | 124 => 0,
                76 | 140 => 16,
                _ => {
                    log::debug!("chat: unsupported full avatar motion block length");
                    self.forget_avatar_position(block.id);
                    continue;
                }
            };
            if let Some(position) = motion_position(&block.object_data[offset..]) {
                self.people
                    .avatars
                    .retain(|local_id, p| *local_id == block.id || p.id != block.full_id);
                if self.people.avatars.len() < MAX_RESIDENTS || self.people.avatars.contains_key(&block.id) {
                    self.people.avatars.insert(
                        block.id,
                        AvatarPosition {
                            id: block.full_id,
                            position,
                            seen: Instant::now(),
                        },
                    );
                    self.people.radar_dirty = true;
                }
            } else {
                log::debug!("chat: ignoring non-finite avatar position");
                self.forget_avatar_position(block.id);
            }
        }
    }

    pub(super) fn terse_positions(&mut self, message: &ImprovedTerseObjectUpdate) {
        if message.region_data.region_handle != self.login.region_handle() {
            return;
        }
        for block in &message.object_data {
            let data = &block.data;
            // Local ID, state, avatar flag, foot plane, XYZ.
            if data.get(5).is_some_and(|&avatar| avatar != 0)
                && let Some(id) = data.first_chunk::<4>().copied().map(u32::from_le_bytes)
                && let Some(entry) = self.people.avatars.get_mut(&id)
            {
                if let Some(position) = data.get(22..).and_then(motion_position) {
                    self.people.radar_dirty |= entry.position != position;
                    entry.position = position;
                    entry.seen = Instant::now();
                } else {
                    log::debug!("chat: ignoring invalid terse avatar position");
                }
            }
        }
    }

    pub(super) fn forget_avatar_position(&mut self, local_id: u32) {
        self.people.radar_dirty |= self.people.avatars.remove(&local_id).is_some();
    }

    pub(super) fn coarse_locations(&mut self, message: &CoarseLocationUpdate) -> Result<(), Error> {
        let positions = message
            .location
            .iter()
            .zip(&message.agent_data)
            .take(MAX_RESIDENTS)
            .map(|(p, agent)| (agent.agent_id, p.x, p.y, p.z))
            .collect();
        self.nearby_snapshot(positions)
    }

    pub(super) fn coarse_event(&mut self, body: &Llsd) -> Result<(), Error> {
        let positions = body["Location"]
            .as_array()
            .iter()
            .zip(body["AgentData"].as_array())
            .take(MAX_RESIDENTS)
            .filter_map(|(p, agent)| {
                match (
                    u8::try_from(p["X"].as_i32()),
                    u8::try_from(p["Y"].as_i32()),
                    u8::try_from(p["Z"].as_i32()),
                ) {
                    (Ok(x), Ok(y), Ok(z)) => Some((agent["AgentID"].as_uuid(), x, y, z)),
                    _ => {
                        log::warn!("chat: invalid coarse location coordinates");
                        None
                    }
                }
            })
            .collect();
        self.nearby_snapshot(positions)
    }

    pub(super) fn expire_nearby(&mut self) -> Result<(), Error> {
        let previous = self.people.avatars.len();
        self.people.avatars.retain(|_, entry| entry.seen.elapsed() < RADAR_TTL);
        self.people.radar_dirty |= self.people.avatars.len() != previous;
        if self.people.nearby_at.is_some_and(|at| at.elapsed() >= RADAR_TTL) {
            self.people.nearby_at = None;
            self.people.coarse.clear();
            self.people.radar_dirty = true;
        }
        if self.people.radar_dirty {
            self.publish_nearby()?;
        }
        Ok(())
    }
}

fn motion_position(data: &[u8]) -> Option<Vec3> {
    let data: &[u8; 12] = data.get(..12)?.try_into().ok()?;
    let (axes, _) = data.as_chunks::<4>();
    let position = Vec3::new(
        f32::from_le_bytes(axes[0]),
        f32::from_le_bytes(axes[1]),
        f32::from_le_bytes(axes[2]),
    );
    position.is_finite().then_some(position)
}

/// One bounded request at a time; batching follows LLAvatarNameCache's URL limit.
pub(super) async fn lookup_names(http: &reqwest::Client, cap: &str, ids: &[Uuid]) -> Result<Vec<AvatarNameData>, caps::CapsError> {
    let mut url = reqwest::Url::parse(&format!("{}/", cap.trim_end_matches('/')))
        .map_err(|_| caps::CapsError::Http("invalid display-name capability".into()))?;
    for id in ids {
        url.query_pairs_mut().append_pair("ids", &id.to_string());
    }
    let mut response = http
        .get(url)
        .header("Accept", "application/llsd+xml")
        .send()
        .await
        .map_err(|e| caps::CapsError::Http(e.without_url().to_string()))?;
    if !response.status().is_success() {
        return Err(caps::CapsError::Status(response.status().as_u16()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| caps::CapsError::Http(e.without_url().to_string()))?
    {
        if bytes.len() + chunk.len() > 1024 * 1024 {
            return Err(caps::CapsError::Http("display-name response too large".into()));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value = aurora_llsd::from_xml(&bytes)?;
    if !value.is_map() || !matches!(&value["agents"], Llsd::Array(_)) {
        return Err(caps::CapsError::Http("invalid display-name response".into()));
    }
    Ok(value["agents"]
        .as_array()
        .iter()
        .take(NAME_BATCH)
        .map(AvatarNameData::from_llsd)
        .filter(|name| ids.contains(&name.id))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::super::tests::{fixture, incoming};
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn chat_radar_recovers_four_metres_above_the_coarse_ceiling() {
        let (mut session, mut events, _) = fixture().await;
        let me = session.login.agent_id;
        let peer = Uuid::from_u128(42);
        session.nearby_snapshot(vec![(me, 10, 10, 255), (peer, 10, 10, 255)]).unwrap();
        assert!(matches!(events.try_recv(), Ok(Event::Nearby(rows)) if rows[0].distance.is_none()));

        let mut full = ObjectUpdate::default();
        full.region_data.region_handle = session.login.region_handle();
        for (id, avatar, z) in [(1, me, 1500.0_f32), (2, peer, 1504.0)] {
            let mut xyz = vec![0; 76];
            xyz[16..20].copy_from_slice(&10.0_f32.to_le_bytes());
            xyz[20..24].copy_from_slice(&10.0_f32.to_le_bytes());
            xyz[24..28].copy_from_slice(&z.to_le_bytes());
            full.object_data.push(object_update::ObjectData {
                id,
                full_id: avatar,
                p_code: 0x2f,
                object_data: xyz,
                ..Default::default()
            });
        }
        session.received(&incoming(&full)).unwrap();
        session.expire_nearby().unwrap();
        assert!(matches!(events.try_recv(), Ok(Event::Nearby(rows)) if rows == vec![Nearby { id: peer, distance: Some(4.0) }]));

        let mut terse = ImprovedTerseObjectUpdate::default();
        terse.region_data.region_handle = full.region_data.region_handle;
        let mut data = vec![0; 60];
        data[..4].copy_from_slice(&2_u32.to_le_bytes());
        data[5] = 1;
        data[22..30].copy_from_slice(&full.object_data[1].object_data[16..24]);
        data[30..34].copy_from_slice(&1505.0_f32.to_le_bytes());
        terse.object_data.push(improved_terse_object_update::ObjectData {
            data,
            texture_entry: Vec::new(),
        });
        session.received(&incoming(&terse)).unwrap();
        session.expire_nearby().unwrap();
        assert!(matches!(events.try_recv(), Ok(Event::Nearby(rows)) if rows[0].distance == Some(5.0)));

        let mut kill = KillObject::default();
        kill.object_data.push(kill_object::ObjectData { id: 2 });
        session.received(&incoming(&kill)).unwrap();
        session.expire_nearby().unwrap();
        assert!(matches!(events.try_recv(), Ok(Event::Nearby(rows)) if rows[0].distance.is_none()));
    }

    #[tokio::test]
    async fn chat_radar_scales_xyz_omits_self_deduplicates_and_expires() {
        let (mut session, mut events, _) = fixture().await;
        session.login.region_size_x = 512;
        session.login.region_size_y = 256;
        let me = session.login.agent_id;
        let peer = Uuid::from_u128(42);
        let high = Uuid::from_u128(43);
        session
            .nearby_snapshot(vec![(me, 10, 10, 10), (peer, 16, 10, 14), (high, 10, 10, 255), (peer, 16, 10, 14)])
            .unwrap();
        let snapshot = match events.try_recv().unwrap() {
            Event::Nearby(nearby) => nearby,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(
            snapshot,
            vec![
                Nearby {
                    id: peer,
                    distance: Some(20.0)
                },
                Nearby { id: high, distance: None }
            ]
        );
        session
            .nearby_snapshot(vec![(me, 10, 10, 10), (peer, 16, 10, 14), (high, 10, 10, 255)])
            .unwrap();
        assert!(events.try_recv().is_err());
        session.people.nearby_at = Some(Instant::now() - RADAR_TTL);
        session.expire_nearby().unwrap();
        assert!(matches!(events.try_recv(), Ok(Event::Nearby(nearby)) if nearby.is_empty()));
        session.expire_nearby().unwrap();
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn chat_radar_udp_and_capability_snapshots_agree_and_replace_departures() {
        let (mut session, mut events, _) = fixture().await;
        session.arrived = true;
        session.position = Vec3::new(0.0, 0.0, 40.0);
        let peer = Uuid::from_u128(42);
        let mut wire = CoarseLocationUpdate::default();
        wire.location.push(coarse_location_update::Location { x: 3, y: 4, z: 10 });
        wire.agent_data.push(coarse_location_update::AgentData { agent_id: peer });
        session.coarse_locations(&wire).unwrap();
        assert!(matches!(events.try_recv(), Ok(Event::Nearby(nearby)) if nearby == vec![Nearby { id: peer, distance: Some(5.0) }]));
        session
            .coarse_event(&llsd_map! {
                "Location" => vec![llsd_map! { "X" => 3, "Y" => 4, "Z" => 10 }],
                "AgentData" => vec![llsd_map! { "AgentID" => peer }],
            })
            .unwrap();
        assert!(events.try_recv().is_err());
        session.coarse_locations(&CoarseLocationUpdate::default()).unwrap();
        assert!(matches!(events.try_recv(), Ok(Event::Nearby(nearby)) if nearby.is_empty()));
    }

    #[tokio::test]
    async fn chat_name_cache_and_requests_are_bounded_and_late_legacy_keeps_display_name() {
        let (mut session, mut events, _) = fixture().await;
        let peer = Uuid::from_u128(42);
        session.observe_name(peer, "Example Resident").unwrap();
        events.try_recv().unwrap();
        session
            .display_name(AvatarNameData::from_llsd(&llsd_map! {
                "id" => peer, "display_name" => "桜", "legacy_first_name" => "Example", "legacy_last_name" => "Resident",
            }))
            .unwrap();
        events.try_recv().unwrap();
        session.legacy_name(peer, "Example Resident").unwrap();
        assert!(
            matches!(events.try_recv(), Ok(Event::Names(names)) if names[0].display_name == "桜" && names[0].legacy_name == "Example Resident")
        );
        session.observe_name(peer, "stale name").unwrap();
        assert!(events.try_recv().is_err(), "observing the same name must not flood UI events");
        for id in 100..100 + MAX_RESIDENTS as u128 + 1 {
            session.observe_name(Uuid::from_u128(id), "").unwrap();
        }
        assert_eq!(session.people.names.len(), MAX_RESIDENTS);
        let first = session.people.name_batch();
        let second = session.people.name_batch();
        assert_eq!(first.len(), NAME_BATCH);
        assert_eq!(second.len(), NAME_BATCH);
        assert!(first.iter().all(|id| !second.contains(id)));
        assert!(session.people.names.values().all(|entry| entry.name.legacy_name.len() <= 512));
    }

    async fn name_server(body: Vec<u8>) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut input = [0; 8192];
            let n = socket.read(&mut input).await.unwrap();
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
            // An oversized response may be refused before all of it is written.
            if socket.write_all(&body).await.is_err() {
                return String::from_utf8_lossy(&input[..n]).into_owned();
            }
            crate::test_http::finish(socket);
            String::from_utf8_lossy(&input[..n]).into_owned()
        });
        (url, task)
    }

    #[tokio::test]
    async fn chat_display_name_http_batches_only_requested_ids_and_bounds_the_body() {
        let peer = Uuid::from_u128(42);
        let stranger = Uuid::from_u128(99);
        let value = llsd_map! { "agents" => vec![
            llsd_map! { "id" => peer, "display_name" => "桜", "legacy_first_name" => "Example", "legacy_last_name" => "Resident" },
            llsd_map! { "id" => stranger, "display_name" => "unsolicited" },
        ] };
        let (url, server) = name_server(aurora_llsd::to_xml(&value)).await;
        let result = lookup_names(&crate::test_http_client(), &url, &[peer]).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, peer);
        assert_eq!(result[0].display_name, "桜");
        assert!(server.await.unwrap().starts_with(&format!("GET /?ids={peer} ")));
        let (url, server) = name_server(vec![b'x'; 1024 * 1024 + 1]).await;
        assert!(lookup_names(&crate::test_http_client(), &url, &[peer]).await.is_err());
        server.await.unwrap();
        let (url, server) = name_server(aurora_llsd::to_xml(&llsd_map! { "agents" => "not an array" })).await;
        assert!(lookup_names(&crate::test_http_client(), &url, &[peer]).await.is_err());
        server.await.unwrap();
    }
}
