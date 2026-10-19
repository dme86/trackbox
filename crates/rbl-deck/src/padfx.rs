//! PAD FX and SMART CFX: the per-channel inserts behind the performance pads
//! and the CFX knob.
//!
//! Both run in the channel strip, after trim, EQ and CFX and before the level
//! report, the CUE tap and the fader, so the headphones hear them too. Every
//! buffer is allocated when the strip is built, at the first callback; idle,
//! neither touches a sample, so a strip nobody performs on is bit for bit the
//! plain one. Every switch goes through a two-millisecond `Ramp`, and echo and
//! reverb only close their input: their tails ring out.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::fade::Ramp;
use crate::fx::{Echo, Reverb};
use crate::mixer::{filter_shape, Svf};

/// How far back a roll can reach: four beats down to 60 BPM.
const HISTORY_SECONDS: f32 = 4.0;
/// The crossfade at a roll's seam and on a re-press.
const SEAM_SECONDS: f32 = 0.002;
const ECHO_FEEDBACK: f32 = 0.55;
/// How loud an echo's or a reverb's return sits on top of the dry signal.
const RETURN: f32 = 0.8;
/// A released HPF/LPF opens on this time constant: neutral in about 50 ms.
const OPEN_TAU_SECONDS: f32 = 0.015;
/// PAD FX CRUSH: a fixed amount, six bits at 6 kHz.
const CRUSH_BITS: f32 = 6.0;
const CRUSH_HZ: f32 = 6_000.0;
/// Below this a return is finished: −100 dB.
const QUIET: f32 = 1e-5;
/// SMART CFX NOISE at the end of the knob's travel, before the filter.
const NOISE_LEVEL: f32 = 0.35;
const SMART_ROOM: f32 = 0.88;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadFxKind {
    Echo,
    Reverb,
    Trans,
    Roll,
    Hpf,
    Lpf,
    Crush,
}

impl PadFxKind {
    const ALL: [Self; 7] = [Self::Echo, Self::Reverb, Self::Trans, Self::Roll, Self::Hpf, Self::Lpf, Self::Crush];

    fn from_wire(wire: u32) -> Self {
        Self::ALL.get(wire as usize).copied().unwrap_or(Self::Echo)
    }

    fn to_wire(self) -> u32 {
        self as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmartCfxKind {
    Echo,
    Reverb,
    Noise,
    Crush,
}

impl SmartCfxKind {
    const ALL: [Self; 4] = [Self::Echo, Self::Reverb, Self::Noise, Self::Crush];

    pub(crate) fn from_wire(wire: u32) -> Self {
        Self::ALL.get(wire as usize).copied().unwrap_or(Self::Echo)
    }

    pub(crate) fn to_wire(self) -> u32 {
        self as u32
    }
}

/// One channel's pad, as the interface leaves it for the callback.
#[derive(Debug)]
pub(crate) struct PadSettings {
    on: AtomicBool,
    kind: AtomicU32,
    beats: AtomicU32,
    beat_ms: AtomicU32,
    /// Counts presses, so a new pad while one is held restarts the effect.
    presses: AtomicU32,
}

impl Default for PadSettings {
    fn default() -> Self {
        Self {
            on: AtomicBool::new(false),
            kind: AtomicU32::new(0),
            beats: AtomicU32::new(1.0_f32.to_bits()),
            beat_ms: AtomicU32::new(500.0_f32.to_bits()),
            presses: AtomicU32::new(0),
        }
    }
}

/// What the callback reads of a pad, once per buffer.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Pad {
    held: Option<PadFxKind>,
    beats: f32,
    beat_ms: f32,
    press: u32,
}

impl PadSettings {
    pub(crate) fn set(&self, fx: Option<(PadFxKind, f32)>, beat_ms: f32) {
        let ms = if beat_ms.is_finite() { beat_ms.clamp(10.0, 8_000.0) } else { 500.0 };
        self.beat_ms.store(ms.to_bits(), Ordering::Relaxed);
        match fx {
            Some((kind, beats)) => {
                let beats = if beats.is_finite() { beats.clamp(1.0 / 16.0, 16.0) } else { 1.0 };
                self.kind.store(kind.to_wire(), Ordering::Relaxed);
                self.beats.store(beats.to_bits(), Ordering::Relaxed);
                self.presses.fetch_add(1, Ordering::Release);
                self.on.store(true, Ordering::Release);
            }
            None => self.on.store(false, Ordering::Release),
        }
    }

    pub(crate) fn get(&self) -> Option<(PadFxKind, f32)> {
        self.read().held.map(|kind| (kind, f32::from_bits(self.beats.load(Ordering::Relaxed))))
    }

    pub(crate) fn read(&self) -> Pad {
        let on = self.on.load(Ordering::Acquire);
        let press = self.presses.load(Ordering::Acquire);
        Pad {
            held: on.then(|| PadFxKind::from_wire(self.kind.load(Ordering::Relaxed))),
            beats: f32::from_bits(self.beats.load(Ordering::Relaxed)),
            beat_ms: f32::from_bits(self.beat_ms.load(Ordering::Relaxed)),
            press,
        }
    }
}

/// Frames an effect's return has stayed quiet with nothing going in.
#[derive(Debug, Clone, Copy)]
struct Tail {
    quiet: usize,
}

impl Tail {
    const IDLE: Self = Self { quiet: usize::MAX };

    /// Quiet for `window` frames — longer than its longest line — and the
    /// effect has nothing left to say.
    fn live(self, window: usize) -> bool {
        self.quiet < window
    }

    fn note(&mut self, feeding: bool, back: (f32, f32)) {
        if feeding || back.0.abs() > QUIET || back.1.abs() > QUIET {
            self.quiet = 0;
        } else {
            self.quiet = self.quiet.saturating_add(1);
        }
    }
}

/// Sample-and-hold and requantise: the bit crusher both inserts share.
#[derive(Debug, Clone, Copy)]
struct Crusher {
    hold: [f32; 2],
    phase: f32,
}

impl Crusher {
    /// Starts due, so the first frame is held rather than silence.
    const NEW: Self = Self { hold: [0.0; 2], phase: 1.0 };

    /// `step` is the held rate over the device's, at most 1; `levels` the
    /// steps either side of zero.
    fn run(&mut self, frame: [f32; 2], step: f32, levels: f32) -> [f32; 2] {
        self.phase += step;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
            self.hold = frame.map(|x| (x * levels).round() / levels);
        }
        self.hold
    }
}

/// A roll's read head over the history ring, with a crossfade at its seam.
#[derive(Debug, Clone, Copy, Default)]
struct Loop {
    start: usize,
    end: usize,
    read: usize,
    fade: usize,
    /// A second head fading in: its position and how far in it is.
    incoming: Option<(usize, usize)>,
}

impl Loop {
    fn next(&mut self, history: &[f32]) -> (f32, f32) {
        let frames = (history.len() / 2).max(1);
        let at = |i: usize| {
            (history.get(i * 2).copied().unwrap_or(0.0), history.get(i * 2 + 1).copied().unwrap_or(0.0))
        };
        // `fade` frames before the end, a head starts that many frames before
        // the start: it reaches the start just as the main head reaches the end.
        if self.incoming.is_none() && (self.end + frames - self.read) % frames == self.fade {
            self.incoming = Some(((self.start + frames - self.fade) % frames, 0));
        }
        let main = at(self.read);
        self.read = (self.read + 1) % frames;
        let Some((head, done)) = self.incoming else { return main };
        let k = (done + 1) as f32 / (self.fade + 1) as f32;
        let new = at(head);
        let next = (head + 1) % frames;
        if done + 1 >= self.fade {
            self.read = next;
            self.incoming = None;
        } else {
            self.incoming = Some((next, done + 1));
        }
        (main.0 + (new.0 - main.0) * k, main.1 + (new.1 - main.1) * k)
    }
}

/// HPF or LPF: closes over the pad's beats, opens in about 50 ms on release.
#[derive(Debug, Clone, Copy)]
struct Sweep {
    closed: f32,
    filters: [Svf; 2],
    high: bool,
}

impl Sweep {
    fn new(high: bool) -> Self {
        Self { closed: 0.0, filters: [Svf::default(); 2], high }
    }

    fn run(&mut self, out: &mut [f32], held: bool, span: f32, rate: f32) {
        if !held && self.closed <= 0.0 {
            return;
        }
        let rise = 1.0 / span;
        let keep = (-1.0 / (OPEN_TAU_SECONDS * rate)).exp();
        for frame in out.chunks_exact_mut(2) {
            self.closed = if held {
                (self.closed + rise).min(1.0)
            } else if self.closed * keep < 1e-3 {
                0.0
            } else {
                self.closed * keep
            };
            // The CFX knob's own curve, so a pad sweep and a hand on the knob
            // sound the same, dead zone and fade-in included.
            let half = self.closed / 2.0;
            let (hz, high, wet) = filter_shape(if self.high { 0.5 + half } else { 0.5 - half });
            if wet <= 0.0 {
                self.filters = [Svf::default(); 2];
                continue;
            }
            let g = (std::f32::consts::PI * (hz / rate).min(0.45)).tan();
            for (sample, svf) in frame.iter_mut().zip(self.filters.iter_mut()) {
                let (low, high_out) = svf.run(*sample, g);
                let filtered = if high { high_out } else { low };
                *sample += (filtered - *sample) * wet;
            }
        }
    }
}

/// Exactly `dry` at 0 and exactly `wet` at 1, which `dry + (wet - dry) * m` is not.
fn blend(dry: f32, wet: f32, m: f32) -> f32 {
    wet * m + dry * (1.0 - m)
}

fn room_of(beats: f32) -> f32 {
    0.7 + 0.27 * ((beats - 1.0) / 3.0).clamp(0.0, 1.0)
}

/// One channel's PAD FX, as the callback owns it.
pub(crate) struct PadFx {
    rate: f32,
    press: u32,
    echo: Echo,
    echo_send: Ramp,
    echo_tail: Tail,
    /// The echo's time, taken while its pad is held so a tail keeps it.
    echo_frames: f32,
    reverb: Reverb,
    reverb_send: Ramp,
    reverb_tail: Tail,
    room: f32,
    trans_closed: Ramp,
    trans_phase: f32,
    /// Post-strip audio, stereo, written while no roll is sounding.
    history: Vec<f32>,
    written: usize,
    roll: Ramp,
    looped: Loop,
    sweeps: [Sweep; 2],
    crush: Ramp,
    crusher: Crusher,
}

impl std::fmt::Debug for PadFx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PadFx").finish_non_exhaustive()
    }
}

impl PadFx {
    pub(crate) fn new(rate: u32) -> Self {
        let r = rate.max(1) as f32;
        let frames = ((HISTORY_SECONDS * r) as usize).max(4);
        Self {
            rate: r,
            press: 0,
            echo: Echo::new(rate),
            echo_send: Ramp::silent(),
            echo_tail: Tail::IDLE,
            echo_frames: r / 2.0,
            reverb: Reverb::new(rate),
            reverb_send: Ramp::silent(),
            reverb_tail: Tail::IDLE,
            room: room_of(1.0),
            trans_closed: Ramp::silent(),
            trans_phase: 0.0,
            history: vec![0.0; frames * 2],
            written: 0,
            roll: Ramp::silent(),
            looped: Loop::default(),
            sweeps: [Sweep::new(false), Sweep::new(true)],
            crush: Ramp::silent(),
            crusher: Crusher::NEW,
        }
    }

    /// Runs the pad over one deck's strip output, in place.
    pub(crate) fn process(&mut self, out: &mut [f32], pad: Pad) {
        let pressed = pad.held.is_some() && pad.press != self.press;
        self.press = pad.press;
        let held = |kind| pad.held == Some(kind);
        let span = (pad.beats * pad.beat_ms * self.rate / 1000.0).max(1.0);

        self.roll(out, held(PadFxKind::Roll), pressed && held(PadFxKind::Roll), span);
        let rate = self.rate;
        for (sweep, kind) in self.sweeps.iter_mut().zip([PadFxKind::Lpf, PadFxKind::Hpf]) {
            sweep.run(out, held(kind), span, rate);
        }
        self.crush(out, held(PadFxKind::Crush));
        self.trans(out, held(PadFxKind::Trans), pressed && held(PadFxKind::Trans), span);
        if held(PadFxKind::Echo) {
            self.echo_frames = span;
        }
        self.echo(out, held(PadFxKind::Echo));
        if held(PadFxKind::Reverb) {
            self.room = room_of(pad.beats);
        }
        self.reverb(out, held(PadFxKind::Reverb));
    }

    fn remember(&mut self, out: &[f32]) {
        let frames = (self.history.len() / 2).max(1);
        for frame in out.chunks_exact(2) {
            if let Some(slot) = self.history.get_mut(self.written * 2..self.written * 2 + 2) {
                slot.copy_from_slice(frame);
            }
            self.written = (self.written + 1) % frames;
        }
    }

    /// The most recent `span` frames become the loop. Pressed again while a
    /// roll sounds, the new loop fades in from wherever the old one was.
    fn capture(&mut self, span: f32, rolling: bool) {
        let frames = (self.history.len() / 2).max(1);
        let seam = (SEAM_SECONDS * self.rate) as usize;
        let len = (span as usize).clamp(2, frames.saturating_sub(seam + 2).max(2));
        let start = (self.written + frames - len) % frames;
        self.looped.start = start;
        self.looped.end = self.written;
        self.looped.fade = seam.min(len / 2);
        if rolling {
            self.looped.incoming = Some((start, 0));
        } else {
            self.looped.read = start;
            self.looped.incoming = None;
        }
    }

    fn roll(&mut self, out: &mut [f32], held: bool, pressed: bool, span: f32) {
        if pressed {
            self.capture(span, !self.roll.silent_now());
        }
        // ponytail: the history stops while a roll sounds (the loop lives in
        // it); a re-press just after a release can reach back across the gap.
        if !held && self.roll.silent_now() {
            self.remember(out);
            return;
        }
        for frame in out.chunks_exact_mut(2) {
            let [left, right] = frame else { continue };
            let mix = self.roll.step(held);
            let (l, r) = self.looped.next(&self.history);
            *left = blend(*left, l, mix);
            *right = blend(*right, r, mix);
        }
    }

    fn crush(&mut self, out: &mut [f32], held: bool) {
        if !held && self.crush.silent_now() {
            return;
        }
        let step = (CRUSH_HZ / self.rate).min(1.0);
        let levels = 2.0_f32.powf(CRUSH_BITS - 1.0);
        for frame in out.chunks_exact_mut(2) {
            let [left, right] = frame else { continue };
            let mix = self.crush.step(held);
            let [l, r] = self.crusher.run([*left, *right], step, levels);
            *left = blend(*left, l, mix);
            *right = blend(*right, r, mix);
        }
    }

    fn trans(&mut self, out: &mut [f32], held: bool, pressed: bool, span: f32) {
        if pressed {
            self.trans_phase = 0.0;
        }
        if !held && self.trans_closed.silent_now() {
            return;
        }
        for frame in out.chunks_exact_mut(2) {
            let open = !held || self.trans_phase < span / 2.0;
            self.trans_phase += 1.0;
            if self.trans_phase >= span {
                self.trans_phase -= span;
            }
            let gain = 1.0 - self.trans_closed.step(!open);
            for sample in frame {
                *sample *= gain;
            }
        }
    }

    fn echo(&mut self, out: &mut [f32], held: bool) {
        let window = self.echo_frames as usize + (0.1 * self.rate) as usize;
        if !held && self.echo_send.silent_now() && !self.echo_tail.live(window) {
            return;
        }
        for frame in out.chunks_exact_mut(2) {
            let [left, right] = frame else { continue };
            let send = self.echo_send.step(held);
            let back = self.echo.run((*left * send, *right * send), self.echo_frames, ECHO_FEEDBACK);
            self.echo_tail.note(send > 0.0, back);
            *left += back.0 * RETURN;
            *right += back.1 * RETURN;
        }
    }

    fn reverb(&mut self, out: &mut [f32], held: bool) {
        let window = (0.2 * self.rate) as usize;
        if !held && self.reverb_send.silent_now() && !self.reverb_tail.live(window) {
            return;
        }
        for frame in out.chunks_exact_mut(2) {
            let [left, right] = frame else { continue };
            let send = self.reverb_send.step(held);
            let back = self.reverb.run((*left * send, *right * send), self.room);
            self.reverb_tail.note(send > 0.0, back);
            *left += back.0 * RETURN;
            *right += back.1 * RETURN;
        }
    }
}

/// How far the CFX knob is from its dead zone, 0 to 1.
pub(crate) fn smart_amount(at: f32, dead: f32) -> f32 {
    let from_centre = ((at.clamp(0.0, 1.0) - 0.5) * 2.0).abs();
    ((from_centre - dead) / (1.0 - dead)).clamp(0.0, 1.0)
}

/// One channel's SMART CFX, as the strip owns it. The filter is the strip's
/// own; this adds what goes with it.
pub(crate) struct SmartFx {
    rate: f32,
    echo: Echo,
    echo_tail: Tail,
    reverb: Reverb,
    reverb_tail: Tail,
    /// Each kind's share, so switching kind or SMART CFX itself does not click.
    shares: [Ramp; 4],
    crusher: Crusher,
    noise: u32,
}

impl std::fmt::Debug for SmartFx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmartFx").finish_non_exhaustive()
    }
}

impl SmartFx {
    pub(crate) fn new(rate: u32) -> Self {
        Self {
            rate: rate.max(1) as f32,
            echo: Echo::new(rate),
            echo_tail: Tail::IDLE,
            reverb: Reverb::new(rate),
            reverb_tail: Tail::IDLE,
            shares: [Ramp::silent(); 4],
            crusher: Crusher::NEW,
            noise: 0x9E37_79B9,
        }
    }

    /// Steps each kind's share one frame towards `on`.
    pub(crate) fn shares(&mut self, on: Option<SmartCfxKind>) -> [f32; 4] {
        let mut out = [0.0; 4];
        for ((slot, ramp), kind) in out.iter_mut().zip(self.shares.iter_mut()).zip(SmartCfxKind::ALL) {
            *slot = ramp.step(on == Some(kind));
        }
        out
    }

    /// NOISE for one frame, to go in before the filter so the filter shapes it.
    /// `None` when there is none, so a strip without it stays bit for bit.
    pub(crate) fn noise(&mut self, amount: f32, shares: [f32; 4]) -> Option<[f32; 2]> {
        let level = NOISE_LEVEL * amount * shares.get(2).copied().unwrap_or(0.0);
        (level > 0.0).then(|| [self.white() * level, self.white() * level])
    }

    fn white(&mut self) -> f32 {
        // xorshift32: no allocation, no lock, and white enough for a riser.
        let mut x = self.noise;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.noise = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// CRUSH, ECHO and REVERB on one frame, after the filter. `wet` is the
    /// filter's own fade-in, `beat_frames` the BEAT FX beat.
    pub(crate) fn after_filter(&mut self, frame: &mut [f32], amount: f32, wet: f32, shares: [f32; 4], beat_frames: f32) {
        let [left, right] = frame else { return };
        let [echo_share, reverb_share, _, crush_share] = shares;
        if amount > 0.0 && crush_share > 0.0 {
            // 16 bits at the device rate near the centre, 5 bits at 15% of it at the end.
            let step = 1.0 - 0.85 * amount;
            let levels = 2.0_f32.powf(15.0 - 11.0 * amount);
            let [l, r] = self.crusher.run([*left, *right], step, levels);
            let mix = crush_share * wet;
            *left += (l - *left) * mix;
            *right += (r - *right) * mix;
        }
        let delay = 0.75 * beat_frames;
        let send = amount * echo_share;
        if send > 0.0 || self.echo_tail.live(delay as usize + (0.1 * self.rate) as usize) {
            let back = self.echo.run((*left * send, *right * send), delay, 0.3 + 0.5 * amount);
            self.echo_tail.note(send > 0.0, back);
            *left += back.0 * RETURN;
            *right += back.1 * RETURN;
        }
        let send = amount * reverb_share;
        if send > 0.0 || self.reverb_tail.live((0.2 * self.rate) as usize) {
            let back = self.reverb.run((*left * send, *right * send), SMART_ROOM);
            self.reverb_tail.note(send > 0.0, back);
            *left += back.0 * RETURN;
            *right += back.1 * RETURN;
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "bypass is exact, and that is the assertion")]
mod tests {
    use super::*;
    use crate::mixer::{Channel, ChannelSettings, Curve};

    const RATE: u32 = 44_100;

    fn run(fx: &mut PadFx, settings: &PadSettings, audio: &mut [f32]) {
        for chunk in audio.chunks_mut(1024) {
            fx.process(chunk, settings.read());
        }
    }

    fn sine(hz: f32, frames: usize, from: usize) -> Vec<f32> {
        (from..from + frames)
            .flat_map(|i| {
                let v = 0.8 * (i as f32 * hz * std::f32::consts::TAU / RATE as f32).sin();
                [v, v]
            })
            .collect()
    }

    fn peak(audio: &[f32]) -> f32 {
        audio.iter().fold(0.0_f32, |a, s| a.max(s.abs()))
    }

    fn worst_step(audio: &[f32]) -> f32 {
        audio.chunks_exact(2).map(|f| f[0]).collect::<Vec<_>>().windows(2).map(|p| (p[1] - p[0]).abs()).fold(0.0, f32::max)
    }

    #[test]
    fn a_pad_never_pressed_is_the_signal_bit_for_bit() {
        let settings = PadSettings::default();
        let mut fx = PadFx::new(RATE);
        let mut audio = sine(440.0, 8_192, 0);
        let before = audio.clone();
        run(&mut fx, &settings, &mut audio);
        assert_eq!(audio, before);
    }

    #[test]
    fn every_pad_lets_go_back_to_the_signal_bit_for_bit() {
        for kind in PadFxKind::ALL {
            let settings = PadSettings::default();
            let mut fx = PadFx::new(RATE);
            settings.set(Some((kind, 1.0)), 20.0);
            let mut audio = sine(440.0, 8_192, 0);
            run(&mut fx, &settings, &mut audio);
            settings.set(None, 20.0);
            let mut tail = vec![0.0; RATE as usize * 10];
            run(&mut fx, &settings, &mut tail);
            let mut after = sine(440.0, 4_096, 0);
            let before = after.clone();
            run(&mut fx, &settings, &mut after);
            assert_eq!(after, before, "{kind:?} was still in the path after it rang out");
        }
    }

    #[test]
    fn an_echo_comes_back_beats_times_the_beat_later() {
        let settings = PadSettings::default();
        let mut fx = PadFx::new(RATE);
        // Half a beat of 200 ms: 100 ms.
        settings.set(Some((PadFxKind::Echo, 0.5)), 200.0);
        let mut warm = vec![0.0; 8_820];
        run(&mut fx, &settings, &mut warm);
        let mut audio = vec![0.0; 2 * 8_820];
        audio[0] = 1.0;
        audio[1] = 1.0;
        run(&mut fx, &settings, &mut audio);
        let (at, level) = audio
            .chunks_exact(2)
            .enumerate()
            .skip(10)
            .fold((0, 0.0_f32), |(at, best), (i, f)| if f[0].abs() > best { (i, f[0].abs()) } else { (at, best) });
        assert!(level > 0.5, "the echo came back at {level}");
        assert!(at.abs_diff(4_410) <= 1, "the echo came back at frame {at}, not 4410");
    }

    #[test]
    fn a_released_echo_rings_out_with_the_dry_signal_passing() {
        let settings = PadSettings::default();
        let mut fx = PadFx::new(RATE);
        settings.set(Some((PadFxKind::Echo, 1.0)), 50.0);
        let mut audio = vec![0.5; 8_820];
        run(&mut fx, &settings, &mut audio);
        settings.set(None, 50.0);
        let mut tail = vec![0.0; 8_820];
        run(&mut fx, &settings, &mut tail);
        assert!(peak(&tail[4_410..]) > 0.05, "the echo was cut, not rung out");
    }

    #[test]
    fn a_roll_repeats_the_last_beats_and_letting_go_is_the_live_signal() {
        let settings = PadSettings::default();
        let mut fx = PadFx::new(RATE);
        // A signal that says where it is: frame n reads n / 1e6.
        let coded = |from: usize, frames: usize| -> Vec<f32> {
            (from..from + frames).flat_map(|i| [i as f32 / 1e6, i as f32 / 1e6]).collect()
        };
        let mut audio = coded(0, 44_100);
        run(&mut fx, &settings, &mut audio);
        // One beat at 50 ms: 2205 frames.
        settings.set(Some((PadFxKind::Roll, 1.0)), 50.0);
        let mut rolled = coded(44_100, 22_050);
        run(&mut fx, &settings, &mut rolled);
        let left: Vec<f32> = rolled.chunks_exact(2).map(|f| f[0]).collect();
        let pressed_at = 44_100.0 / 1e6;
        assert!(left[200..].iter().all(|v| *v <= pressed_at), "the roll played something after the press");
        // Away from the seams, one loop later is the same frame.
        for i in [300, 1_000, 2_000, 5_000, 10_000] {
            assert_eq!(left[i], left[i + 2_205], "frame {i} did not repeat");
        }
        assert!((left[300] - (44_100.0 - 2_205.0 + 300.0) / 1e6).abs() < 1e-6, "the loop is not the last beat");
        settings.set(None, 50.0);
        let mut back = coded(66_150, 4_410);
        let live = back.clone();
        run(&mut fx, &settings, &mut back);
        assert_eq!(back[200..], live[200..], "the deck did not play on underneath the roll");
    }

    #[test]
    fn a_roll_loops_without_a_step_at_its_seam() {
        let settings = PadSettings::default();
        let mut fx = PadFx::new(RATE);
        let mut audio = sine(220.0, 44_100, 0);
        run(&mut fx, &settings, &mut audio);
        // A beat that is not a whole number of cycles: the seam is mid-wave.
        settings.set(Some((PadFxKind::Roll, 1.0)), 37.3);
        let mut rolled = sine(220.0, 22_050, 44_100);
        run(&mut fx, &settings, &mut rolled);
        settings.set(None, 37.3);
        let mut back = sine(220.0, 4_410, 66_150);
        run(&mut fx, &settings, &mut back);
        let mut all = audio[audio.len() - 2_000..].to_vec();
        all.extend(rolled);
        all.extend(back);
        // A 220 Hz sine at 0.8 moves 0.025 a frame; a crossfade adds at most
        // 1.6 / 88. A cut would be up to 1.6.
        let worst = worst_step(&all);
        assert!(worst < 0.05, "the roll stepped by {worst}");
    }

    #[test]
    fn trans_gates_half_of_every_period() {
        let settings = PadSettings::default();
        let mut fx = PadFx::new(RATE);
        settings.set(Some((PadFxKind::Trans, 1.0)), 50.0);
        let mut audio = vec![1.0; 2 * 2_205 * 3];
        run(&mut fx, &settings, &mut audio);
        let left: Vec<f32> = audio.chunks_exact(2).map(|f| f[0]).collect();
        for period in 0..3 {
            let base = period * 2_205;
            assert_eq!(left[base + 550], 1.0, "open in the first half of period {period}");
            assert_eq!(left[base + 1_650], 0.0, "closed in the second half of period {period}");
        }
        assert!(worst_step(&audio) <= 1.0 / 88.0 + 1e-6, "the gate stepped");
    }

    #[test]
    fn the_filter_pads_close_over_their_beats() {
        for (kind, hz) in [(PadFxKind::Hpf, 60.0), (PadFxKind::Lpf, 8_000.0)] {
            let settings = PadSettings::default();
            let mut fx = PadFx::new(RATE);
            // One beat of 100 ms: closed after 4410 frames.
            settings.set(Some((kind, 1.0)), 100.0);
            let mut sweep = sine(hz, 4_410, 0);
            run(&mut fx, &settings, &mut sweep);
            let mut held = sine(hz, 8_820, 4_410);
            run(&mut fx, &settings, &mut held);
            assert!(peak(&held[8_820..]) < 0.01, "{kind:?} let {hz} Hz through at {}", peak(&held[8_820..]));
            settings.set(None, 100.0);
            let mut open = sine(hz, 8_820, 13_230);
            run(&mut fx, &settings, &mut open);
            assert!(peak(&open[8_820..]) > 0.75, "{kind:?} did not open again");
        }
    }

    #[test]
    fn crush_takes_the_signal_to_six_bits() {
        let settings = PadSettings::default();
        let mut fx = PadFx::new(RATE);
        settings.set(Some((PadFxKind::Crush, 1.0)), 100.0);
        let mut audio = sine(440.0, 4_410, 0);
        run(&mut fx, &settings, &mut audio);
        for sample in &audio[400..] {
            let steps = sample * 32.0;
            assert!((steps - steps.round()).abs() < 1e-4, "{sample} is not on a 6-bit step");
        }
    }

    #[test]
    fn no_pad_steps_on_press_or_release() {
        // A steady level, so any step came from the effect switching.
        for kind in PadFxKind::ALL {
            let settings = PadSettings::default();
            let mut fx = PadFx::new(RATE);
            // Past the roll's whole history, so the level has no start in it:
            // a roll's seam reaches back before its loop.
            let mut warm = vec![0.8; RATE as usize * 9];
            run(&mut fx, &settings, &mut warm);
            let mut audio = vec![0.8; 8_820];
            run(&mut fx, &settings, &mut audio);
            settings.set(Some((kind, 1.0)), 100.0);
            let mut held = vec![0.8; 17_640];
            run(&mut fx, &settings, &mut held);
            settings.set(None, 100.0);
            let mut after = vec![0.8; 17_640];
            run(&mut fx, &settings, &mut after);
            audio.extend(held);
            audio.extend(after);
            let worst = worst_step(&audio);
            assert!(worst < 0.8 / 88.0 * 1.5, "{kind:?} stepped by {worst}");
        }
    }

    fn strip(smart: Option<SmartCfxKind>, filter: f32, audio: &mut [f32]) -> Channel {
        let settings = ChannelSettings::default();
        settings.set_filter(filter);
        let mut channel = Channel::new(RATE);
        for chunk in audio.chunks_mut(1024) {
            channel.process_strip_with_fx(chunk, &settings, Curve::Eq, smart, 500.0);
        }
        channel
    }

    #[test]
    fn smart_cfx_at_centre_is_the_plain_strip_bit_for_bit() {
        let settings = ChannelSettings::default();
        let mut plain = sine(440.0, 8_192, 0);
        let mut channel = Channel::new(RATE);
        channel.process_strip(&mut plain, &settings, Curve::Eq);
        for kind in SmartCfxKind::ALL {
            for at in [0.5, 0.51, 0.49] {
                let mut smart = sine(440.0, 8_192, 0);
                strip(Some(kind), at, &mut smart);
                assert_eq!(smart, plain, "{kind:?} at {at} touched the strip");
            }
        }
    }

    #[test]
    fn smart_noise_is_silent_at_centre_and_rises_off_it() {
        let mut centre = vec![0.0; 8_820];
        strip(Some(SmartCfxKind::Noise), 0.5, &mut centre);
        assert_eq!(peak(&centre), 0.0);
        let mut some = vec![0.0; 8_820];
        strip(Some(SmartCfxKind::Noise), 0.7, &mut some);
        let mut more = vec![0.0; 8_820];
        strip(Some(SmartCfxKind::Noise), 1.0, &mut more);
        let rms = |a: &[f32]| (a.iter().map(|s| s * s).sum::<f32>() / a.len() as f32).sqrt();
        assert!(rms(&some[4_410..]) > 0.01, "no noise off centre");
        assert!(peak(&some).is_finite() && peak(&more) < 2.0);
        let mut off = vec![0.0; 8_820];
        strip(None, 0.7, &mut off);
        assert_eq!(peak(&off), 0.0, "plain CFX made noise");
    }

    #[test]
    fn smart_echo_rings_on_after_the_knob_returns_and_then_leaves_the_strip() {
        let settings = ChannelSettings::default();
        let mut channel = Channel::new(RATE);
        settings.set_filter(0.8);
        let mut audio = sine(440.0, 22_050, 0);
        for chunk in audio.chunks_mut(1024) {
            channel.process_strip_with_fx(chunk, &settings, Curve::Eq, Some(SmartCfxKind::Echo), 200.0);
        }
        settings.set_filter(0.5);
        let mut tail = vec![0.0; 22_050];
        for chunk in tail.chunks_mut(1024) {
            channel.process_strip_with_fx(chunk, &settings, Curve::Eq, Some(SmartCfxKind::Echo), 200.0);
        }
        assert!(peak(&tail[2_000..]) > 0.01, "the echo was cut when the knob came back");
        let mut long = vec![0.0; RATE as usize * 4];
        for chunk in long.chunks_mut(1024) {
            channel.process_strip_with_fx(chunk, &settings, Curve::Eq, Some(SmartCfxKind::Echo), 200.0);
        }
        let mut after = sine(440.0, 4_096, 0);
        let mut plain = after.clone();
        Channel::new(RATE).process_strip(&mut plain, &ChannelSettings::default(), Curve::Eq);
        for chunk in after.chunks_mut(1024) {
            channel.process_strip_with_fx(chunk, &settings, Curve::Eq, Some(SmartCfxKind::Echo), 200.0);
        }
        // The strip's own filters carry state, so compare against silence in, not bits.
        assert!(after.iter().zip(&plain).all(|(a, b)| (a - b).abs() < 1e-3), "the echo never left");
    }

    #[test]
    fn smart_crush_and_reverb_change_the_signal_off_centre() {
        // A low pass at 900 Hz, so the 440 Hz tone is still there to change;
        // long enough for the echo's 375 ms to come back.
        for kind in [SmartCfxKind::Crush, SmartCfxKind::Reverb, SmartCfxKind::Echo] {
            let mut plain = sine(440.0, 22_050, 0);
            strip(None, 0.25, &mut plain);
            let mut smart = sine(440.0, 22_050, 0);
            strip(Some(kind), 0.25, &mut smart);
            let diff = smart.iter().zip(&plain).fold(0.0_f32, |a, (s, p)| a.max((s - p).abs()));
            assert!(diff > 0.01, "{kind:?} did nothing off centre");
            assert!(smart.iter().all(|s| s.is_finite() && s.abs() < 3.0));
        }
    }
}
