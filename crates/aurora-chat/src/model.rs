use aurora_i18n::tr;
use aurora_net::chat::{Command, Event, Friend, MAX_INPUT_BYTES, MAX_RESIDENTS, MAX_SESSIONS, Nearby, ResidentName};
use aurora_net::social::GroupMembership;
use std::collections::{HashMap, VecDeque};
use uuid::Uuid;
use zeroize::Zeroizing;

pub const MAX_LINES: usize = 128;
const MAX_LINE_BYTES: usize = 2048;

#[derive(Default)]
pub struct Editor {
    pub text: Zeroizing<String>,
    pub cursor: usize,
}

impl Editor {
    pub fn insert(&mut self, text: &str) {
        for c in text.chars().filter(|c| !c.is_control()) {
            if self.text.len() + c.len_utf8() > MAX_INPUT_BYTES {
                break;
            }
            self.text.insert(self.cursor, c);
            self.cursor += c.len_utf8();
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.text[..self.cursor].char_indices().next_back().map_or(0, |(i, _)| i);
    }

    pub fn right(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    pub fn backspace(&mut self) {
        let end = self.cursor;
        self.left();
        self.text.drain(self.cursor..end);
    }

    pub fn delete(&mut self) {
        let start = self.cursor;
        self.right();
        let end = self.cursor;
        self.cursor = start;
        self.text.drain(start..end);
    }

    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut *self.text)
    }
}

/// Never let server text act as a terminal escape, bidi override or title.
pub fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !c.is_control() && !matches!(*c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .scan(0usize, |bytes, c| {
            *bytes += c.len_utf8();
            (*bytes <= MAX_LINE_BYTES).then_some(c)
        })
        .collect()
}

fn slt_timestamp(now: chrono::DateTime<chrono::Utc>) -> String {
    now.with_timezone(&chrono_tz::America::Los_Angeles)
        .format("%Y/%m/%d %H:%M")
        .to_string()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chat {
    Local,
    Group(Uuid),
    Direct(Uuid),
}

pub struct Entry {
    timestamp: String,
    sender: Option<Uuid>,
    from: String,
    text: String,
}

pub struct Buffer {
    pub chat: Chat,
    pub name: String,
    pub ready: bool,
    pub unread: usize,
    pub lines: VecDeque<Entry>,
    pub scroll: u16,
    pub draft: Editor,
}

impl Buffer {
    fn new(chat: Chat, name: String) -> Self {
        Self {
            chat,
            name: clean(&name),
            ready: !matches!(chat, Chat::Group(_)),
            unread: 0,
            lines: VecDeque::new(),
            scroll: 0,
            draft: Editor::default(),
        }
    }

    fn push(&mut self, line: Entry) {
        if self.lines.len() == MAX_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }
}

pub enum Action {
    None,
    Send(Command),
    Quit,
}

enum RecipientError {
    Unknown,
    Ambiguous,
}

pub struct Model {
    pub buffers: Vec<Buffer>,
    pub groups: Vec<GroupMembership>,
    pub current: usize,
    pub names: HashMap<Uuid, ResidentName>,
    name_order: VecDeque<Uuid>,
    pub friends: Vec<Friend>,
    pub nearby: Vec<Nearby>,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            buffers: vec![Buffer::new(Chat::Local, tr!("tui-local").into())],
            groups: Vec::new(),
            current: 0,
            names: HashMap::new(),
            name_order: VecDeque::new(),
            friends: Vec::new(),
            nearby: Vec::new(),
        }
    }
}

impl Model {
    pub fn active(&self) -> &Buffer {
        &self.buffers[self.current]
    }

    pub fn active_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[self.current]
    }

    pub fn switch(&mut self, step: isize) {
        self.current = (self.current as isize + step).rem_euclid(self.buffers.len() as isize) as usize;
        self.active_mut().unread = 0;
    }

    pub fn notice(&mut self, text: &str) {
        self.notice_to(self.current, text);
    }

    fn notice_to(&mut self, index: usize, text: &str) {
        self.buffers[index].push(Entry {
            timestamp: slt_timestamp(chrono::Utc::now()),
            sender: None,
            from: String::new(),
            text: clean(text),
        });
    }

    pub fn resident_name(&self, id: Uuid) -> String {
        self.names.get(&id).map_or_else(
            || id.to_string(),
            |name| {
                if name.display_name.is_empty() && name.legacy_name.is_empty() {
                    id.to_string()
                } else if name.display_name.is_empty() || name.display_name == name.legacy_name {
                    name.legacy_name.clone()
                } else if name.legacy_name.is_empty() {
                    name.display_name.clone()
                } else {
                    format!("{} ({})", name.display_name, name.legacy_name)
                }
            },
        )
    }

    pub fn title(&self, buffer: &Buffer) -> String {
        match buffer.chat {
            Chat::Direct(id) => tr!("tui-direct-title", name = self.resident_name(id)),
            _ => buffer.name.clone(),
        }
    }

    pub fn format_entry(&self, entry: &Entry) -> String {
        let from = entry
            .sender
            .filter(|id| self.names.contains_key(id))
            .map_or_else(|| entry.from.clone(), |id| self.resident_name(id));
        if from.is_empty() {
            format!("[{}] * {}", entry.timestamp, entry.text)
        } else {
            format!("[{}] {from}: {}", entry.timestamp, entry.text)
        }
    }

    pub fn radar_rows(&self) -> Vec<String> {
        // FSRadar::update (indra/newview/fsradar.cpp) excludes unknown
        // altitude from chat range; normal chat uses the default 20 m radius.
        self.nearby
            .iter()
            .map(|resident| {
                let range = match resident.distance {
                    Some(distance) if distance.is_finite() && distance >= 0.0 => {
                        if distance <= 20.0 {
                            tr!("tui-radar-in-range", distance = format!("{distance:.1}"))
                        } else {
                            tr!("tui-radar-outside", distance = format!("{distance:.1}"))
                        }
                    }
                    _ => tr!("tui-radar-unknown").into(),
                };
                format!("{} — {range}", self.resident_name(resident.id))
            })
            .collect()
    }

    fn buffer(&mut self, chat: Chat) -> Option<usize> {
        if let Some(index) = self.buffers.iter().position(|b| b.chat == chat) {
            return Some(index);
        }
        if self.buffers.len() > MAX_SESSIONS {
            self.notice(tr!("tui-session-limit"));
            return None;
        }
        let name = match chat {
            Chat::Group(id) => self.groups.iter().find(|g| g.id == id)?.name.clone(),
            Chat::Direct(id) => self.resident_name(id),
            Chat::Local => return Some(0),
        };
        self.buffers.push(Buffer::new(chat, name));
        Some(self.buffers.len() - 1)
    }

    fn append(&mut self, index: usize, sender: Option<Uuid>, from: &str, text: &str) {
        // Split remote paragraphs into bounded rows; never retain an unbounded line.
        let timestamp = slt_timestamp(chrono::Utc::now());
        for line in text.lines().take(MAX_LINES) {
            let from = clean(from);
            let text = clean(line);
            self.buffers[index].push(Entry {
                timestamp: timestamp.clone(),
                sender,
                from,
                text,
            });
        }
        if index != self.current {
            self.buffers[index].unread = self.buffers[index].unread.saturating_add(1);
        }
    }

    pub fn event(&mut self, event: Event) {
        match event {
            Event::Connected(name) => self.notice(&tr!("tui-connected-as", name = clean(&name))),
            Event::Groups(groups) => {
                self.groups = groups;
                for buffer in &mut self.buffers {
                    if let Some(group) = self.groups.iter().find(|g| buffer.chat == Chat::Group(g.id)) {
                        buffer.name = clean(&group.name);
                    }
                }
            }
            Event::GroupRemoved(id) => {
                self.groups.retain(|g| g.id != id);
                self.remove(Chat::Group(id));
            }
            Event::Local { sender, from, text } => self.append(0, sender, &from, &text),
            Event::Group { id, sender, from, text } => {
                if let Some(index) = self.buffer(Chat::Group(id)) {
                    self.append(index, sender, &from, &text);
                }
            }
            Event::Direct { id, sender, from, text } => {
                if let Some(index) = self.buffer(Chat::Direct(id)) {
                    self.append(index, Some(sender), &from, &text);
                }
            }
            Event::Names(names) => {
                for mut name in names.into_iter().take(MAX_RESIDENTS) {
                    name.display_name = clean(&name.display_name);
                    name.legacy_name = clean(&name.legacy_name);
                    if let Some(existing) = self.names.get(&name.id) {
                        if name.display_name.is_empty() {
                            name.display_name.clone_from(&existing.display_name);
                        }
                        if name.legacy_name.is_empty() {
                            name.legacy_name.clone_from(&existing.legacy_name);
                        }
                    } else {
                        if self.names.len() == MAX_RESIDENTS
                            && let Some(oldest) = self.name_order.pop_front()
                        {
                            self.names.remove(&oldest);
                        }
                        self.name_order.push_back(name.id);
                    }
                    self.names.insert(name.id, name);
                }
            }
            Event::Friends(friends) => self.friends = friends.into_iter().take(MAX_RESIDENTS).collect(),
            Event::Nearby(nearby) => self.nearby = nearby.into_iter().take(MAX_RESIDENTS).collect(),
            Event::Joined(id) => {
                if let Some(index) = self.buffer(Chat::Group(id)) {
                    self.buffers[index].ready = true;
                    self.notice_to(index, tr!("tui-session-ready"));
                }
            }
            Event::Left(id) => self.remove(Chat::Group(id)),
            Event::Diagnostics(lines) => {
                self.notice(tr!("tui-diagnostics-intro"));
                self.append(self.current, None, "", &lines.join("\n"));
            }
            Event::Notice(text) => self.notice(&text),
        }
    }

    fn remove(&mut self, chat: Chat) {
        if let Some(index) = self.buffers.iter().position(|b| b.chat == chat) {
            self.buffers.remove(index);
            if self.current >= index {
                self.current = self.current.saturating_sub(1);
            }
            self.active_mut().unread = 0;
        }
    }

    fn recipient(&self, value: &str) -> Result<Uuid, RecipientError> {
        if value.is_empty() {
            return Err(RecipientError::Unknown);
        }
        if let Ok(id) = Uuid::parse_str(value) {
            return Ok(id);
        }
        if let Some(friend) = value
            .parse::<usize>()
            .ok()
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| self.friends.get(i))
        {
            return Ok(friend.id);
        }
        let mut matches = self
            .names
            .values()
            .filter(|name| name.display_name.eq_ignore_ascii_case(value) || name.legacy_name.eq_ignore_ascii_case(value));
        let id = matches.next().ok_or(RecipientError::Unknown)?.id;
        if matches.next().is_none() {
            Ok(id)
        } else {
            Err(RecipientError::Ambiguous)
        }
    }

    fn direct_command(&mut self, argument: &str, query: bool) -> Action {
        let recipient = if query {
            self.recipient(argument).map(|id| (id, "")).ok()
        } else {
            // Full names may contain spaces: prefer the longest exact name.
            let mut found = None;
            for (i, _) in argument.char_indices().rev().filter(|(_, c)| c.is_whitespace()) {
                let text = argument[i..].trim();
                if text.is_empty() {
                    continue;
                }
                match self.recipient(argument[..i].trim()) {
                    Ok(id) => {
                        found = Some((id, text));
                        break;
                    }
                    Err(RecipientError::Ambiguous) => break,
                    Err(RecipientError::Unknown) => {}
                }
            }
            found
        };
        let Some((id, text)) = recipient else {
            self.notice(tr!("tui-recipient-usage"));
            return Action::None;
        };
        let Some(index) = self.buffer(Chat::Direct(id)) else {
            return Action::None;
        };
        self.current = index;
        self.active_mut().unread = 0;
        if query {
            Action::None
        } else {
            Action::Send(Command::Direct { id, text: text.into() })
        }
    }

    fn list(&mut self, heading: &str, rows: Vec<String>, argument: &str) {
        const PAGE_SIZE: usize = 100;
        let pages = rows.len().div_ceil(PAGE_SIZE).max(1);
        let page = if argument.is_empty() {
            Some(1)
        } else {
            argument.parse::<usize>().ok()
        };
        let Some(page) = page.filter(|page| (1..=pages).contains(page)) else {
            self.notice(tr!("tui-list-page-usage"));
            return;
        };
        self.notice(heading);
        for line in rows.iter().skip((page - 1) * PAGE_SIZE).take(PAGE_SIZE) {
            self.notice(line);
        }
        self.notice(&tr!("tui-list-page", page = page.to_string(), pages = pages.to_string()));
    }

    pub fn submit(&mut self) -> Action {
        let input = self.active_mut().draft.take();
        let input = input.trim();
        if input.is_empty() {
            return Action::None;
        }
        // LLChatBar::stripChannelNumber (indra/newview/llchatbar.cpp).
        // Deliberate difference: // remains a literal slash, never repeat-channel.
        if input
            .strip_prefix('/')
            .and_then(|s| s.chars().next())
            .is_some_and(|c| c.is_ascii_digit() || c == '-' || c == '+')
        {
            let rest = &input[1..];
            let end = rest
                .char_indices()
                .skip(usize::from(rest.starts_with(['-', '+'])))
                .find(|(_, c)| !c.is_ascii_digit())
                .map_or(rest.len(), |(i, _)| i);
            let text = rest[end..].trim();
            let channel = match rest[..end].parse::<i32>() {
                Ok(channel) if channel != i32::MAX && !text.is_empty() => channel,
                _ => {
                    self.notice(tr!("tui-channel-usage"));
                    return Action::None;
                }
            };
            if self.active().chat != Chat::Local {
                self.notice(tr!("tui-channel-local"));
                return Action::None;
            }
            return Action::Send(Command::Local {
                text: text.into(),
                kind: 1,
                channel,
            });
        }
        let (command, argument) = input.split_once(' ').unwrap_or((input, ""));
        let argument = argument.trim();
        match command {
            "/quit" => Action::Quit,
            "/help" => {
                for line in tr!("tui-help").lines() {
                    self.notice(line);
                }
                Action::None
            }
            "/diagnostics" => Action::Send(Command::Diagnostics),
            "/finishanim" if argument.is_empty() => Action::Send(Command::FinishAnimation),
            "/groups" => {
                self.notice(tr!("tui-groups-heading"));
                let lines: Vec<_> = self
                    .groups
                    .iter()
                    .enumerate()
                    .map(|(i, g)| format!("{}: {}", i + 1, clean(&g.name)))
                    .collect();
                for line in lines {
                    self.notice(&line);
                }
                Action::None
            }
            "/friends" => {
                let lines: Vec<_> = self
                    .friends
                    .iter()
                    .enumerate()
                    .map(|(i, friend)| {
                        let status = if friend.online { tr!("tui-online") } else { tr!("tui-offline") };
                        format!("{}: {} — {status}", i + 1, self.resident_name(friend.id))
                    })
                    .collect();
                self.list(tr!("tui-friends-heading"), lines, argument);
                Action::None
            }
            "/radar" | "/nearby" => {
                self.list(tr!("tui-radar-heading"), self.radar_rows(), argument);
                Action::None
            }
            "/query" => self.direct_command(argument, true),
            "/msg" | "/im" => self.direct_command(argument, false),
            "/local" => {
                self.current = 0;
                self.active_mut().unread = 0;
                Action::None
            }
            "/next" | "/prev" => {
                self.switch(if command == "/next" { 1 } else { -1 });
                Action::None
            }
            "/join" => {
                let group = argument
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| i.checked_sub(1))
                    .and_then(|i| self.groups.get(i))
                    .or_else(|| self.groups.iter().find(|g| g.name.eq_ignore_ascii_case(argument)));
                if let Some(id) = group.map(|g| g.id) {
                    if let Some(index) = self.buffer(Chat::Group(id)) {
                        self.current = index;
                        self.active_mut().unread = 0;
                        return Action::Send(Command::Join(id));
                    }
                } else {
                    self.notice(tr!("tui-join-usage"));
                }
                Action::None
            }
            "/part" => match self.active().chat {
                Chat::Group(id) => Action::Send(Command::Leave(id)),
                Chat::Direct(id) => {
                    self.remove(Chat::Direct(id));
                    Action::None
                }
                Chat::Local => {
                    self.notice(tr!("tui-part-local"));
                    Action::None
                }
            },
            "/whisper" | "/shout" => {
                if self.active().chat != Chat::Local || argument.is_empty() {
                    self.notice(tr!("tui-local-command"));
                    Action::None
                } else {
                    Action::Send(Command::Local {
                        text: argument.into(),
                        kind: if command == "/whisper" { 0 } else { 2 },
                        channel: 0,
                    })
                }
            }
            _ if input.starts_with('/') && !input.starts_with("//") && command != "/me" => {
                self.notice(tr!("tui-unknown-command"));
                Action::None
            }
            _ => {
                let text = input.strip_prefix("//").map_or(input, |_| &input[1..]).to_owned();
                match self.active().chat {
                    Chat::Group(id) => {
                        if self.active().ready {
                            Action::Send(Command::Group { id, text })
                        } else {
                            self.notice(tr!("tui-group-not-ready"));
                            Action::None
                        }
                    }
                    Chat::Direct(id) => Action::Send(Command::Direct { id, text }),
                    Chat::Local => Action::Send(Command::Local { text, kind: 1, channel: 0 }),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn groups() -> Vec<GroupMembership> {
        vec![GroupMembership {
            id: Uuid::from_u128(1),
            name: "Demo group".into(),
            insignia: Uuid::nil(),
            powers: 0,
            accept_notices: false,
            list_in_profile: true,
            contribution: 0,
        }]
    }

    fn submit(model: &mut Model, line: &str) -> Action {
        model.active_mut().draft.insert(line);
        model.submit()
    }

    #[test]
    fn slt_timestamps_follow_pacific_daylight_saving_transitions() {
        let cases = [
            ("2026-01-15T20:30:00Z", "2026/01/15 12:30"),
            ("2026-07-15T20:30:00Z", "2026/07/15 13:30"),
            ("2026-03-08T09:59:00Z", "2026/03/08 01:59"),
            ("2026-03-08T10:00:00Z", "2026/03/08 03:00"),
            ("2026-11-01T08:59:00Z", "2026/11/01 01:59"),
            ("2026-11-01T09:00:00Z", "2026/11/01 01:00"),
            ("2026-01-01T02:00:00Z", "2025/12/31 18:00"),
        ];
        for (utc, expected) in cases {
            let instant = chrono::DateTime::parse_from_rfc3339(utc).unwrap().to_utc();
            assert_eq!(slt_timestamp(instant), expected, "{utc}");
        }
    }

    #[test]
    fn notices_and_all_message_lines_use_slt_receipt_time() {
        let mut model = Model::default();
        let before = slt_timestamp(chrono::Utc::now());
        model.notice("notice");
        model.append(0, None, "Resident", "first\nsecond");
        let after = slt_timestamp(chrono::Utc::now());
        let entries = &model.active().lines;
        assert_eq!(entries.len(), 3);
        for entry in entries {
            assert!(entry.timestamp == before || entry.timestamp == after);
        }
        assert_eq!(entries[1].timestamp, entries[2].timestamp);
        assert_eq!(model.format_entry(&entries[0]), format!("[{}] * notice", entries[0].timestamp));
        assert_eq!(
            model.format_entry(&entries[1]),
            format!("[{}] Resident: first", entries[1].timestamp)
        );
    }

    #[test]
    fn diagnostics_are_a_local_snapshot_command_in_every_chat() {
        let mut model = Model::default();
        assert!(matches!(submit(&mut model, "/diagnostics"), Action::Send(Command::Diagnostics)));
        assert!(matches!(submit(&mut model, "/finishanim"), Action::Send(Command::FinishAnimation)));
        assert!(matches!(submit(&mut model, "/finishanim arbitrary-id"), Action::None));
        model.event(Event::Direct {
            id: Uuid::from_u128(11),
            sender: Uuid::from_u128(11),
            from: "Peer".into(),
            text: "hello".into(),
        });
        model.switch(1);
        assert!(matches!(submit(&mut model, "/diagnostics"), Action::Send(Command::Diagnostics)));
        assert!(matches!(submit(&mut model, "/finishanim"), Action::Send(Command::FinishAnimation)));
        model.event(Event::Diagnostics(vec!["diagnostics=1".into(), "received_im_dialogs: 19=1".into()]));
        let lines: Vec<_> = model.active().lines.iter().map(|line| model.format_entry(line)).collect();
        assert!(lines.iter().any(|line| line.ends_with("* diagnostics=1")));
        assert!(lines.iter().any(|line| line.ends_with("* received_im_dialogs: 19=1")));
        assert!(lines.iter().any(|line| line.contains(&*tr!("tui-diagnostics-intro"))));
    }

    #[test]
    fn editing_preserves_utf8_boundaries_and_caps_pastes() {
        let mut e = Editor::default();
        e.insert("a桜😀b");
        e.left();
        e.backspace();
        assert_eq!(&*e.text, "a桜b");
        e.left();
        e.delete();
        assert_eq!(&*e.text, "ab");
        e.insert("\x1b\n\r\tZ");
        assert_eq!(&*e.text, "aZb");
        e.insert(&"桜".repeat(MAX_INPUT_BYTES));
        assert!(e.text.len() <= MAX_INPUT_BYTES);
        assert!(e.text.is_char_boundary(e.cursor));
    }

    #[test]
    fn terminal_controls_and_bidi_overrides_never_reach_the_screen() {
        let result = clean("\x1b]52;c;payload\x07\r\nhello\u{202e}world\u{009b}");
        assert!(!result.chars().any(char::is_control));
        assert!(!result.contains('\u{202e}'));
        assert_eq!(clean(&"桜".repeat(3000)).len(), 2046);
    }

    #[test]
    fn joins_are_membership_checked_and_wait_for_server_confirmation() {
        let mut model = Model::default();
        assert!(matches!(submit(&mut model, "/join 1"), Action::None));
        model.event(Event::Groups(groups()));
        assert!(matches!(submit(&mut model, "/join Demo group"), Action::Send(Command::Join(_))));
        assert!(matches!(submit(&mut model, "not yet"), Action::None));
        model.event(Event::Joined(Uuid::from_u128(1)));
        assert!(matches!(submit(&mut model, "hello"), Action::Send(Command::Group { .. })));
        assert!(matches!(submit(&mut model, "/part"), Action::Send(Command::Leave(_))));
        assert_eq!(model.groups.len(), 1);
        model.event(Event::Left(Uuid::from_u128(1)));
        assert_eq!(model.buffers.len(), 1);
        assert_eq!(model.groups.len(), 1);
    }

    #[test]
    fn unread_drafts_and_history_are_bounded_and_independent() {
        let mut model = Model::default();
        model.event(Event::Groups(groups()));
        model.active_mut().draft.insert("local draft");
        model.event(Event::Joined(Uuid::from_u128(1)));
        for i in 0..1000 {
            model.event(Event::Group {
                id: Uuid::from_u128(1),
                sender: None,
                from: "someone".into(),
                text: format!("{i}"),
            });
        }
        assert_eq!(model.current, 0);
        assert_eq!(model.buffers[1].lines.len(), MAX_LINES);
        assert_eq!(model.buffers[1].unread, 1000);
        model.switch(1);
        assert_eq!(model.active().unread, 0);
        assert!(model.active().draft.text.is_empty());
        model.switch(-1);
        assert_eq!(&*model.active().draft.text, "local draft");
    }

    #[test]
    fn background_session_notices_do_not_count_as_unread_chat() {
        let mut model = Model::default();
        let id = Uuid::from_u128(1);
        model.event(Event::Groups(groups()));
        model.event(Event::Joined(id));
        assert_eq!(model.current, 0);
        assert_eq!(model.buffers[1].unread, 0);
        assert!(
            model
                .format_entry(model.buffers[1].lines.back().unwrap())
                .contains(&*tr!("tui-session-ready"))
        );
        model.event(Event::Group {
            id,
            sender: None,
            from: "Peer".into(),
            text: "hello".into(),
        });
        assert_eq!(model.buffers[1].unread, 1);
        model.event(Event::Joined(id));
        assert_eq!(model.buffers[1].unread, 1);
    }

    #[test]
    fn commands_do_not_accidentally_send_to_the_wrong_chat() {
        let mut model = Model::default();
        assert!(matches!(
            submit(&mut model, "/shout hello"),
            Action::Send(Command::Local { kind: 2, .. })
        ));
        assert!(matches!(submit(&mut model, "//quit"), Action::Send(Command::Local { text, .. }) if text == "/quit"));
        assert!(matches!(submit(&mut model, "/unknown"), Action::None));
        assert!(matches!(submit(&mut model, "/join 0"), Action::None));
        assert!(matches!(submit(&mut model, "/quit"), Action::Quit));
    }

    fn residents(model: &mut Model) {
        model.event(Event::Names(vec![
            ResidentName {
                id: Uuid::from_u128(11),
                display_name: "Sakura".into(),
                legacy_name: "Sakura Example".into(),
            },
            ResidentName {
                id: Uuid::from_u128(12),
                display_name: "River".into(),
                legacy_name: "River Example".into(),
            },
        ]));
        model.event(Event::Friends(vec![
            Friend {
                id: Uuid::from_u128(11),
                online: true,
            },
            Friend {
                id: Uuid::from_u128(12),
                online: false,
            },
        ]));
    }

    #[test]
    fn channels_accept_attached_text_and_signed_i32_without_leaking_to_other_buffers() {
        let mut model = Model::default();
        for (input, channel, expected) in [
            ("/3 roll", 3, "roll"),
            ("/-3roll", -3, "roll"),
            ("/0 hello", 0, "hello"),
            ("/-2147483648 min", i32::MIN, "min"),
            ("/2147483646 max", i32::MAX - 1, "max"),
        ] {
            assert!(
                matches!(submit(&mut model, input), Action::Send(Command::Local { channel: actual, kind: 1, text }) if actual == channel && text == expected)
            );
        }
        for input in ["/2147483647 debug", "/2147483648 overflow", "/-2147483649 overflow", "/3", "/-oops"] {
            assert!(matches!(submit(&mut model, input), Action::None), "{input}");
            assert!(
                model
                    .format_entry(model.active().lines.back().unwrap())
                    .contains(&*tr!("tui-channel-usage"))
            );
        }
        model.event(Event::Groups(groups()));
        model.event(Event::Joined(Uuid::from_u128(1)));
        submit(&mut model, "/join 1");
        assert!(matches!(submit(&mut model, "/3 roll"), Action::None));
        residents(&mut model);
        submit(&mut model, "/query 1");
        assert!(matches!(submit(&mut model, "/-3roll"), Action::None));
        assert!(matches!(submit(&mut model, "//3 roll"), Action::Send(Command::Direct { text, .. }) if text == "/3 roll"));
        assert!(matches!(submit(&mut model, "/me waves"), Action::Send(Command::Direct { text, .. }) if text == "/me waves"));
    }

    #[test]
    fn late_names_resolve_history_but_never_rename_objects() {
        let mut model = Model::default();
        let sender = Some(Uuid::from_u128(11));
        model.event(Event::Local {
            sender,
            from: "Sakura Example".into(),
            text: "/me waves".into(),
        });
        model.event(Event::Local {
            sender: None,
            from: "Sakura Example".into(),
            text: "Object says hello".into(),
        });
        let original = model.format_entry(&model.buffers[0].lines[0]);
        assert!(original.ends_with("] Sakura Example: /me waves"));
        assert_eq!(&original[..1], "[");
        assert_eq!(original.as_bytes()[5], b'/');
        assert_eq!(original.as_bytes()[8], b'/');
        assert_eq!(original.as_bytes()[11], b' ');
        assert_eq!(original.as_bytes()[14], b':');
        residents(&mut model);
        assert!(
            model
                .format_entry(&model.buffers[0].lines[0])
                .ends_with("] Sakura (Sakura Example): /me waves")
        );
        assert!(
            model
                .format_entry(&model.buffers[0].lines[1])
                .ends_with("] Sakura Example: Object says hello")
        );
        model.event(Event::Names(vec![ResidentName {
            id: Uuid::from_u128(11),
            display_name: "Same".into(),
            legacy_name: "Same".into(),
        }]));
        assert!(model.format_entry(&model.buffers[0].lines[0]).ends_with("] Same: /me waves"));
    }

    #[test]
    fn direct_recipients_accept_friend_indices_uuids_and_exact_full_names() {
        let mut model = Model::default();
        residents(&mut model);
        for input in [
            "/msg 1 hello",
            "/im Sakura Example hello",
            "/msg sakura hello",
            "/msg 00000000-0000-0000-0000-00000000000b hello",
        ] {
            assert!(
                matches!(submit(&mut model, input), Action::Send(Command::Direct { id, text }) if id == Uuid::from_u128(11) && text == "hello")
            );
        }
        for input in ["/query missing", "/msg 1", "/msg Saku hello", "/query 0"] {
            assert!(matches!(submit(&mut model, input), Action::None));
        }
        submit(&mut model, "/query River Example");
        assert_eq!(model.active().chat, Chat::Direct(Uuid::from_u128(12)));
        assert!(
            matches!(submit(&mut model, "hello offline friend"), Action::Send(Command::Direct { id, .. }) if id == Uuid::from_u128(12))
        );
    }

    #[test]
    fn ambiguous_recipient_does_not_fall_back_to_a_shorter_name() {
        let mut model = Model::default();
        model.event(Event::Names(vec![
            ResidentName {
                id: Uuid::from_u128(11),
                display_name: "A B".into(),
                legacy_name: "First Example".into(),
            },
            ResidentName {
                id: Uuid::from_u128(12),
                display_name: "A B".into(),
                legacy_name: "Second Example".into(),
            },
            ResidentName {
                id: Uuid::from_u128(13),
                display_name: "A".into(),
                legacy_name: "Third Example".into(),
            },
        ]));
        assert!(matches!(submit(&mut model, "/query A B"), Action::None));
        assert_eq!(model.buffers.len(), 1);
        assert!(matches!(submit(&mut model, "/msg A B secret"), Action::None));
        assert_eq!(model.buffers.len(), 1);
        assert!(
            matches!(submit(&mut model, "/msg First Example hello"), Action::Send(Command::Direct { id, .. }) if id == Uuid::from_u128(11))
        );
    }

    #[test]
    fn direct_and_group_sessions_never_collide_or_leave_the_wrong_session() {
        let mut model = Model::default();
        let id = Uuid::from_u128(1);
        model.event(Event::Groups(groups()));
        model.event(Event::Joined(id));
        model.event(Event::Direct {
            id,
            sender: id,
            from: "Peer".into(),
            text: "hello".into(),
        });
        assert_eq!(model.current, 0);
        assert_eq!(model.buffers.len(), 3);
        assert_eq!(model.buffers[2].unread, 1);
        submit(&mut model, &format!("/query {id}"));
        assert!(matches!(submit(&mut model, "/part"), Action::None));
        assert!(model.buffers.iter().any(|b| b.chat == Chat::Group(id)));
        submit(&mut model, &format!("/query {id}"));
        model.event(Event::Left(id));
        assert_eq!(model.active().chat, Chat::Direct(id));
        assert_eq!(model.buffers.len(), 2);
    }

    #[test]
    fn social_snapshots_names_and_direct_history_are_bounded() {
        let mut model = Model::default();
        model.event(Event::Names(
            (0..MAX_RESIDENTS + 2)
                .map(|i| ResidentName {
                    id: Uuid::from_u128(i as u128),
                    display_name: "Name".into(),
                    legacy_name: format!("Resident {i}"),
                })
                .collect(),
        ));
        model.event(Event::Friends(
            (0..MAX_RESIDENTS + 2)
                .map(|i| Friend {
                    id: Uuid::from_u128(i as u128),
                    online: false,
                })
                .collect(),
        ));
        model.event(Event::Nearby(
            (0..MAX_RESIDENTS + 2)
                .map(|i| Nearby {
                    id: Uuid::from_u128(i as u128),
                    distance: None,
                })
                .collect(),
        ));
        assert_eq!(model.names.len(), MAX_RESIDENTS);
        assert_eq!(model.friends.len(), MAX_RESIDENTS);
        assert_eq!(model.nearby.len(), MAX_RESIDENTS);
        for i in 0..MAX_LINES + 3 {
            model.event(Event::Direct {
                id: Uuid::from_u128(1),
                sender: Uuid::from_u128(1),
                from: "Name".into(),
                text: i.to_string(),
            });
        }
        assert_eq!(model.buffers[1].lines.len(), MAX_LINES);
        assert_eq!(model.buffers[1].lines[0].text, "3");
        for i in 2..MAX_SESSIONS + 3 {
            model.event(Event::Direct {
                id: Uuid::from_u128(i as u128),
                sender: Uuid::from_u128(i as u128),
                from: "Name".into(),
                text: "hello".into(),
            });
        }
        assert_eq!(model.buffers.len(), MAX_SESSIONS + 1);
        model.event(Event::Friends(Vec::new()));
        model.event(Event::Nearby(Vec::new()));
        assert!(model.friends.is_empty() && model.nearby.is_empty());
    }

    #[test]
    fn radar_never_claims_unknown_or_invalid_altitude_is_in_range() {
        let mut model = Model::default();
        for (distance, in_range) in [
            (Some(20.0), true),
            (Some(20.1), false),
            (None, false),
            (Some(f32::NAN), false),
            (Some(-1.0), false),
            (Some(f32::INFINITY), false),
        ] {
            model.event(Event::Nearby(vec![Nearby {
                id: Uuid::from_u128(1),
                distance,
            }]));
            let row = &model.radar_rows()[0];
            assert_eq!(row.contains(&*tr!("tui-radar-in-range", distance = "20.0")), in_range);
            if distance.is_none_or(|d| !d.is_finite() || d < 0.0) {
                assert!(row.contains(&*tr!("tui-radar-unknown")));
            }
        }
    }

    #[test]
    fn friend_list_shows_online_and_offline_without_sending() {
        let mut model = Model::default();
        residents(&mut model);
        assert!(matches!(submit(&mut model, "/friends"), Action::None));
        let lines: Vec<_> = model.active().lines.iter().map(|line| model.format_entry(line)).collect();
        assert!(
            lines
                .iter()
                .any(|line| line.contains("1: Sakura (Sakura Example)") && line.contains(&*tr!("tui-online")))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("2: River (River Example)") && line.contains(&*tr!("tui-offline")))
        );
        assert!(matches!(submit(&mut model, "/radar"), Action::None));
        assert!(matches!(submit(&mut model, "/nearby"), Action::None));
    }

    #[test]
    fn large_social_lists_can_be_paged_without_losing_first_indices_to_history_limits() {
        let mut model = Model::default();
        model.event(Event::Friends(
            (1..=250)
                .map(|id| Friend {
                    id: Uuid::from_u128(id),
                    online: true,
                })
                .collect(),
        ));
        assert!(matches!(submit(&mut model, "/friends"), Action::None));
        assert_eq!(model.active().lines.len(), 102);
        assert!(model.format_entry(&model.active().lines[1]).contains("1: "));
        submit(&mut model, "/friends 3");
        assert!(model.active().lines.iter().any(|entry| model.format_entry(entry).contains("250: ")));
        assert!(matches!(submit(&mut model, "/query 250"), Action::None));
        assert_eq!(model.active().chat, Chat::Direct(Uuid::from_u128(250)));
        for input in ["/friends 0", "/friends 4", "/radar -1", "/nearby invalid"] {
            assert!(matches!(submit(&mut model, input), Action::None));
            assert!(
                model
                    .format_entry(model.active().lines.back().unwrap())
                    .contains(&*tr!("tui-list-page-usage"))
            );
        }
    }

    #[test]
    fn names_are_terminal_safe_and_updates_do_not_increase_the_cap() {
        let mut model = Model::default();
        model.event(Event::Names(vec![ResidentName {
            id: Uuid::from_u128(11),
            display_name: "\x1b[31mSakura\u{202e}".into(),
            legacy_name: "Sakura\nExample".into(),
        }]));
        assert_eq!(model.resident_name(Uuid::from_u128(11)), "[31mSakura (SakuraExample)");
        model.event(Event::Names(vec![ResidentName {
            id: Uuid::from_u128(11),
            display_name: "桜".repeat(3000),
            legacy_name: "桜".repeat(3000),
        }]));
        assert_eq!(model.names.len(), 1);
        assert!(model.resident_name(Uuid::from_u128(11)).len() <= MAX_LINE_BYTES);
        assert!(matches!(submit(&mut model, "/query"), Action::None));
        assert_eq!(model.buffers.len(), 1);
    }

    #[test]
    fn name_cache_evicts_old_entries_but_empty_updates_preserve_resolved_display_names() {
        let mut model = Model::default();
        residents(&mut model);
        model.event(Event::Names(vec![ResidentName {
            id: Uuid::from_u128(11),
            display_name: String::new(),
            legacy_name: "Sakura Example".into(),
        }]));
        assert_eq!(model.resident_name(Uuid::from_u128(11)), "Sakura (Sakura Example)");
        assert_eq!(model.name_order.len(), 2);
        model.event(Event::Names(
            (100..100 + MAX_RESIDENTS)
                .map(|id| ResidentName {
                    id: Uuid::from_u128(id as u128),
                    display_name: format!("New {id}"),
                    legacy_name: format!("New{id} Resident"),
                })
                .collect(),
        ));
        assert_eq!(model.names.len(), MAX_RESIDENTS);
        assert_eq!(model.name_order.len(), MAX_RESIDENTS);
        assert!(!model.names.contains_key(&Uuid::from_u128(11)));
        assert!(model.names.contains_key(&Uuid::from_u128((99 + MAX_RESIDENTS) as u128)));
        model.event(Event::Names(vec![ResidentName {
            id: Uuid::from_u128(11),
            display_name: "桜".into(),
            legacy_name: "Sakura Example".into(),
        }]));
        assert_eq!(model.resident_name(Uuid::from_u128(11)), "桜 (Sakura Example)");
        assert_eq!(model.names.len(), MAX_RESIDENTS);
        assert_eq!(model.name_order.len(), MAX_RESIDENTS);
    }
}
