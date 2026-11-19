//! AgentUpdate control bits shared by graphical and chat sessions.

pub const AT_POS: u32 = 0x0000_0001;
pub const AT_NEG: u32 = 0x0000_0002;
pub const LEFT_POS: u32 = 0x0000_0004;
pub const LEFT_NEG: u32 = 0x0000_0008;
pub const UP_POS: u32 = 0x0000_0010;
pub const UP_NEG: u32 = 0x0000_0020;
pub const PITCH_POS: u32 = 0x0000_0040;
pub const PITCH_NEG: u32 = 0x0000_0080;
pub const YAW_POS: u32 = 0x0000_0100;
pub const YAW_NEG: u32 = 0x0000_0200;
pub const FAST_AT: u32 = 0x0000_0400;
pub const FAST_LEFT: u32 = 0x0000_0800;
pub const FAST_UP: u32 = 0x0000_1000;
pub const FLY: u32 = 0x0000_2000;
pub const STOP: u32 = 0x0000_4000;
pub const FINISH_ANIM: u32 = 0x0000_8000;
pub const STAND_UP: u32 = 0x0001_0000;
pub const SIT_ON_GROUND: u32 = 0x0002_0000;
pub const MOUSELOOK: u32 = 0x0004_0000;
pub const NUDGE_AT_POS: u32 = 0x0008_0000;
pub const NUDGE_AT_NEG: u32 = 0x0010_0000;
pub const NUDGE_LEFT_POS: u32 = 0x0020_0000;
pub const NUDGE_LEFT_NEG: u32 = 0x0040_0000;
pub const NUDGE_UP_POS: u32 = 0x0080_0000;
pub const NUDGE_UP_NEG: u32 = 0x0100_0000;
pub const TURN_LEFT: u32 = 0x0200_0000;
pub const TURN_RIGHT: u32 = 0x0400_0000;
pub const AWAY: u32 = 0x0800_0000;
/// Left button outside mouselook, and in mouselook when no script took
/// its mouselook bit (LLViewerInput::scanMouse, LLToolGrabBase).
pub const LBUTTON_DOWN: u32 = 0x1000_0000;
pub const LBUTTON_UP: u32 = 0x2000_0000;
/// Left button in mouselook, taken by a script (LLToolCompGun).
pub const ML_LBUTTON_DOWN: u32 = 0x4000_0000;
pub const ML_LBUTTON_UP: u32 = 0x8000_0000;
