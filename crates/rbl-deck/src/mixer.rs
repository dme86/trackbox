//! The channel strip: trim, three bands, and the crossfader.
//!
//! One of these per deck, run inside the audio callback. Everything the
//! interface can move is an atomic — the callback must never wait for a knob —
//! and everything the callback applies goes through `smooth`, because a gain
//! that jumps between buffers is a click whatever it is a gain of.
//!
//! The bands are a Linkwitz-Riley four-pole split, which is what sums back to
//! flat: a two-pole split leaves a 3 dB hump at the crossover, and a DJ EQ
//! with every band at centre has to be the track. The low band is run through
//! the second crossover as well and its two halves summed, which is that
//! crossover's all-pass — without it the low band arrives phase-rotated
//! against the other two and the sum dips where they meet.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::latency::{ControlLatency, LatencyMeter, Stamp};
use crate::padfx::{smart_amount, PadFx, PadFxKind, PadSettings, SmartCfxKind, SmartFx};
use crate::smooth::Smoothed;

/// Where the low band ends and the middle begins.
///
/// **[ASSUME]** The DJM-900NXS2 manual quotes the frequencies its EQ is
/// *measured* at — HI 30 kHz, MID 1 kHz, LOW 20 Hz — not where its crossovers
/// sit, and no capture here says what those are. 300 Hz and 4 kHz are the
/// usual places for a three-band DJ EQ: 300 keeps a kick whole in the low band
/// and 4 k puts hats and air in the high one. Settle it by sweeping a real
/// DJM, or a recording of one, before anything depends on the exact number.
const CROSS_LOW_HZ: f32 = 300.0;
const CROSS_HIGH_HZ: f32 = 4_000.0;

/// The most a band can be boosted, in dB. Both curves share it.
const MAX_BOOST_DB: f32 = 6.0;

/// The least a band can be cut in EQ, in dB. ISOLATOR goes to silence instead.
const MIN_EQ_DB: f32 = -26.0;

/// What the three bands do at the ends of their travel.
///
/// The difference is the bottom of the range and nothing else: EQ bottoms out
/// at −26 dB, which still lets a little of the band through, and ISOLATOR
/// bottoms out at silence. Both top out at +6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Curve {
    Eq,
    Isolator,
}

impl Curve {
    fn from_wire(value: u32) -> Self {
        if value == 1 { Self::Isolator } else { Self::Eq }
    }

    fn to_wire(self) -> u32 {
        match self {
            Self::Eq => 0,
            Self::Isolator => 1,
        }
    }

    /// The gain a band knob at `position` (0 to 1) asks for.
    ///
    /// Centre is unity in both curves, which is what makes a knob at twelve
    /// o'clock the track and not an opinion about it.
    fn gain(self, position: f32) -> f32 {
        let at = position.clamp(0.0, 1.0);
        if at >= 0.5 {
            db_to_gain(MAX_BOOST_DB * (at - 0.5) * 2.0)
        } else {
            match self {
                Self::Eq => db_to_gain(MIN_EQ_DB * (0.5 - at) * 2.0),
                // Straight to silence rather than through −26: an isolator's
                // whole point is that the band goes away.
                Self::Isolator => at * 2.0,
            }
        }
    }

    /// What a kill button drives its band to: the bottom of this curve.
    ///
    /// A kill is not an attenuation of its own — it is the knob at its
    /// minimum, so switching curve changes what killing does, which is what
    /// the mixer it is a clone of does.
    fn killed(self) -> f32 {
        self.gain(0.0)
    }
}

fn db_to_gain(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

/// One deck's settings, as the interface leaves them for the callback.
///
/// Every field is an atomic and none of them is ever read as a group, so a
/// knob moved between two of the callback's reads simply lands on the next
/// frame — which is what a knob does.
#[derive(Debug)]
pub struct ChannelSettings {
    /// The trim, 0 to 2 — up to +6 dB, as the gain knob on a mixer gives.
    trim: AtomicU32,
    /// The post-EQ channel level (0..1), independent of input trim and crossfader.
    volume: AtomicU32,
    /// The three band knobs, 0 to 1, centre at 0.5.
    bands: [AtomicU32; 3],
    /// Which of them are killed.
    kills: [AtomicBool; 3],
    /// The channel's CUE button: whether it is heard on the headphones.
    cue: AtomicBool,
    /// CFX, 0 to 1: centre is off, left a low pass, right a high pass.
    filter: AtomicU32,
    /// The loudest sample after the strip since the last read, for a meter.
    level: AtomicU32,
    /// The same peak, held for the interface's meter: its own slot, so the
    /// interface and a controller's LEDs never clear each other's reading.
    meter: AtomicU32,
    /// PAD FX: held while a pad is. Not carried to a rebuilt engine.
    pad: PadSettings,
    changed: Stamp,
}

impl Default for ChannelSettings {
    fn default() -> Self {
        Self {
            trim: AtomicU32::new(1.0_f32.to_bits()),
            volume: AtomicU32::new(1.0_f32.to_bits()),
            bands: [
                AtomicU32::new(0.5_f32.to_bits()),
                AtomicU32::new(0.5_f32.to_bits()),
                AtomicU32::new(0.5_f32.to_bits()),
            ],
            kills: [AtomicBool::new(false), AtomicBool::new(false), AtomicBool::new(false)],
            cue: AtomicBool::new(false),
            filter: AtomicU32::new(0.5_f32.to_bits()),
            level: AtomicU32::new(0),
            meter: AtomicU32::new(0),
            pad: PadSettings::default(),
            changed: Stamp::default(),
        }
    }
}

/// Which band a control names. High to low, as the strip is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    High,
    Mid,
    Low,
}

impl Band {
    fn index(self) -> usize {
        match self {
            Self::High => 0,
            Self::Mid => 1,
            Self::Low => 2,
        }
    }

    pub const ALL: [Band; 3] = [Band::High, Band::Mid, Band::Low];
}

impl ChannelSettings {
    pub fn set_trim(&self, trim: f32) {
        let safe = if trim.is_finite() { trim.clamp(0.0, 2.0) } else { 1.0 };
        self.trim.store(safe.to_bits(), Ordering::Relaxed);
        self.changed.mark();
    }

    pub fn trim(&self) -> f32 {
        f32::from_bits(self.trim.load(Ordering::Relaxed))
    }

    pub fn set_volume(&self, volume: f32) {
        let safe = if volume.is_finite() { volume.clamp(0.0, 1.0) } else { 1.0 };
        self.volume.store(safe.to_bits(), Ordering::Relaxed);
        self.changed.mark();
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }

    pub fn set_band(&self, band: Band, position: f32) {
        let safe = if position.is_finite() { position.clamp(0.0, 1.0) } else { 0.5 };
        if let Some(slot) = self.bands.get(band.index()) {
            slot.store(safe.to_bits(), Ordering::Relaxed);
        }
        self.changed.mark();
    }

    pub fn band(&self, band: Band) -> f32 {
        self.bands.get(band.index()).map_or(0.5, |slot| f32::from_bits(slot.load(Ordering::Relaxed)))
    }

    pub fn set_kill(&self, band: Band, killed: bool) {
        if let Some(slot) = self.kills.get(band.index()) {
            slot.store(killed, Ordering::Relaxed);
        }
        self.changed.mark();
    }

    pub fn killed(&self, band: Band) -> bool {
        self.kills.get(band.index()).is_some_and(|slot| slot.load(Ordering::Relaxed))
    }

    /// Sends the channel to the headphones, pre-fader, as a DJM's CUE does.
    pub fn set_cue(&self, on: bool) {
        self.cue.store(on, Ordering::Relaxed);
        self.changed.mark();
    }

    pub fn cue(&self) -> bool {
        self.cue.load(Ordering::Relaxed)
    }

    pub fn set_filter(&self, position: f32) {
        let safe = if position.is_finite() { position.clamp(0.0, 1.0) } else { 0.5 };
        self.filter.store(safe.to_bits(), Ordering::Relaxed);
        self.changed.mark();
    }

    pub fn filter(&self) -> f32 {
        f32::from_bits(self.filter.load(Ordering::Relaxed))
    }

    /// A pad effect while its pad is held, `beats` long at `beat_ms` a beat;
    /// `None` lets go and lets echo and reverb ring out.
    pub fn set_pad_fx(&self, fx: Option<(PadFxKind, f32)>, beat_ms: f32) {
        self.pad.set(fx, beat_ms);
        self.changed.mark();
    }

    pub fn pad_fx(&self) -> Option<(PadFxKind, f32)> {
        self.pad.get()
    }

    /// Held rather than replaced, so a peak between two reads is not lost.
    pub(crate) fn report_level(&self, peak: f32) {
        for slot in [&self.level, &self.meter] {
            let mut current = slot.load(Ordering::Relaxed);
            while peak > f32::from_bits(current) {
                match slot.compare_exchange_weak(current, peak.to_bits(), Ordering::Relaxed, Ordering::Relaxed) {
                    Ok(_) => break,
                    Err(seen) => current = seen,
                }
            }
        }
    }

    /// The channel's peak before its fader since the last read, and clears it.
    /// The controller's reading; the interface has `take_meter`.
    pub fn take_level(&self) -> f32 {
        f32::from_bits(self.level.swap(0, Ordering::Relaxed))
    }

    /// The same peak for the interface's channel meter, cleared independently
    /// of `take_level`.
    pub fn take_meter(&self) -> f32 {
        f32::from_bits(self.meter.swap(0, Ordering::Relaxed))
    }

    /// The gain each band is asking for, high to low.
    fn wanted(&self, curve: Curve) -> [f32; 3] {
        let mut out = [1.0_f32; 3];
        for (i, slot) in out.iter_mut().enumerate() {
            let band = Band::ALL.get(i).copied().unwrap_or(Band::High);
            *slot = if self.killed(band) { curve.killed() } else { curve.gain(self.band(band)) };
        }
        out
    }
}

/// The shape of the crossfader.
///
/// **[UNKNOWN]** What rekordbox's own crossfader does between its ends has not
/// been measured. `Full` is the default because it is the one shape that
/// cannot be wrong in the one-player layout, which has no crossfader at all:
/// it leaves both decks at unity in the middle, so a deck nobody has touched a
/// fader for plays at the level of its file. Settle the real shape against
/// rekordbox before offering the choice as a preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fade {
    /// Unity across the middle, falling to silence only over the far half.
    Full,
    /// Constant power: −3 dB in the middle, so two tracks playing together do
    /// not jump as the fader passes.
    Power,
}

impl Fade {
    fn from_wire(value: u32) -> Self {
        if value == 1 { Self::Power } else { Self::Full }
    }

    fn to_wire(self) -> u32 {
        match self {
            Self::Full => 0,
            Self::Power => 1,
        }
    }

    /// What deck A and deck B are multiplied by at `at`, 0 at A and 1 at B.
    fn split(self, at: f32) -> (f32, f32) {
        let at = at.clamp(0.0, 1.0);
        match self {
            Self::Full => (((1.0 - at) * 2.0).min(1.0), (at * 2.0).min(1.0)),
            Self::Power => {
                let angle = at * std::f32::consts::FRAC_PI_2;
                (angle.cos(), angle.sin())
            }
        }
    }
}

/// The mixer's own settings: the curve both channels use, and the crossfader.
#[derive(Debug)]
pub struct MixerSettings {
    pub channels: [ChannelSettings; 2],
    /// 0 is deck A alone, 1 is deck B alone, 0.5 is both.
    position: AtomicU32,
    curve: AtomicU32,
    fade: AtomicU32,
    /// HEADPHONES MIX: 0 is the cue alone, 1 is the master alone.
    headphone_mix: AtomicU32,
    /// Off is THRU: both decks at their channel faders, whatever the crossfader says.
    crossfader_on: AtomicBool,
    /// MASTER CUE: the master on the headphones, as if it were cued.
    master_cue: AtomicBool,
    /// SMART CFX: the CFX knobs drive an effect along with the filter.
    smart_on: AtomicBool,
    smart_kind: AtomicU32,
    changed: Stamp,
    latency: LatencyMeter,
}

impl Default for MixerSettings {
    fn default() -> Self {
        Self {
            channels: [ChannelSettings::default(), ChannelSettings::default()],
            position: AtomicU32::new(0.5_f32.to_bits()),
            curve: AtomicU32::new(Curve::Eq.to_wire()),
            fade: AtomicU32::new(Fade::Full.to_wire()),
            // A knob nobody has reported is on CUE: the headphones are for
            // what is cued, and a controller's real position arrives when
            // it moves.
            headphone_mix: AtomicU32::new(0.0_f32.to_bits()),
            crossfader_on: AtomicBool::new(true),
            master_cue: AtomicBool::new(false),
            smart_on: AtomicBool::new(false),
            smart_kind: AtomicU32::new(SmartCfxKind::Echo.to_wire()),
            changed: Stamp::default(),
            latency: LatencyMeter::default(),
        }
    }
}

impl MixerSettings {
    /// The crossfader, 0 at deck A and 1 at deck B.
    ///
    /// Vertical in rekordbox's 2-player layout — A above, B below — which is
    /// why this is not called left and right.
    pub fn set_crossfade(&self, position: f32) {
        let safe = if position.is_finite() { position.clamp(0.0, 1.0) } else { 0.5 };
        self.position.store(safe.to_bits(), Ordering::Relaxed);
        self.changed.mark();
    }

    pub fn crossfade(&self) -> f32 {
        f32::from_bits(self.position.load(Ordering::Relaxed))
    }

    pub fn set_curve(&self, curve: Curve) {
        self.curve.store(curve.to_wire(), Ordering::Relaxed);
    }

    pub fn curve(&self) -> Curve {
        Curve::from_wire(self.curve.load(Ordering::Relaxed))
    }

    pub fn set_fade(&self, fade: Fade) {
        self.fade.store(fade.to_wire(), Ordering::Relaxed);
    }

    pub fn fade(&self) -> Fade {
        Fade::from_wire(self.fade.load(Ordering::Relaxed))
    }

    /// What each deck is multiplied by at the current fader position.
    pub(crate) fn fader(&self) -> (f32, f32) {
        if !self.crossfader_on() {
            return (1.0, 1.0);
        }
        self.fade().split(self.crossfade())
    }

    pub fn set_crossfader_on(&self, on: bool) {
        self.crossfader_on.store(on, Ordering::Relaxed);
        self.changed.mark();
    }

    pub fn crossfader_on(&self) -> bool {
        self.crossfader_on.load(Ordering::Relaxed)
    }

    pub fn set_master_cue(&self, on: bool) {
        self.master_cue.store(on, Ordering::Relaxed);
    }

    pub fn master_cue(&self) -> bool {
        self.master_cue.load(Ordering::Relaxed)
    }

    pub fn set_smart_cfx_on(&self, on: bool) {
        self.smart_on.store(on, Ordering::Relaxed);
        self.changed.mark();
    }

    pub fn smart_cfx_on(&self) -> bool {
        self.smart_on.load(Ordering::Relaxed)
    }

    pub fn set_smart_cfx_kind(&self, kind: SmartCfxKind) {
        self.smart_kind.store(kind.to_wire(), Ordering::Relaxed);
        self.changed.mark();
    }

    pub fn smart_cfx_kind(&self) -> SmartCfxKind {
        SmartCfxKind::from_wire(self.smart_kind.load(Ordering::Relaxed))
    }

    /// What the CFX knobs drive besides the filter, if SMART CFX is on.
    pub(crate) fn smart_cfx(&self) -> Option<SmartCfxKind> {
        self.smart_cfx_on().then(|| self.smart_cfx_kind())
    }

    pub fn set_headphone_mix(&self, position: f32) {
        let safe = if position.is_finite() { position.clamp(0.0, 1.0) } else { 0.5 };
        self.headphone_mix.store(safe.to_bits(), Ordering::Relaxed);
        self.changed.mark();
    }

    /// Called by the callback before it reads any control.
    pub(crate) fn observe_latency(&self) {
        let [a, b] = &self.channels;
        self.latency.observe(&[self.changed.take(), a.changed.take(), b.changed.take()]);
    }

    /// Drops changes made while nothing was rendering: the time a stream sat
    /// paused is not a control's latency.
    pub(crate) fn forget_pending(&self) {
        let [a, b] = &self.channels;
        for stamp in [&self.changed, &a.changed, &b.changed] {
            stamp.take();
        }
    }

    /// Control change stored → first callback that renders it.
    pub fn control_latency(&self) -> ControlLatency {
        self.latency.snapshot()
    }

    pub fn headphone_mix(&self) -> f32 {
        f32::from_bits(self.headphone_mix.load(Ordering::Relaxed))
    }

    /// Takes every knob, fader and switch from `other`: an engine rebuilt for
    /// a new device keeps the mixer the hardware and the interface show.
    pub fn copy_from(&self, other: &MixerSettings) {
        for (mine, theirs) in self.channels.iter().zip(other.channels.iter()) {
            mine.set_trim(theirs.trim());
            mine.set_volume(theirs.volume());
            mine.set_cue(theirs.cue());
            mine.set_filter(theirs.filter());
            for band in Band::ALL {
                mine.set_band(band, theirs.band(band));
                mine.set_kill(band, theirs.killed(band));
            }
        }
        self.set_crossfade(other.crossfade());
        self.set_curve(other.curve());
        self.set_fade(other.fade());
        self.set_headphone_mix(other.headphone_mix());
        self.set_crossfader_on(other.crossfader_on());
        self.set_master_cue(other.master_cue());
        self.set_smart_cfx_on(other.smart_cfx_on());
        self.set_smart_cfx_kind(other.smart_cfx_kind());
    }

    /// What the cue sum and the master are multiplied by on the headphones.
    ///
    /// **[ASSUME]** Constant power, like `Fade::Power`; not yet compared with
    /// rekordbox's own HEADPHONES MIX by ear.
    pub(crate) fn headphone_split(&self) -> (f32, f32) {
        Fade::Power.split(self.headphone_mix())
    }
}

/// The cue sum's headroom before the headphone soft clip: −6 dB, so two
/// decks cued at full level meet the knee rather than pass it.
pub(crate) const CUE_HEADROOM: f32 = 0.501_187_2;

/// Where the headphone soft clip starts bending.
const KNEE: f32 = 0.8;

/// Linear up to `KNEE`, then a tanh shoulder up to full scale.
///
/// Not the master limiter: a limiter pumping under a cued track is exactly
/// what a DJ listening for the beat does not want to hear.
pub(crate) fn soft_clip(x: f32) -> f32 {
    let size = x.abs();
    if size <= KNEE {
        return x;
    }
    let bent = KNEE + (1.0 - KNEE) * ((size - KNEE) / (1.0 - KNEE)).tanh();
    bent.copysign(x)
}

/// Below this a filter is taken as having stopped ringing.
///
/// An IIR's tail never actually reaches zero, and the last of it is where
/// denormal floats live — numbers a CPU can be an order of magnitude slower to
/// multiply, in the one place where being slow is an audible fault. −360 dB is
/// far below anything a 32-bit sample can carry anyway, so this costs nothing
/// but a comparison and it makes a stopped deck exactly silent rather than
/// nearly so.
const RUNG_OUT: f32 = 1e-18;

fn flush(value: f32) -> f32 {
    if value.abs() < RUNG_OUT { 0.0 } else { value }
}

/// A one-pole-per-stage biquad, transposed direct form 2.
///
/// Transposed because it is the form that keeps its state small and its
/// arithmetic stable in `f32` at audio rates, which is the whole of why this
/// is not written as a difference equation over a history buffer.
#[derive(Debug, Clone, Copy, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    /// A two-pole Butterworth low or high pass at `hz`.
    fn butterworth(hz: f32, rate: f32, high: bool) -> Self {
        let w = std::f32::consts::TAU * (hz / rate).clamp(1e-5, 0.49);
        let (sin, cos) = w.sin_cos();
        // Q of a Butterworth pair: two of these in series make Linkwitz-Riley.
        let alpha = sin / std::f32::consts::SQRT_2;
        let a0 = 1.0 + alpha;
        let (b0, b1, b2) = if high {
            let half = f32::midpoint(1.0, cos);
            (half, -(1.0 + cos), half)
        } else {
            let half = f32::midpoint(1.0, -cos);
            (half, 1.0 - cos, half)
        };
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: (-2.0 * cos) / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn run(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = flush(self.b1 * x - self.a1 * y + self.z2);
        self.z2 = flush(self.b2 * x - self.a2 * y);
        y
    }
}

/// A Linkwitz-Riley four-pole split: two Butterworths of the same corner.
#[derive(Debug, Clone, Copy, Default)]
struct Split {
    low: [Biquad; 2],
    high: [Biquad; 2],
}

impl Split {
    fn at(hz: f32, rate: f32) -> Self {
        Self {
            low: [Biquad::butterworth(hz, rate, false), Biquad::butterworth(hz, rate, false)],
            high: [Biquad::butterworth(hz, rate, true), Biquad::butterworth(hz, rate, true)],
        }
    }

    fn run(&mut self, x: f32) -> (f32, f32) {
        let mut low = x;
        for stage in &mut self.low {
            low = stage.run(low);
        }
        let mut high = x;
        for stage in &mut self.high {
            high = stage.run(high);
        }
        // Neither half is flipped. A two-pole split needs one of them
        // inverted to sum; a four-pole one does not — both halves are 6 dB
        // down at the corner and in phase there, which is the property this
        // costs twice the arithmetic for.
        (low, high)
    }
}

/// The three bands of one audio channel — one of these per side of the stereo.
#[derive(Debug, Clone, Copy, Default)]
struct Bands {
    first: Split,
    second: Split,
    /// The same corner as `second`, run on the low band to all-pass it.
    align: Split,
}

impl Bands {
    fn new(rate: f32) -> Self {
        Self {
            first: Split::at(CROSS_LOW_HZ, rate),
            second: Split::at(CROSS_HIGH_HZ, rate),
            align: Split::at(CROSS_HIGH_HZ, rate),
        }
    }

    /// High, mid and low, in that order.
    fn run(&mut self, x: f32) -> [f32; 3] {
        let (low_raw, rest) = self.first.run(x);
        let (mid, high) = self.second.run(rest);
        // The low band through the second crossover and summed back: that sum
        // is an all-pass, so the low band arrives with the same phase rotation
        // the other two picked up on the way through it.
        let (a, b) = self.align.run(low_raw);
        [high, mid, a + b]
    }
}

/// Within this of the centre CFX is off, so the knob at twelve o'clock is the
/// track bit for bit.
const FILTER_DEAD: f32 = 0.04;
/// How far past the dead zone the filter takes to fade fully in, so leaving
/// the centre does not switch a filter in with a step.
const FILTER_FADE: f32 = 0.1;
/// 1/Q: a little resonance, as a DJ filter has. **[ASSUME]** to be set by ear.
const FILTER_K: f32 = 1.1;

/// A state-variable filter, in the trapezoidal (TPT) form that stays stable
/// while its cutoff is swept every frame.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Svf {
    ic1: f32,
    ic2: f32,
}

impl Svf {
    /// The low-pass and high-pass outputs for one sample; `g` is
    /// `tan(pi * cutoff / rate)`.
    pub(crate) fn run(&mut self, x: f32, g: f32) -> (f32, f32) {
        let a1 = 1.0 / (1.0 + g * (g + FILTER_K));
        let a2 = g * a1;
        let a3 = g * a2;
        let v3 = x - self.ic2;
        let v1 = a1 * self.ic1 + a2 * v3;
        let v2 = self.ic2 + a2 * self.ic1 + a3 * v3;
        self.ic1 = flush(2.0 * v1 - self.ic1);
        self.ic2 = flush(2.0 * v2 - self.ic2);
        (v2, x - FILTER_K * v1 - v2)
    }
}

/// The CFX knob at `at` as a cutoff, whether it is a high pass, and how much
/// of the filtered signal is heard. Left sweeps a low pass from 20 kHz down to
/// 40 Hz, right a high pass from 20 Hz up to 12 kHz.
pub(crate) fn filter_shape(at: f32) -> (f32, bool, f32) {
    let from_centre = (at.clamp(0.0, 1.0) - 0.5) * 2.0;
    let wet = ((from_centre.abs() - FILTER_DEAD) / FILTER_FADE).clamp(0.0, 1.0);
    if from_centre < 0.0 {
        (20_000.0 * (40.0_f32 / 20_000.0).powf(-from_centre), false, wet)
    } else {
        (20.0 * (12_000.0_f32 / 20.0).powf(from_centre), true, wet)
    }
}

/// One deck's strip, as the callback owns it.
#[derive(Debug)]
pub struct Channel {
    bands: [Bands; 2],
    gains: [Smoothed; 3],
    trim: Smoothed,
    fader: Smoothed,
    /// The CUE send, walked between 0 and 1 so pressing CUE does not click.
    cue: Smoothed,
    filters: [Svf; 2],
    filter_at: Smoothed,
    rate: f32,
    smart: SmartFx,
    pad: PadFx,
}

impl Channel {
    pub fn new(rate: u32) -> Self {
        let bands = Bands::new(rate as f32);
        Self {
            bands: [bands, bands],
            gains: [
                Smoothed::new(1.0, rate),
                Smoothed::new(1.0, rate),
                Smoothed::new(1.0, rate),
            ],
            trim: Smoothed::new(1.0, rate),
            fader: Smoothed::new(1.0, rate),
            cue: Smoothed::new(0.0, rate),
            filters: [Svf::default(); 2],
            filter_at: Smoothed::new(0.5, rate),
            rate: rate.max(1) as f32,
            smart: SmartFx::new(rate),
            pad: PadFx::new(rate),
        }
    }

    /// Runs one deck's audio through its strip, in place.
    ///
    /// `fader` is the combined channel-volume/crossfader gain. Every gain moves
    /// per frame rather than per buffer, so nothing here can click.
    pub fn process(&mut self, out: &mut [f32], settings: &ChannelSettings, curve: Curve, fader: f32) {
        self.process_strip(out, settings, curve);
        self.apply_fader(out, fader);
    }

    /// Trim, the three bands and CFX: everything before the fader, which is
    /// where the headphones listen.
    pub fn process_strip(&mut self, out: &mut [f32], settings: &ChannelSettings, curve: Curve) {
        self.strip(out, settings, curve, None, 0.0);
    }

    /// The strip with SMART CFX (`smart`, timed to the BEAT FX beat) and the
    /// channel's PAD FX after it: what the engine runs before the fader.
    pub fn process_strip_with_fx(
        &mut self,
        out: &mut [f32],
        settings: &ChannelSettings,
        curve: Curve,
        smart: Option<SmartCfxKind>,
        beat_ms: f32,
    ) {
        self.strip(out, settings, curve, smart, beat_ms);
        self.pad.process(out, settings.pad.read());
    }

    fn strip(&mut self, out: &mut [f32], settings: &ChannelSettings, curve: Curve, smart: Option<SmartCfxKind>, beat_ms: f32) {
        let wanted = settings.wanted(curve);
        let trim = settings.trim();
        let filter = settings.filter();
        let beat_frames = beat_ms * self.rate / 1000.0;
        for frame in out.chunks_exact_mut(2) {
            let trim = self.trim.step(trim);
            let at = self.filter_at.step(filter);
            let (hz, high_pass, wet) = filter_shape(at);
            let amount = smart_amount(at, FILTER_DEAD);
            let shares = self.smart.shares(smart);
            let noise = if amount > 0.0 { self.smart.noise(amount, shares) } else { None };
            let g = if wet > 0.0 {
                (std::f32::consts::PI * (hz / self.rate).min(0.45)).tan()
            } else {
                self.filters = [Svf::default(); 2];
                0.0
            };
            let mut gains = [0.0_f32; 3];
            for (slot, (smoothed, target)) in
                gains.iter_mut().zip(self.gains.iter_mut().zip(wanted.iter()))
            {
                *slot = smoothed.step(*target);
            }
            for (side, sample) in frame.iter_mut().enumerate() {
                let Some(bands) = self.bands.get_mut(side) else { continue };
                let split = bands.run(*sample * trim);
                let mut sum = 0.0;
                for (band, gain) in split.iter().zip(gains.iter()) {
                    sum += band * gain;
                }
                // SMART CFX NOISE goes in before the filter, which shapes it.
                if let Some(add) = noise.as_ref().and_then(|n| n.get(side)) {
                    sum += add;
                }
                if wet > 0.0 {
                    if let Some(svf) = self.filters.get_mut(side) {
                        let (low, high) = svf.run(sum, g);
                        let filtered = if high_pass { high } else { low };
                        sum += (filtered - sum) * wet;
                    }
                }
                *sample = sum;
            }
            self.smart.after_filter(frame, amount, wet, shares, beat_frames);
        }
    }

    /// The channel fader and crossfader, after the strip.
    pub fn apply_fader(&mut self, out: &mut [f32], fader: f32) {
        for frame in out.chunks_exact_mut(2) {
            let gain = self.fader.step(fader);
            for sample in frame {
                *sample *= gain;
            }
        }
    }

    /// Adds the strip's output to the cue sum while CUE is on, fading in and
    /// out rather than switching. `to` may be shorter than `from` — or empty,
    /// when there are no headphones — and only what fits is sent.
    pub fn send_cue(&mut self, from: &[f32], to: &mut [f32], on: bool) {
        let target = if on { 1.0 } else { 0.0 };
        if !on && self.cue.value() <= 0.0 {
            return;
        }
        for (source, sink) in from.chunks_exact(2).zip(to.chunks_exact_mut(2)) {
            let gain = self.cue.step(target);
            for (add, sample) in source.iter().zip(sink.iter_mut()) {
                *sample += add * gain;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "unity and silence are exact, and that is the assertion")]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    /// A sine at `hz`, one second of it, mono written to both channels.
    fn tone(hz: f32, frames: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let t = i as f32 / RATE as f32;
            let value = (t * hz * std::f32::consts::TAU).sin();
            out.push(value);
            out.push(value);
        }
        out
    }

    /// The peak of the second half, so the filters' start-up is not measured.
    fn settled_peak(out: &[f32]) -> f32 {
        let half = out.len() / 2;
        out.get(half..).unwrap_or(out).iter().fold(0.0_f32, |a, s| a.max(s.abs()))
    }

    fn run(channel: &mut Channel, settings: &ChannelSettings, curve: Curve, hz: f32) -> f32 {
        let mut audio = tone(hz, RATE as usize / 2);
        channel.process(&mut audio, settings, curve, 1.0);
        settled_peak(&audio)
    }

    #[test]
    fn the_meter_and_the_controller_hold_and_clear_their_own_peaks() {
        let settings = ChannelSettings::default();
        settings.report_level(0.25);
        settings.report_level(0.5);
        settings.report_level(0.1);
        assert_eq!(settings.take_level(), 0.5);
        assert_eq!(settings.take_level(), 0.0);
        assert_eq!(settings.take_meter(), 0.5, "the controller's read cleared the meter");
        assert_eq!(settings.take_meter(), 0.0);
        settings.report_level(0.75);
        assert_eq!(settings.take_meter(), 0.75);
        assert_eq!(settings.take_level(), 0.75, "the meter's read cleared the controller's");
    }

    #[test]
    fn every_band_at_centre_is_the_track_and_not_an_opinion_about_it() {
        // The whole of why the split is four-pole: at centre the strip has to
        // be transparent at every frequency, including where two bands meet.
        for hz in [50.0, 120.0, 300.0, 800.0, 2_000.0, 4_000.0, 9_000.0] {
            let settings = ChannelSettings::default();
            let mut channel = Channel::new(RATE);
            let peak = run(&mut channel, &settings, Curve::Eq, hz);
            // Measured: the worst of these is 0.13% out, at 9 kHz, and the
            // two crossovers themselves are the flattest points of all.
            assert!(
                (peak - 1.0).abs() < 0.005,
                "at {hz} Hz a flat strip passed {peak}, not 1.0",
            );
        }
    }

    #[test]
    fn killing_the_low_band_takes_the_bass_and_leaves_the_rest() {
        let settings = ChannelSettings::default();
        settings.set_kill(Band::Low, true);
        let mut channel = Channel::new(RATE);
        let bass = run(&mut channel, &settings, Curve::Eq, 60.0);
        let mut channel = Channel::new(RATE);
        let top = run(&mut channel, &settings, Curve::Eq, 8_000.0);
        // −26 dB is a twentieth.
        assert!(bass < 0.08, "the bass was still at {bass}");
        assert!(top > 0.9, "the top was pulled down to {top}");
    }

    #[test]
    fn an_isolator_kill_is_silence_and_an_eq_kill_is_not() {
        let settings = ChannelSettings::default();
        settings.set_kill(Band::Low, true);
        let mut channel = Channel::new(RATE);
        let eq = run(&mut channel, &settings, Curve::Eq, 60.0);
        let mut channel = Channel::new(RATE);
        let iso = run(&mut channel, &settings, Curve::Isolator, 60.0);
        assert!(eq > 0.02, "an EQ kill should still let a little through: {eq}");
        // Not literally nothing: the band is gone, and what is left is the
        // skirt of the band above it, 56 dB down at 60 Hz. That is what an
        // isolator does on a real mixer too — it removes a band, it is not a
        // brick wall.
        assert!(iso < 0.01, "an isolator kill should leave next to nothing: {iso}");
        assert!(iso < eq / 10.0, "an isolator kill should be far below an EQ one");
    }

    #[test]
    fn a_band_at_the_top_of_its_travel_is_six_decibels_up() {
        let settings = ChannelSettings::default();
        settings.set_band(Band::Low, 1.0);
        let mut channel = Channel::new(RATE);
        let peak = run(&mut channel, &settings, Curve::Eq, 60.0);
        let six_db = db_to_gain(6.0);
        assert!((peak - six_db).abs() < 0.06, "the low band boosted to {peak}, not {six_db}");
    }

    #[test]
    fn the_default_fader_leaves_a_deck_nobody_has_touched_at_unity() {
        // The one-player layout draws no crossfader at all, so whatever the
        // engine does by default has to be "play the file at its own level".
        let mixer = MixerSettings::default();
        assert_eq!(mixer.fader(), (1.0, 1.0));
    }

    #[test]
    fn constant_power_holds_the_level_as_it_crosses() {
        // A track on both decks must not jump as the fader passes the middle.
        // Summed in power, since two decks playing different music do not add
        // coherently.
        let mixer = MixerSettings::default();
        mixer.set_fade(Fade::Power);
        for at in [0.0_f32, 0.25, 0.5, 0.75, 1.0] {
            mixer.set_crossfade(at);
            let (a, b) = mixer.fader();
            let power = (a * a + b * b).sqrt();
            assert!((power - 1.0).abs() < 1e-5, "at {at} the pair summed to {power}");
        }
    }

    #[test]
    fn the_ends_of_the_crossfader_are_one_deck_and_nothing_of_the_other() {
        for fade in [Fade::Full, Fade::Power] {
            let mixer = MixerSettings::default();
            mixer.set_fade(fade);
            mixer.set_crossfade(0.0);
            assert_eq!(mixer.fader().0, 1.0, "{fade:?} should leave A whole");
            assert!(mixer.fader().1.abs() < 1e-6, "{fade:?} should silence B");
            mixer.set_crossfade(1.0);
            assert!(mixer.fader().0.abs() < 1e-6, "{fade:?} should silence A");
            assert_eq!(mixer.fader().1, 1.0, "{fade:?} should leave B whole");
        }
    }

    #[test]
    fn channel_volume_is_independent_of_trim_and_smoothly_mutes_the_channel() {
        let settings = ChannelSettings::default();
        assert_eq!(settings.volume(), 1.0);
        assert_eq!(settings.trim(), 1.0);
        let mut channel = Channel::new(RATE);
        let mut audio = tone(220.0, RATE as usize / 4);
        channel.process(&mut audio, &settings, Curve::Eq, settings.volume());
        assert!(settled_peak(&audio) > 0.9);

        settings.set_volume(0.0);
        assert_eq!(settings.trim(), 1.0, "volume must not change input trim");
        let mut silence = tone(220.0, RATE as usize / 4);
        channel.process(&mut silence, &settings, Curve::Eq, settings.volume());
        assert!(settled_peak(&silence) < 1e-4, "channel fader did not fade to silence");

        settings.set_volume(0.5);
        let mut half = tone(220.0, RATE as usize / 4);
        channel.process(&mut half, &settings, Curve::Eq, settings.volume());
        let peak = settled_peak(&half);
        assert!((peak - 0.5).abs() < 0.01, "half volume was {peak}");

        settings.set_volume(f32::NAN);
        assert_eq!(settings.volume(), 1.0);
        settings.set_volume(9.0);
        assert_eq!(settings.volume(), 1.0);
        settings.set_volume(-9.0);
        assert_eq!(settings.volume(), 0.0);
    }

    #[test]
    fn the_headphone_mix_runs_from_cue_alone_to_master_alone() {
        let mixer = MixerSettings::default();
        assert_eq!(mixer.headphone_mix(), 0.0, "a knob nobody has reported is on CUE");
        mixer.set_headphone_mix(0.0);
        let (cue, master) = mixer.headphone_split();
        assert_eq!(cue, 1.0);
        assert!(master.abs() < 1e-6);
        mixer.set_headphone_mix(1.0);
        let (cue, master) = mixer.headphone_split();
        assert!(cue.abs() < 1e-6);
        assert_eq!(master, 1.0);
        mixer.set_headphone_mix(f32::NAN);
        assert_eq!(mixer.headphone_mix(), 0.5);
    }

    #[test]
    fn the_soft_clip_is_transparent_below_its_knee_and_never_passes_full_scale() {
        for x in [-0.8_f32, -0.3, 0.0, 0.5, 0.8] {
            assert_eq!(soft_clip(x), x);
        }
        for x in [0.9_f32, 1.5, 4.0, 100.0] {
            let y = soft_clip(x);
            // f32's tanh rounds to 1.0 a little past 4, so full scale is reached.
            assert!(y > KNEE && y <= 1.0, "{x} came out at {y}");
            assert_eq!(soft_clip(-x), -y, "the curve must be symmetric");
        }
        // No corner at the knee: just past it, it still rises at about 1:1.
        let slope = (soft_clip(KNEE + 1e-3) - KNEE) / 1e-3;
        assert!((slope - 1.0).abs() < 0.01, "the knee bends at {slope}");
    }

    #[test]
    fn cue_is_a_switch_on_the_channel_and_starts_off() {
        let settings = ChannelSettings::default();
        assert!(!settings.cue());
        settings.set_cue(true);
        assert!(settings.cue());
    }

    #[test]
    fn cfx_at_centre_is_the_track_bit_for_bit() {
        let settings = ChannelSettings::default();
        let mut with = Channel::new(RATE);
        let mut audio = tone(440.0, RATE as usize / 8);
        with.process_strip(&mut audio, &settings, Curve::Eq);
        settings.set_filter(0.51);
        let mut nudged = tone(440.0, RATE as usize / 8);
        Channel::new(RATE).process_strip(&mut nudged, &settings, Curve::Eq);
        assert_eq!(audio, nudged, "inside the dead zone CFX must do nothing at all");
    }

    #[test]
    fn cfx_left_takes_the_top_and_right_takes_the_bottom() {
        let settings = ChannelSettings::default();
        settings.set_filter(0.0);
        let mut channel = Channel::new(RATE);
        let top = run(&mut channel, &settings, Curve::Eq, 8_000.0);
        let mut channel = Channel::new(RATE);
        let bass = run(&mut channel, &settings, Curve::Eq, 30.0);
        assert!(top < 0.01, "a closed low pass let 8 kHz through at {top}");
        assert!(bass > 0.8, "a closed low pass took the bass to {bass}");

        settings.set_filter(1.0);
        let mut channel = Channel::new(RATE);
        let bass = run(&mut channel, &settings, Curve::Eq, 60.0);
        assert!(bass < 0.01, "a closed high pass let 60 Hz through at {bass}");
    }

    #[test]
    fn sweeping_cfx_never_blows_up() {
        let settings = ChannelSettings::default();
        let mut channel = Channel::new(RATE);
        let mut worst = 0.0_f32;
        for step in 0..200 {
            settings.set_filter((step % 20) as f32 / 19.0);
            let mut audio = tone(1_000.0, 256);
            channel.process_strip(&mut audio, &settings, Curve::Eq);
            worst = audio.iter().fold(worst, |a, s| a.max(s.abs()));
        }
        assert!(worst.is_finite() && worst < 2.0, "a swept filter peaked at {worst}");
    }

    #[test]
    fn a_crossfader_switched_off_leaves_both_decks_whole() {
        let mixer = MixerSettings::default();
        mixer.set_crossfade(0.0);
        mixer.set_crossfader_on(false);
        assert_eq!(mixer.fader(), (1.0, 1.0));
        mixer.set_crossfader_on(true);
        assert!(mixer.fader().1.abs() < 1e-6);
    }

    #[test]
    fn a_copied_mixer_has_every_knob_where_the_old_one_left_it() {
        let old = MixerSettings::default();
        old.channels[1].set_volume(0.3);
        old.channels[1].set_cue(true);
        old.channels[0].set_filter(0.9);
        old.channels[0].set_kill(Band::Mid, true);
        old.set_crossfade(0.1);
        old.set_headphone_mix(0.7);
        old.set_smart_cfx_on(true);
        old.set_smart_cfx_kind(SmartCfxKind::Noise);
        old.channels[0].set_pad_fx(Some((PadFxKind::Roll, 1.0)), 500.0);
        let fresh = MixerSettings::default();
        fresh.copy_from(&old);
        assert_eq!(fresh.channels[1].volume(), 0.3);
        assert!(fresh.channels[1].cue());
        assert_eq!(fresh.channels[0].filter(), 0.9);
        assert!(fresh.channels[0].killed(Band::Mid));
        assert_eq!(fresh.crossfade(), 0.1);
        assert_eq!(fresh.headphone_mix(), 0.7);
        assert!(fresh.smart_cfx_on());
        assert_eq!(fresh.smart_cfx_kind(), SmartCfxKind::Noise);
        assert_eq!(fresh.channels[0].pad_fx(), None, "a held pad is not carried to a new engine");
    }

    #[test]
    fn a_knob_at_an_end_is_taken_rather_than_refused() {
        let settings = ChannelSettings::default();
        settings.set_band(Band::Mid, 4.0);
        assert_eq!(settings.band(Band::Mid), 1.0);
        settings.set_band(Band::Mid, f32::NAN);
        assert_eq!(settings.band(Band::Mid), 0.5);
        settings.set_trim(-1.0);
        assert_eq!(settings.trim(), 0.0);
    }
}
