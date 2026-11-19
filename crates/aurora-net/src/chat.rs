//! Stationary, chat-only simulator session. Follows LLStartUp's circuit
//! handshake (llstartup.cpp), LLIMModel::sendStartSession (llimview.cpp)
//! and really_send_chat_from_viewer (fsnearbychathub.cpp).
//!
//! No world model, asset fetcher, inventory, appearance, neighbours or media.
//! Task traffic cannot be disabled: the simulator also sends chat on it.

use crate::caps::{self, EqMessage};
use crate::circuit::{Circuit, Resend};
use crate::circuit_socket::CircuitSocket;
use crate::login::{self, LoginError};
use crate::social::{self, GroupMembership};
use crate::{LoginRequest, LoginResponse, NetStats};
use aurora_i18n::tr;
use aurora_llsd::{Llsd, llsd_map};
use aurora_msg::msgs::*;
use aurora_msg::{IncomingPacket, Msg, field_str, parse_packet, str_field};
use glam::{Quat, Vec3};
use std::collections::{HashMap, HashSet};
use std::net::{SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use uuid::Uuid;

pub const MAX_GROUPS: usize = 128;
pub const MAX_SESSIONS: usize = 64;
pub const MAX_INPUT_BYTES: usize = 4096;
const MAX_PENDING: usize = 128;
const CHAT_CAPS: &[&str] = &["EventQueueGet", "ChatSessionRequest"];
const CHAT_OPTIONS: &[&str] = &["login-flags", "max-agent-groups", "adult_compliant"];

#[derive(Debug)]
pub enum Command {
    Local { text: String, kind: u8 },
    Join(Uuid),
    Leave(Uuid),
    Group { id: Uuid, text: String },
    Logout,
}

#[derive(Debug, Clone)]
pub enum Event {
    Connected(String),
    Groups(Vec<GroupMembership>),
    GroupRemoved(Uuid),
    Local { from: String, text: String },
    Group { id: Uuid, from: String, text: String },
    Joined(Uuid),
    Left(Uuid),
    Notice(String),
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Login(#[from] LoginError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Setup(#[from] crate::NetError),
    #[error("{0}")]
    Session(String),
}

struct Session {
    login: LoginResponse,
    socket: CircuitSocket,
    circuit: Circuit,
    stats: Arc<NetStats>,
    events: mpsc::Sender<Event>,
    groups: HashMap<Uuid, GroupMembership>,
    joined: HashSet<Uuid>,
    joining: HashMap<Uuid, Instant>,
    left: HashSet<Uuid>,
    muted: HashSet<Uuid>,
    sent: HashMap<u32, (Uuid, String)>,
    position: Vec3,
    arrived: bool,
    logged_out: bool,
}

impl Session {
    fn emit(&self, event: Event) -> Result<(), Error> {
        self.events.try_send(event).map_err(|_| Error::Session(tr!("tui-backlog").into()))
    }

    fn send<M: Msg>(&mut self, message: &M, reliable: bool) -> u32 {
        self.circuit.send(&self.socket, &self.stats, message, reliable)
    }

    fn im(&mut self, id: Uuid, dialog: u8, text: &str) -> u32 {
        let mut m = ImprovedInstantMessage::default();
        m.agent_data.agent_id = self.login.agent_id;
        m.agent_data.session_id = self.login.session_id;
        let b = &mut m.message_block;
        b.to_agent_id = id;
        b.id = id;
        b.dialog = dialog;
        b.from_agent_name = str_field(&format!("{} {}", self.login.first_name, self.login.last_name));
        b.message = str_field(text);
        b.binary_bucket = vec![0];
        self.send(&m, true)
    }

    fn complete_movement(&mut self) {
        let mut m = CompleteAgentMovement::default();
        m.agent_data.agent_id = self.login.agent_id;
        m.agent_data.session_id = self.login.session_id;
        m.agent_data.circuit_code = self.login.circuit_code;
        self.send(&m, true);
        let mut throttle = AgentThrottle::default();
        throttle.agent_data.agent_id = self.login.agent_id;
        throttle.agent_data.session_id = self.login.session_id;
        throttle.agent_data.circuit_code = self.login.circuit_code;
        // Deliberate chat-only difference from LLViewerThrottle: no world
        // channels; a small Task allowance retains chat and control traffic.
        throttle.throttle.throttles = [8000.0_f32, 0.0, 0.0, 0.0, 16000.0, 0.0, 0.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        self.send(&throttle, true);
    }

    fn agent_update(&mut self) {
        let mut m = AgentUpdate::default();
        let a = &mut m.agent_data;
        a.agent_id = self.login.agent_id;
        a.session_id = self.login.session_id;
        a.body_rotation = Quat::IDENTITY;
        a.head_rotation = Quat::IDENTITY;
        a.camera_center = self.position;
        a.camera_at_axis = Vec3::X;
        a.camera_left_axis = Vec3::Y;
        a.camera_up_axis = Vec3::Z;
        a.far = 0.0;
        self.send(&m, false);
    }

    fn remember_groups(&mut self, groups: Vec<GroupMembership>) -> Result<(), Error> {
        // UDP group lists may arrive in several packets; do not replace them.
        for group in groups {
            if self.groups.len() >= MAX_GROUPS && !self.groups.contains_key(&group.id) {
                return Err(Error::Session(tr!("tui-group-limit").into()));
            }
            if !group.id.is_nil() {
                self.groups.insert(group.id, group);
            }
        }
        let mut groups: Vec<_> = self.groups.values().cloned().collect();
        groups.sort_by_cached_key(|g| g.name.to_lowercase());
        self.emit(Event::Groups(groups))
    }

    fn received(&mut self, p: &IncomingPacket) -> Result<bool, Error> {
        let decode = |e: aurora_msg::DecodeError| Error::Session(e.to_string());
        if p.id == PacketAck::ID {
            let m: PacketAck = p.decode().map_err(decode)?;
            self.circuit.ack_received(m.packets.iter().map(|p| p.id));
        } else if p.id == StartPingCheck::ID {
            let m: StartPingCheck = p.decode().map_err(decode)?;
            let mut reply = CompletePingCheck::default();
            reply.ping_id.ping_id = m.ping_id.ping_id;
            self.send(&reply, false);
        } else if p.id == CompletePingCheck::ID {
            let m: CompletePingCheck = p.decode().map_err(decode)?;
            self.circuit.on_ping_reply(m.ping_id.ping_id);
        } else if p.id == RegionHandshake::ID {
            let _: RegionHandshake = p.decode().map_err(decode)?;
            let mut reply = RegionHandshakeReply::default();
            reply.agent_data.agent_id = self.login.agent_id;
            reply.agent_data.session_id = self.login.session_id;
            self.send(&reply, true);
        } else if p.id == AgentMovementComplete::ID {
            let m: AgentMovementComplete = p.decode().map_err(decode)?;
            if m.agent_data.agent_id != self.login.agent_id || m.agent_data.session_id != self.login.session_id {
                return Ok(false);
            }
            self.position = m.data.position;
            if !self.arrived {
                self.arrived = true;
                let mut groups = AgentDataUpdateRequest::default();
                groups.agent_data.agent_id = self.login.agent_id;
                groups.agent_data.session_id = self.login.session_id;
                self.send(&groups, true);
                self.agent_update();
            }
        } else if p.id == AgentGroupDataUpdate::ID {
            let m: AgentGroupDataUpdate = p.decode().map_err(decode)?;
            if m.agent_data.agent_id == self.login.agent_id {
                self.remember_groups(
                    m.group_data
                        .iter()
                        .map(|g| GroupMembership {
                            id: g.group_id,
                            name: field_str(&g.group_name),
                            insignia: g.group_insignia_id,
                            powers: g.group_powers,
                            accept_notices: g.accept_notices,
                            list_in_profile: true,
                            contribution: g.contribution,
                        })
                        .collect(),
                )?;
            }
        } else if p.id == ChatFromSimulator::ID {
            let m: ChatFromSimulator = p.decode().map_err(decode)?;
            let c = m.chat_data;
            if c.audible == 1 && !matches!(c.chat_type, 4 | 5) {
                self.emit(Event::Local {
                    from: field_str(&c.from_name),
                    text: field_str(&c.message),
                })?;
            }
        } else if p.id == AgentDropGroup::ID {
            let m: AgentDropGroup = p.decode().map_err(decode)?;
            if m.agent_data.agent_id == self.login.agent_id {
                let id = m.agent_data.group_id;
                self.close_group(id)?;
                self.groups.remove(&id);
                self.left.remove(&id);
                self.emit(Event::GroupRemoved(id))?;
            }
        } else if p.id == ImprovedInstantMessage::ID {
            let m: ImprovedInstantMessage = p.decode().map_err(decode)?;
            let b = m.message_block;
            if b.dialog == 17
                && (self.joined.contains(&b.id) || self.joining.contains_key(&b.id))
                && m.agent_data.agent_id != self.login.agent_id
            {
                self.emit(Event::Group {
                    id: b.id,
                    from: field_str(&b.from_agent_name),
                    text: field_str(&b.message),
                })?;
            }
        } else if p.id == KickUser::ID {
            let m: KickUser = p.decode().map_err(decode)?;
            return Err(Error::Session(field_str(&m.user_info.reason)));
        } else if p.id == DisableSimulator::ID {
            return Err(Error::Session(tr!("session-the-region-closed-the-connection").into()));
        } else if p.id == LogoutReply::ID {
            let m: LogoutReply = p.decode().map_err(decode)?;
            self.logged_out = m.agent_data.agent_id == self.login.agent_id && m.agent_data.session_id == self.login.session_id;
            return Ok(self.logged_out);
        }
        // Unneeded world packets are acknowledged but never decoded.
        Ok(false)
    }

    fn command(&mut self, command: Command) -> Result<bool, Error> {
        if matches!(command, Command::Logout) {
            let mut m = LogoutRequest::default();
            m.agent_data.agent_id = self.login.agent_id;
            m.agent_data.session_id = self.login.session_id;
            self.send(&m, true);
            return Ok(true);
        }
        if self.circuit.unacked_count() > MAX_PENDING - 8 {
            return self.emit(Event::Notice(tr!("tui-send-busy").into())).map(|()| false);
        }
        match command {
            Command::Join(id) => {
                if !self.groups.contains_key(&id) {
                    self.emit(Event::Notice(tr!("tui-not-member").into()))?;
                } else if self.joined.contains(&id) {
                    self.emit(Event::Joined(id))?;
                } else if !self.joining.contains_key(&id) {
                    if self.joined.len() + self.joining.len() >= MAX_SESSIONS {
                        self.emit(Event::Notice(tr!("tui-session-limit").into()))?;
                    } else {
                        self.left.remove(&id);
                        self.joining.insert(id, Instant::now());
                        self.im(id, 15, "");
                    }
                }
            }
            Command::Leave(id) => {
                self.im(id, 18, "");
                self.close_group(id)?;
            }
            Command::Local { text, kind } => {
                if valid_text(&text) && matches!(kind, 0..=2) {
                    for part in crate::message_split::message_parts(&text) {
                        let mut m = ChatFromViewer::default();
                        m.agent_data.agent_id = self.login.agent_id;
                        m.agent_data.session_id = self.login.session_id;
                        m.chat_data.message = str_field(part);
                        m.chat_data.type_ = kind;
                        m.chat_data.channel = 0;
                        self.send(&m, true);
                    }
                } else {
                    self.emit(Event::Notice(tr!("tui-invalid-text").into()))?;
                }
            }
            Command::Group { id, text } => {
                if !self.joined.contains(&id) || self.muted.contains(&id) {
                    self.emit(Event::Notice(tr!("tui-group-not-ready").into()))?;
                } else if valid_text(&text) {
                    for part in crate::message_split::message_parts(&text) {
                        let seq = self.im(id, 17, part);
                        self.sent.insert(seq, (id, part.to_owned()));
                    }
                } else {
                    self.emit(Event::Notice(tr!("tui-invalid-text").into()))?;
                }
            }
            Command::Logout => unreachable!(),
        }
        Ok(false)
    }

    fn close_group(&mut self, id: Uuid) -> Result<(), Error> {
        self.joined.remove(&id);
        self.joining.remove(&id);
        self.muted.remove(&id);
        self.sent.retain(|_, (group, _)| *group != id);
        if self.groups.contains_key(&id) {
            self.left.insert(id);
        }
        self.emit(Event::Left(id))
    }

    fn event(&mut self, message: &str, body: &Llsd) -> Result<(), Error> {
        match message {
            "AgentGroupDataUpdate" => self.remember_groups(social::parse_group_data(body))?,
            "AgentDropGroup" => {
                let id = body["AgentData"][0]["GroupID"].as_uuid();
                self.groups.remove(&id);
                self.left.remove(&id);
                self.close_group(id)?;
                self.emit(Event::GroupRemoved(id))?;
            }
            "ChatterBoxSessionStartReply" => {
                let temp = body["temp_session_id"].as_uuid();
                if self.joining.remove(&temp).is_some() {
                    let id = body["session_id"].as_uuid();
                    if body["success"].as_bool() && id == temp && self.groups.contains_key(&id) {
                        self.joined.insert(id);
                        self.emit(Event::Joined(id))?;
                    } else {
                        self.emit(Event::Notice(tr!("tui-join-failed", reason = body["error"].as_str())))?;
                    }
                }
            }
            "ChatterBoxSessionAgentListUpdates" => {
                let id = body["session_id"].as_uuid();
                if !self.groups.contains_key(&id) {
                    return Ok(());
                }
                for agent in social::parse_session_agents(body) {
                    if agent.agent == self.login.agent_id
                        && let Some(muted) = agent.text_muted
                    {
                        if muted {
                            self.muted.insert(id);
                        } else {
                            self.muted.remove(&id);
                        }
                        self.emit(Event::Notice(tr!("tui-moderation-changed").into()))?;
                    }
                }
            }
            "ChatterBoxSessionEventReply" if !body["success"].as_bool() => {
                self.emit(Event::Notice(tr!("tui-join-failed", reason = body["error"].as_str())))?;
            }
            "ForceCloseChatterBoxSession" => {
                self.close_group(body["session_id"].as_uuid())?;
                self.emit(Event::Notice(tr!("tui-join-failed", reason = body["reason"].as_str())))?;
            }
            _ => {}
        }
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.logged_out {
            let mut m = LogoutRequest::default();
            m.agent_data.agent_id = self.login.agent_id;
            m.agent_data.session_id = self.login.session_id;
            self.send(&m, true);
        }
    }
}

fn valid_text(text: &str) -> bool {
    !text.trim().is_empty() && text.len() <= MAX_INPUT_BYTES && !text.chars().any(char::is_control)
}

enum Job {
    QueueEnded,
    Accepted {
        id: Uuid,
        from: String,
        text: String,
        result: Result<Llsd, caps::CapsError>,
    },
}

/// Run on a Tokio runtime. All channels and retained session state are bounded.
/// The caller owns the task and must let Logout finish before dropping it.
pub async fn run(req: LoginRequest, events: mpsc::Sender<Event>, mut commands: mpsc::Receiver<Command>) -> Result<(), Error> {
    let socket = Arc::new(crate::circuit_socket::bind(false).await?);
    let ca = reqwest::Certificate::from_pem_bundle(include_bytes!("../lindenlab.pem"))
        .map_err(|e| Error::Setup(crate::NetError::Http(e.to_string())))?;
    let login_http = aurora_proxy::http_client(|b| {
        b.user_agent(aurora_version::user_agent())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(90))
            .tls_certs_merge(ca)
            .redirect(login::redirect_policy())
    })
    .map_err(|e| Error::Setup(crate::NetError::Http(e.without_url().to_string())))?;
    let tls = crate::simulator_tls()?;
    let http = aurora_proxy::http_client(|b| {
        b.user_agent(aurora_version::user_agent())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(40))
            .pool_max_idle_per_host(2)
            .redirect(reqwest::redirect::Policy::none())
            .tls_backend_preconfigured(tls)
    })
    .map_err(|e| Error::Setup(crate::NetError::Http(e.without_url().to_string())))?;
    let mut login = login::login_with_options(&login_http, &req, CHAT_OPTIONS).await?;
    drop(req);
    let seed_url = std::mem::take(&mut login.seed_capability);
    let addr = SocketAddr::V4(SocketAddrV4::new(login.sim_ip, login.sim_port));
    let mut s = Session {
        login,
        socket: CircuitSocket::direct(socket.clone()),
        circuit: Circuit::new(addr),
        stats: Arc::default(),
        events,
        groups: HashMap::new(),
        joined: HashSet::new(),
        joining: HashMap::new(),
        left: HashSet::new(),
        muted: HashSet::new(),
        sent: HashMap::new(),
        position: Vec3::ZERO,
        arrived: false,
        logged_out: false,
    };
    let seed = caps::resolve_seed_names(&http, &seed_url, CHAT_CAPS, caps::SeedRetry::FIRESTORM, || true);
    tokio::pin!(seed);
    let mut capabilities = None;
    let mut jobs = JoinSet::new();
    let (eq_tx, mut eq_rx) = mpsc::channel(128);
    socket.writable().await?;
    let mut ucc = UseCircuitCode::default();
    ucc.circuit_code.code = s.login.circuit_code;
    ucc.circuit_code.session_id = s.login.session_id;
    ucc.circuit_code.id = s.login.agent_id;
    let seq = s
        .circuit
        .send_reliable_with(&s.socket, &s.stats, &ucc, Resend::LOGIN_USE_CIRCUIT_CODE);
    let mut movement_sent = false;
    let mut announced = false;
    let started = Instant::now();
    let mut logout = None::<Instant>;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut update = Instant::now();
    let mut ping = Instant::now();
    let mut buffer = [0; 65536];
    loop {
        tokio::select! {
            result = &mut seed, if capabilities.is_none() => {
                let caps = result.map_err(|e| Error::Session(e.to_string()))?;
                let queue = caps.get("EventQueueGet").ok_or_else(|| Error::Session(tr!("tui-no-capabilities").into()))?.clone();
                if !caps.contains_key("ChatSessionRequest") {
                    return Err(Error::Session(tr!("tui-no-capabilities").into()));
                }
                let client = http.clone();
                let tx = eq_tx.clone();
                let stats = s.stats.clone();
                jobs.spawn(async move {
                    caps::bounded_event_queue_loop(client, queue, addr, tx, stats).await;
                    Job::QueueEnded
                });
                capabilities = Some(caps);
            }
            received = s.socket.recv_from(&mut buffer) => {
                let Some((range, from)) = received? else { continue; };
                if from != addr { continue; }
                let p = match parse_packet(&buffer[range]) {
                    Ok(packet) => packet,
                    Err(error) => {
                        log::warn!("chat: malformed UDP packet: {error}");
                        continue;
                    }
                };
                if !s.circuit.on_receive(p.flags, p.sequence, &p.acks) { continue; }
                let finished = s.received(&p)?;
                s.circuit.accept(p.flags, p.sequence);
                if finished { break; }
            }
            Some(command) = commands.recv(), if logout.is_none() => {
                if matches!(command, Command::Logout) || announced {
                    if s.command(command)? { logout = Some(Instant::now()); }
                } else {
                    s.emit(Event::Notice(tr!("tui-connecting").into()))?;
                }
            }
            Some(message) = eq_rx.recv() => {
                match message {
                    EqMessage::Lost(_) => return Err(Error::Session(tr!("tui-queue-lost").into())),
                    EqMessage::Event(ev) if ev.message == "ChatterBoxInvitation" => {
                        if let Some(inv) = social::parse_invitation(&ev.body) {
                            let id = inv.session;
                            if !s.groups.contains_key(&id) || s.left.contains(&id)
                                || s.joined.contains(&id) || s.joining.contains_key(&id) {
                                continue;
                            }
                            if s.joined.len() + s.joining.len() >= MAX_SESSIONS {
                                s.emit(Event::Notice(tr!("tui-session-limit").into()))?;
                                continue;
                            }
                            if let Some(url) = capabilities.as_ref().and_then(|caps| caps.get("ChatSessionRequest")) {
                                let url = url.clone();
                                let http = http.clone();
                                s.joining.insert(id, Instant::now());
                                jobs.spawn(async move {
                                    let body = llsd_map! { "method" => "accept invitation", "session-id" => id };
                                    let result = caps::post_llsd(&http, &url, &body).await;
                                    Job::Accepted { id, from: inv.from_name, text: inv.message, result }
                                });
                            }
                        }
                    }
                    EqMessage::Event(ev) => s.event(&ev.message, &ev.body)?,
                }
            }
            Some(result) = jobs.join_next(), if !jobs.is_empty() => {
                match result.map_err(|e| Error::Session(e.to_string()))? {
                    Job::QueueEnded => return Err(Error::Session(tr!("tui-queue-lost").into())),
                    Job::Accepted { id, from, text, result } => {
                        if s.joining.remove(&id).is_some() && !s.left.contains(&id) && s.groups.contains_key(&id) {
                            match result {
                                Ok(_) => {
                                    s.joined.insert(id);
                                    s.emit(Event::Joined(id))?;
                                    s.emit(Event::Group { id, from, text })?;
                                }
                                Err(e) => s.emit(Event::Notice(tr!("tui-join-failed", reason = e.to_string())))?,
                            }
                        } else if result.is_ok() {
                            // A pending accept can finish after /part or a
                            // membership removal; close the server session too.
                            s.im(id, 18, "");
                        }
                    }
                }
            }
            _ = tick.tick() => {
                s.circuit.tick(&s.socket, &s.stats);
                if !s.circuit.take_dropped().is_empty() {
                    return Err(Error::Session(tr!("tui-delivery-failed").into()));
                }
                if !movement_sent && !s.circuit.is_unacked(seq) {
                    movement_sent = true;
                    s.complete_movement();
                }
                if !announced && s.arrived && capabilities.is_some() {
                    announced = true;
                    s.emit(Event::Connected(format!("{} {}", s.login.first_name, s.login.last_name)))?;
                }
                let acknowledged: Vec<_> = s.sent.keys().filter(|seq| !s.circuit.is_unacked(**seq)).copied().collect();
                for seq in acknowledged {
                    if let Some((id, text)) = s.sent.remove(&seq) {
                        s.emit(Event::Group { id, from: format!("{} {}", s.login.first_name, s.login.last_name), text })?;
                    }
                }
                if commands.is_closed() && logout.is_none() {
                    s.command(Command::Logout)?;
                    logout = Some(Instant::now());
                }
                if logout.is_some_and(|at| at.elapsed() > Duration::from_secs(5)) {
                    return Err(Error::Session(tr!("tui-logout-timeout").into()));
                }
                if s.circuit.last_recv.elapsed() > Duration::from_secs(100)
                    || (!announced && started.elapsed() > Duration::from_secs(240)) {
                    return Err(Error::Session(tr!("tui-connection-timeout").into()));
                }
                if logout.is_none() && s.arrived && update.elapsed() >= Duration::from_secs(1) {
                    s.agent_update();
                    update = Instant::now();
                }
                if ping.elapsed() >= Duration::from_secs(5) {
                    let (id, oldest) = s.circuit.next_ping();
                    let mut m = StartPingCheck::default();
                    m.ping_id.ping_id = id;
                    m.ping_id.oldest_unacked = oldest;
                    s.send(&m, false);
                    ping = Instant::now();
                }
                let before = s.joining.len();
                s.joining.retain(|_, at| at.elapsed() < Duration::from_secs(45));
                if s.joining.len() != before {
                    s.emit(Event::Notice(tr!("tui-join-timeout").into()))?;
                }
            }
        }
    }
    if let Some(url) = capabilities.as_ref().and_then(|caps| caps.get("EventQueueGet")) {
        jobs.abort_all();
        let done = llsd_map! { "ack" => Llsd::Undef, "done" => true };
        match tokio::time::timeout(Duration::from_secs(2), caps::post_llsd(&http, url, &done)).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => log::warn!("chat: the event queue did not confirm shutdown"),
            Err(_) => log::warn!("chat: event queue shutdown timed out"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use tokio::net::UdpSocket;

    async fn fixture() -> (Session, mpsc::Receiver<Event>, UdpSocket) {
        let simulator = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let client = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        client.writable().await.unwrap();
        let (events, rx) = mpsc::channel(128);
        let login = LoginResponse {
            agent_id: Uuid::from_u128(10),
            session_id: Uuid::from_u128(20),
            secure_session_id: Uuid::nil(),
            first_name: "Test".into(),
            last_name: "Resident".into(),
            sim_ip: Ipv4Addr::LOCALHOST,
            sim_port: simulator.local_addr().unwrap().port(),
            circuit_code: 123,
            region_x: 0,
            region_y: 0,
            region_size_x: 256,
            region_size_y: 256,
            seed_capability: String::new(),
            look_at: Vec3::X,
            message: String::new(),
            agent_appearance_service: String::new(),
            inventory_root: Uuid::nil(),
            mfa_hash: None,
            raw: Llsd::Undef,
        };
        let session = Session {
            login,
            socket: CircuitSocket::direct(client),
            circuit: Circuit::new(simulator.local_addr().unwrap()),
            stats: Arc::default(),
            events,
            groups: HashMap::new(),
            joined: HashSet::new(),
            joining: HashMap::new(),
            left: HashSet::new(),
            muted: HashSet::new(),
            sent: HashMap::new(),
            position: Vec3::ZERO,
            arrived: false,
            logged_out: true,
        };
        (session, rx, simulator)
    }

    async fn packet(simulator: &UdpSocket) -> IncomingPacket {
        let mut bytes = [0; 65536];
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), simulator.recv_from(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        parse_packet(&bytes[..n]).unwrap()
    }

    fn incoming(message: &impl Msg) -> IncomingPacket {
        let mut bytes = Vec::new();
        aurora_msg::build_packet(message, 1, true, false, &[], &mut bytes);
        parse_packet(&bytes).unwrap()
    }

    fn group(id: u128) -> GroupMembership {
        GroupMembership {
            id: Uuid::from_u128(id),
            name: format!("Group {id}"),
            insignia: Uuid::nil(),
            powers: 0,
            accept_notices: false,
            list_in_profile: true,
            contribution: 0,
        }
    }

    #[test]
    fn chat_input_is_bounded_and_never_carries_terminal_controls() {
        assert!(valid_text("Hello, 世界 👋"));
        assert!(!valid_text(""));
        assert!(!valid_text("   "));
        for text in ["line\nbreak", "\x1b[2J", "a\0b", "\rhello"] {
            assert!(!valid_text(text));
        }
        assert!(valid_text(&"x".repeat(MAX_INPUT_BYTES)));
        assert!(!valid_text(&"x".repeat(MAX_INPUT_BYTES + 1)));
    }

    #[test]
    fn chat_requests_no_world_or_media_capabilities() {
        assert_eq!(CHAT_CAPS, ["EventQueueGet", "ChatSessionRequest"]);
        assert!(!CHAT_OPTIONS.iter().any(|name| name.contains("inventory") || name.contains("voice")));
    }

    #[tokio::test]
    async fn chat_handshake_sets_zero_world_channels_and_zero_draw_distance() {
        let (mut session, _, simulator) = fixture().await;
        session.complete_movement();
        let movement = packet(&simulator).await.decode::<CompleteAgentMovement>().unwrap();
        assert_eq!(movement.agent_data.agent_id, session.login.agent_id);
        assert_eq!(movement.agent_data.circuit_code, 123);
        let throttle = packet(&simulator).await.decode::<AgentThrottle>().unwrap();
        let bytes = throttle.throttle.throttles;
        assert_eq!(bytes.len(), 7 * 4);
        let rates: Vec<_> = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
        assert_eq!(rates, [8000.0, 0.0, 0.0, 0.0, 16000.0, 0.0, 0.0]);
        session.position = Vec3::new(1.0, 2.0, 3.0);
        session.agent_update();
        let update = packet(&simulator).await.decode::<AgentUpdate>().unwrap();
        assert_eq!(update.agent_data.far, 0.0);
        assert_eq!(update.agent_data.camera_center, session.position);
        assert_eq!(update.agent_data.control_flags, 0);
    }

    #[tokio::test]
    async fn chat_group_start_send_and_part_use_the_existing_membership() {
        let (mut session, mut events, simulator) = fixture().await;
        session.remember_groups(vec![group(1)]).unwrap();
        session.remember_groups(vec![group(2)]).unwrap();
        assert_eq!(session.groups.len(), 2);
        session.command(Command::Join(Uuid::from_u128(1))).unwrap();
        let start = packet(&simulator).await.decode::<ImprovedInstantMessage>().unwrap();
        assert_eq!(start.message_block.dialog, 15);
        assert_eq!(start.message_block.id, Uuid::from_u128(1));
        session
            .event(
                "ChatterBoxSessionStartReply",
                &llsd_map! { "temp_session_id" => Uuid::from_u128(1), "session_id" => Uuid::from_u128(1), "success" => true },
            )
            .unwrap();
        assert!(session.joined.contains(&Uuid::from_u128(1)));
        while events.try_recv().is_ok() {}
        session
            .command(Command::Group {
                id: Uuid::from_u128(1),
                text: "Hello 桜".into(),
            })
            .unwrap();
        let message = packet(&simulator).await;
        let im: ImprovedInstantMessage = message.decode().unwrap();
        assert_eq!(im.message_block.dialog, 17);
        assert_eq!(im.message_block.to_agent_id, Uuid::from_u128(1));
        assert_eq!(field_str(&im.message_block.message), "Hello 桜");
        assert!(events.try_recv().is_err(), "do not echo unacknowledged sends");
        assert!(session.sent.contains_key(&message.sequence));
        session.command(Command::Leave(Uuid::from_u128(1))).unwrap();
        let leave = packet(&simulator).await.decode::<ImprovedInstantMessage>().unwrap();
        assert_eq!(leave.message_block.dialog, 18);
        assert_eq!(session.groups.len(), 2, "/part never leaves the group");
        assert!(session.sent.is_empty(), "a late ack must not reopen a closed chat");
    }

    #[tokio::test]
    async fn chat_local_filters_typing_and_inaudible_lines_but_ignores_world_data() {
        let (mut session, mut events, _) = fixture().await;
        let mut message = ChatFromSimulator::default();
        message.chat_data.from_name = str_field("Speaker");
        message.chat_data.message = str_field("Hello");
        for (audible, kind) in [(0, 1), (255, 1), (1, 4), (1, 5)] {
            message.chat_data.audible = audible;
            message.chat_data.chat_type = kind;
            session.received(&incoming(&message)).unwrap();
            assert!(events.try_recv().is_err());
        }
        message.chat_data.audible = 1;
        message.chat_data.chat_type = 1;
        session.received(&incoming(&message)).unwrap();
        assert!(matches!(events.try_recv(), Ok(Event::Local { text, .. }) if text == "Hello"));
        let mut packet = incoming(&message);
        packet.id = ObjectUpdate::ID;
        packet.body.clear();
        assert!(!session.received(&packet).unwrap(), "world payloads must not be decoded");
    }

    #[tokio::test]
    async fn chat_rejects_unknown_groups_and_oversized_input_without_sending() {
        let (mut session, mut events, _) = fixture().await;
        session.command(Command::Join(Uuid::from_u128(999))).unwrap();
        assert!(matches!(events.try_recv(), Ok(Event::Notice(_))));
        assert_eq!(session.circuit.unacked_count(), 0);
        session
            .command(Command::Local {
                text: "x".repeat(MAX_INPUT_BYTES + 1),
                kind: 1,
            })
            .unwrap();
        assert!(matches!(events.try_recv(), Ok(Event::Notice(_))));
        assert_eq!(session.circuit.unacked_count(), 0);
        for id in 1..1000 {
            session.close_group(Uuid::from_u128(id)).unwrap();
            events.try_recv().unwrap();
        }
        assert!(session.left.is_empty(), "unrecognized sessions must not consume retained state");
    }

    #[tokio::test]
    async fn chat_membership_removal_discards_closed_session_state() {
        let (mut session, mut events, _) = fixture().await;
        let id = Uuid::from_u128(1);
        session.remember_groups(vec![group(1)]).unwrap();
        session.close_group(id).unwrap();
        assert!(session.left.contains(&id));
        session
            .event("AgentDropGroup", &llsd_map! { "AgentData" => vec![llsd_map! { "GroupID" => id }] })
            .unwrap();
        assert!(!session.groups.contains_key(&id));
        assert!(!session.left.contains(&id));
        let mut removed = false;
        while let Ok(event) = events.try_recv() {
            removed |= matches!(event, Event::GroupRemoved(group) if group == id);
        }
        assert!(removed);
    }
}
