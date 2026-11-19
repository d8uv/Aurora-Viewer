use aurora_i18n::tr;
use aurora_net::chat::{Command, Event, MAX_INPUT_BYTES, MAX_SESSIONS};
use aurora_net::social::GroupMembership;
use std::collections::VecDeque;
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

pub struct Buffer {
    pub id: Option<Uuid>,
    pub name: String,
    pub ready: bool,
    pub unread: usize,
    pub lines: VecDeque<String>,
    pub scroll: u16,
    pub draft: Editor,
}

impl Buffer {
    fn new(id: Option<Uuid>, name: String) -> Self {
        Self {
            id,
            name: clean(&name),
            ready: id.is_none(),
            unread: 0,
            lines: VecDeque::new(),
            scroll: 0,
            draft: Editor::default(),
        }
    }

    fn push(&mut self, line: String) {
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

pub struct Model {
    pub buffers: Vec<Buffer>,
    pub groups: Vec<GroupMembership>,
    pub current: usize,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            buffers: vec![Buffer::new(None, tr!("tui-local").into())],
            groups: Vec::new(),
            current: 0,
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
        self.active_mut().push(format!("* {}", clean(text)));
    }

    fn buffer(&mut self, id: Uuid) -> Option<usize> {
        if let Some(index) = self.buffers.iter().position(|b| b.id == Some(id)) {
            return Some(index);
        }
        if self.buffers.len() > MAX_SESSIONS {
            self.notice(tr!("tui-session-limit"));
            return None;
        }
        let group = self.groups.iter().find(|g| g.id == id)?;
        self.buffers.push(Buffer::new(Some(id), group.name.clone()));
        Some(self.buffers.len() - 1)
    }

    fn append(&mut self, index: usize, from: &str, text: &str) {
        // Split remote paragraphs into bounded rows; never retain an unbounded line.
        for line in text.lines().take(MAX_LINES) {
            let from = clean(from);
            let text = clean(line);
            self.buffers[index].push(clean(&format!("<{from}> {text}")));
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
                    if let Some(group) = self.groups.iter().find(|g| Some(g.id) == buffer.id) {
                        buffer.name = clean(&group.name);
                    }
                }
            }
            Event::GroupRemoved(id) => {
                self.groups.retain(|g| g.id != id);
                self.remove(id);
            }
            Event::Local { from, text } => self.append(0, &from, &text),
            Event::Group { id, from, text } => {
                if let Some(index) = self.buffer(id) {
                    self.append(index, &from, &text);
                }
            }
            Event::Joined(id) => {
                if let Some(index) = self.buffer(id) {
                    self.buffers[index].ready = true;
                    self.buffers[index].push(format!("* {}", tr!("tui-session-ready")));
                }
            }
            Event::Left(id) => self.remove(id),
            Event::Notice(text) => self.notice(&text),
        }
    }

    fn remove(&mut self, id: Uuid) {
        if let Some(index) = self.buffers.iter().position(|b| b.id == Some(id)) {
            self.buffers.remove(index);
            if self.current >= index {
                self.current = self.current.saturating_sub(1);
            }
            self.active_mut().unread = 0;
        }
    }

    pub fn submit(&mut self) -> Action {
        let input = self.active_mut().draft.take();
        let input = input.trim();
        if input.is_empty() {
            return Action::None;
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
                    if let Some(index) = self.buffer(id) {
                        self.current = index;
                        self.active_mut().unread = 0;
                        return Action::Send(Command::Join(id));
                    }
                } else {
                    self.notice(tr!("tui-join-usage"));
                }
                Action::None
            }
            "/part" => {
                if let Some(id) = self.active().id {
                    Action::Send(Command::Leave(id))
                } else {
                    self.notice(tr!("tui-part-local"));
                    Action::None
                }
            }
            "/whisper" | "/shout" => {
                if self.active().id.is_some() || argument.is_empty() {
                    self.notice(tr!("tui-local-command"));
                    Action::None
                } else {
                    Action::Send(Command::Local {
                        text: argument.into(),
                        kind: if command == "/whisper" { 0 } else { 2 },
                    })
                }
            }
            _ if input.starts_with('/') && !input.starts_with("//") && command != "/me" => {
                self.notice(tr!("tui-unknown-command"));
                Action::None
            }
            _ => {
                let text = input.strip_prefix("//").map_or(input, |_| &input[1..]).to_owned();
                if let Some(id) = self.active().id {
                    if self.active().ready {
                        Action::Send(Command::Group { id, text })
                    } else {
                        self.notice(tr!("tui-group-not-ready"));
                        Action::None
                    }
                } else {
                    Action::Send(Command::Local { text, kind: 1 })
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
}
