//! Bounded protocol counters, never packet contents or remote identifiers.

use super::Session;
use aurora_msg::msgs::{AvatarAnimation, ScriptControlChange};
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::time::Instant;

// LLAgent::onAnimStop (indra/newview/llagent.cpp) signals completion of
// these states. Names/IDs: indra/llcharacter/llanimationstates.cpp.
const TRANSITIONS: [(u128, &str); 4] = [
    (0x3da1d753_028a_5446_24f3_9c9b856d9422, "standup"),
    (0x7a4e87fe_de39_6fcb_6223_024b00893244, "prejump"),
    (0x7a17b059_12b2_41b1_570a_186368b6aa6f, "land"),
    (0xf4f00d6e_b9fe_9292_f4cb_0ae06ea58d57, "medium_land"),
];

#[derive(Default)]
pub(super) struct State {
    datagrams: u64,
    channel_sent: u64,
    channel_acknowledged: u64,
    // u8 keys bound each histogram to at most 256 entries.
    instant_messages: BTreeMap<u8, u64>,
    local_chats: BTreeMap<u8, u64>,
    local_filtered: u64,
    script_questions: u64,
    permission_bits_seen: u32,
    experience_questions: u64,
    script_dialogs: u64,
    control_packets: u64,
    controls_taken: u64,
    controls_released: u64,
    animation_packets: u64,
    animation_count: usize,
    animation_sources: usize,
    transition_bits: u8,
    animation_at: Option<Instant>,
}

impl State {
    pub(super) fn datagram_received(&mut self) {
        self.datagrams = self.datagrams.saturating_add(1);
    }

    pub(super) fn channel_sent(&mut self) {
        self.channel_sent = self.channel_sent.saturating_add(1);
    }

    pub(super) fn channel_acknowledged(&mut self) {
        self.channel_acknowledged = self.channel_acknowledged.saturating_add(1);
    }

    pub(super) fn instant_message(&mut self, dialog: u8) {
        let count = self.instant_messages.entry(dialog).or_default();
        *count = count.saturating_add(1);
    }

    pub(super) fn local_chat(&mut self, kind: u8, shown: bool) {
        let count = self.local_chats.entry(kind).or_default();
        *count = count.saturating_add(1);
        if !shown {
            self.local_filtered = self.local_filtered.saturating_add(1);
        }
    }

    pub(super) fn script_question(&mut self, questions: i32, experience: bool) -> bool {
        self.script_questions = self.script_questions.saturating_add(1);
        self.permission_bits_seen |= questions as u32;
        if experience {
            self.experience_questions = self.experience_questions.saturating_add(1);
        }
        self.script_questions == 1
    }

    pub(super) fn script_dialog(&mut self) -> bool {
        self.script_dialogs = self.script_dialogs.saturating_add(1);
        self.script_dialogs == 1
    }

    pub(super) fn script_controls(&mut self, message: &ScriptControlChange) {
        self.control_packets = self.control_packets.saturating_add(1);
        for data in &message.data {
            if data.take_controls {
                self.controls_taken = self.controls_taken.saturating_add(1);
            } else {
                self.controls_released = self.controls_released.saturating_add(1);
            }
        }
    }

    pub(super) fn self_animations(&mut self, message: &AvatarAnimation) {
        self.animation_packets = self.animation_packets.saturating_add(1);
        self.animation_at = Some(Instant::now());
        self.animation_count = message.animation_list.len();
        self.animation_sources = message.animation_source_list.len();
        self.transition_bits = 0;
        for animation in &message.animation_list {
            if let Some(index) = TRANSITIONS.iter().position(|(id, _)| *id == animation.anim_id.as_u128()) {
                self.transition_bits |= 1 << index;
            }
        }
    }

    fn lines(&self) -> Vec<String> {
        let transitions: Vec<_> = TRANSITIONS
            .iter()
            .enumerate()
            .filter_map(|(i, (_, name))| (self.transition_bits & (1 << i) != 0).then_some(*name))
            .collect();
        let animation_age = self
            .animation_at
            .map_or_else(|| "none".into(), |at| at.elapsed().as_secs().to_string());
        let mut lines = vec![
            format!("received_datagrams={}", self.datagrams),
            format!(
                "script_channel_packets_sent={} acknowledged={}",
                self.channel_sent, self.channel_acknowledged
            ),
            format!("local_chat_filtered={}", self.local_filtered),
            format!(
                "script_questions={} permission_bits_seen=0x{:08x} experience_questions={} answers_sent=0",
                self.script_questions, self.permission_bits_seen, self.experience_questions
            ),
            format!("script_dialogs={} replies_sent=0", self.script_dialogs),
            format!(
                "script_control_packets={} take_blocks={} release_blocks={}",
                self.control_packets, self.controls_taken, self.controls_released
            ),
            format!(
                "self_animation_packets={} latest_animations={} latest_sources={} last_update_age_seconds={animation_age}",
                self.animation_packets, self.animation_count, self.animation_sources
            ),
            format!(
                "self_transition_states={}",
                if transitions.is_empty() {
                    "none".into()
                } else {
                    transitions.join(",")
                }
            ),
            "animation_playback=disabled appearance_loading=disabled".into(),
        ];
        for (label, values) in [
            ("received_im_dialogs", &self.instant_messages),
            ("received_local_chat_types", &self.local_chats),
        ] {
            let rows: Vec<_> = values.iter().map(|(key, value)| format!("{key}={value}")).collect();
            if rows.is_empty() {
                lines.push(format!("{label}: none"));
            } else {
                for chunk in rows.chunks(32) {
                    lines.push(format!("{label}: {}", chunk.join(",")));
                }
            }
        }
        lines
    }
}

impl Session {
    pub(super) fn diagnostic_report(&self) -> Vec<String> {
        let stats = self.stats.snapshot();
        let mut lines = vec![
            format!("aurora_chat={} diagnostics=1", aurora_version::version()),
            format!("configured_task_bits_per_second={}", super::CHAT_THROTTLES[4]),
            format!(
                "arrived={} last_packet_age_seconds={} reliable_pending={} ping_ms={}",
                self.arrived,
                self.circuit.last_recv.elapsed().as_secs(),
                self.circuit.unacked_count(),
                stats.ping_ms
            ),
            format!(
                "packets_sent={} retransmits={} send_errors={} malformed_packets={}",
                stats.packets_out,
                stats.resent,
                self.stats.send_errors.load(Ordering::Relaxed),
                self.stats.decode_errors.load(Ordering::Relaxed),
            ),
        ];
        lines.extend(self.diagnostics.lines());
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aurora_msg::msgs::avatar_animation::{AnimationList, AnimationSourceList};
    use uuid::Uuid;

    #[test]
    fn diagnostic_histograms_and_output_remain_bounded() {
        let mut state = State::default();
        for value in 0..=u8::MAX {
            state.instant_messages.insert(value, u64::MAX);
            state.local_chats.insert(value, u64::MAX);
            state.instant_message(value);
            state.local_chat(value, false);
        }
        let lines = state.lines();
        assert!(lines.len() < 32);
        assert!(lines.iter().all(|line| line.len() < 2048));
        assert!(lines.iter().any(|line| line.contains("255=18446744073709551615")));
    }

    #[test]
    fn animation_diagnostics_classify_only_known_transitions_and_forget_old_state() {
        let mut state = State::default();
        let mut animation = AvatarAnimation::default();
        let private_asset = Uuid::from_u128(123456);
        animation.animation_list = vec![
            AnimationList {
                anim_id: Uuid::from_u128(TRANSITIONS[0].0),
                anim_sequence_id: 1,
            },
            AnimationList {
                anim_id: private_asset,
                anim_sequence_id: 2,
            },
        ];
        animation.animation_source_list = vec![AnimationSourceList { object_id: private_asset }];
        state.self_animations(&animation);
        let report = state.lines().join("\n");
        assert!(report.contains("latest_animations=2 latest_sources=1"));
        assert!(report.contains("self_transition_states=standup"));
        assert!(!report.contains(&private_asset.to_string()));
        assert!(!report.contains(&Uuid::from_u128(TRANSITIONS[0].0).to_string()));
        animation.animation_list.clear();
        animation.animation_source_list.clear();
        state.self_animations(&animation);
        assert!(state.lines().iter().any(|line| line == "self_transition_states=none"));
        assert!(
            state
                .lines()
                .iter()
                .any(|line| line.contains("latest_animations=0 latest_sources=0"))
        );
    }
}
