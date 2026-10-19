//! DJ controllers, as bytes in and bytes out.
//!
//! Pure on purpose: no MIDI ports, no threads, no engine. The host hands each
//! incoming message to a profile and gets back an [`Intent`] — what the
//! hardware asks the mixer or a deck for — and hands the decks' state to the
//! profile to get back the LED messages that show it. Everything here can be
//! tested with byte arrays.

use serde::Serialize;

pub mod flx4;

/// Which deck a control belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Deck {
    A,
    B,
}

impl Deck {
    pub const ALL: [Deck; 2] = [Deck::A, Deck::B];

    #[must_use]
    pub fn index(self) -> usize {
        match self {
            Deck::A => 0,
            Deck::B => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Band {
    High,
    Mid,
    Low,
}

/// A deck button, with the SHIFT layer already resolved by the hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Button {
    Play,
    ShiftPlay,
    Cue,
    /// SHIFT + CUE: back to the start of the track.
    TrackStart,
    Sync,
    /// BEAT SYNC held: this deck becomes the sync master.
    SyncHold,
    /// SHIFT + BEAT SYNC: the next tempo range.
    TempoRange,
    LoopIn,
    LoopOut,
    /// SHIFT + LOOP IN / OUT: move that loop point with the jog.
    ShiftLoopIn,
    ShiftLoopOut,
    Reloop,
    ShiftReloop,
    CallPrevious,
    CallNext,
    ShiftCallPrevious,
    ShiftCallNext,
    /// SHIFT + the channel's CUE button.
    Quantize,
}

/// What the pads are doing. The hardware switches modes itself and says
/// which in every pad message, so nothing here has to track it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PadMode {
    HotCue,
    PadFx1,
    BeatJump,
    Sampler,
    Keyboard,
    PadFx2,
    BeatLoop,
    KeyShift,
}

/// What a turn of the jog wheel means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JogMode {
    /// The platter, touched: the record under the hand.
    Scratch,
    /// The side, or the platter untouched: a pitch bend.
    Bend,
    /// SHIFT and the wheel: a fast search.
    Search,
}

/// What a control asks for. Positions are 0 to 1 unless said otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Intent {
    /// The engine's trim, 0 to 2: the knob's centre is unity.
    Trim { deck: Deck, value: f32 },
    Eq { deck: Deck, band: Band, value: f32 },
    /// CFX: 0.5 is off, below is a low pass, above a high pass.
    Filter { deck: Deck, value: f32 },
    Volume { deck: Deck, value: f32 },
    /// 0 is deck A alone, 1 is deck B alone.
    Crossfader { value: f32 },
    /// 0 is the cue alone, 1 is the master alone.
    HeadphoneMix { value: f32 },
    /// The channel's CUE button was pressed: toggle the headphone send.
    ChannelCue { deck: Deck },
    /// The tempo fader, −1 (slowest) to 1 (fastest) within the deck's range.
    Tempo { deck: Deck, value: f32 },
    Button { deck: Deck, button: Button, pressed: bool },
    /// `pad` is 0 to 7, left to right and top row first.
    Pad { deck: Deck, mode: PadMode, pad: u8, shift: bool, pressed: bool },
    /// A pad mode button: what the pads do from now on, for the screen to show.
    PadMode { deck: Deck, mode: PadMode },
    JogTouch { deck: Deck, touched: bool },
    /// Signed, in the wheel's own ticks; positive is clockwise.
    Jog { deck: Deck, mode: JogMode, ticks: i32 },
    /// Rows to move the browser's cursor; positive is down.
    Browse { steps: i32 },
    BrowsePress { shift: bool },
    Load { deck: Deck },
    /// BEAT FX SELECT: the next effect, or with SHIFT the previous one.
    FxSelect { previous: bool },
    /// BEAT ◀ ▶: a shorter or a longer beat.
    FxBeat { longer: bool },
    /// BEAT FX ON/OFF: toggles the unit; with SHIFT, off.
    FxToggle { off: bool },
    /// The CH SELECT lever: whether a channel is selected. Both is the master.
    FxChannel { deck: Deck, selected: bool },
    /// LEVEL/DEPTH.
    FxDepth { value: f32 },
    /// MASTER CUE: toggles the master onto the headphones.
    MasterCue,
    /// SMART CFX: toggles the CFX knobs' combined effect.
    SmartCfx,
}

impl Intent {
    /// Whether the host applies this straight to the engine, without a round
    /// trip through the interface: the continuous mixer controls, where
    /// latency is felt. The jog wheels are the host's to route.
    #[must_use]
    pub fn is_direct(&self) -> bool {
        matches!(
            self,
            Intent::Trim { .. }
                | Intent::Eq { .. }
                | Intent::Filter { .. }
                | Intent::Volume { .. }
                | Intent::Crossfader { .. }
                | Intent::HeadphoneMix { .. }
                | Intent::MasterCue
                | Intent::SmartCfx
        )
    }
}

/// What a deck's LEDs should show.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools, reason = "one bool per LED is what the hardware has")]
pub struct DeckLeds {
    pub play: bool,
    pub cue: bool,
    pub sync: bool,
    pub looping: bool,
    pub channel_cue: bool,
    /// Which of the eight hot cues are set.
    pub hot_cues: [bool; 8],
    /// The pad mode button that is lit; 0 for none.
    pub pad_mode: u8,
    /// Which pads are held down, for the PAD FX modes' LEDs.
    pub held_pads: [bool; 8],
    /// The half-second blink an active loop's buttons follow.
    pub blink: bool,
    /// The loop point the jog moves: 0 none, 1 the in point, 2 the out point.
    pub loop_adjust: u8,
    /// The channel's level before its fader, 0 to 127.
    pub level: u8,
}

/// One three-byte message for the controller.
pub type Short = [u8; 3];
