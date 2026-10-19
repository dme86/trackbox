//! BEAT FX: one effect unit, on deck A, deck B or the master, timed to the
//! beat the interface hands it.
//!
//! Everything it needs is allocated when it is built, at the first callback
//! with the device rate, and never again. The effect keeps running after it
//! is switched off: only its input closes, so an echo or a reverb rings out
//! as it does on a DJM instead of being cut.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::smooth::Smoothed;

/// The longest echo, in seconds: two bars at 60 BPM.
const MAX_DELAY_SECONDS: f32 = 8.0;

/// Below this a tail is gone; see `mixer::RUNG_OUT` for why it is flushed.
const RUNG_OUT: f32 = 1e-18;

fn flush(value: f32) -> f32 {
    if value.abs() < RUNG_OUT { 0.0 } else { value }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FxKind {
    Echo,
    Reverb,
}

/// Where the unit is inserted: after a channel's fader, or on the sum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FxTarget {
    A,
    B,
    Master,
}

/// The unit's controls, as the interface and the controller leave them.
#[derive(Debug)]
pub struct FxSettings {
    on: AtomicBool,
    kind: AtomicU32,
    target: AtomicU32,
    depth: AtomicU32,
    beat_ms: AtomicU32,
}

impl Default for FxSettings {
    fn default() -> Self {
        Self {
            on: AtomicBool::new(false),
            kind: AtomicU32::new(0),
            target: AtomicU32::new(2),
            depth: AtomicU32::new(0.5_f32.to_bits()),
            beat_ms: AtomicU32::new(500.0_f32.to_bits()),
        }
    }
}

impl FxSettings {
    pub fn set_on(&self, on: bool) {
        self.on.store(on, Ordering::Relaxed);
    }

    pub fn on(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    pub fn set_kind(&self, kind: FxKind) {
        self.kind.store(u32::from(kind == FxKind::Reverb), Ordering::Relaxed);
    }

    pub fn kind(&self) -> FxKind {
        if self.kind.load(Ordering::Relaxed) == 1 { FxKind::Reverb } else { FxKind::Echo }
    }

    pub fn set_target(&self, target: FxTarget) {
        let wire = match target {
            FxTarget::A => 0,
            FxTarget::B => 1,
            FxTarget::Master => 2,
        };
        self.target.store(wire, Ordering::Relaxed);
    }

    pub fn target(&self) -> FxTarget {
        match self.target.load(Ordering::Relaxed) {
            0 => FxTarget::A,
            1 => FxTarget::B,
            _ => FxTarget::Master,
        }
    }

    /// LEVEL/DEPTH, 0 to 1: how much of the effect is heard.
    pub fn set_depth(&self, depth: f32) {
        let safe = if depth.is_finite() { depth.clamp(0.0, 1.0) } else { 0.5 };
        self.depth.store(safe.to_bits(), Ordering::Relaxed);
    }

    pub fn depth(&self) -> f32 {
        f32::from_bits(self.depth.load(Ordering::Relaxed))
    }

    /// The effect's time: the echo's delay, the reverb's size. The interface
    /// works it out from the beat setting and the BPM.
    pub fn set_beat_ms(&self, ms: f32) {
        let safe = if ms.is_finite() { ms.clamp(10.0, MAX_DELAY_SECONDS * 1000.0) } else { 500.0 };
        self.beat_ms.store(safe.to_bits(), Ordering::Relaxed);
    }

    pub fn beat_ms(&self) -> f32 {
        f32::from_bits(self.beat_ms.load(Ordering::Relaxed))
    }

    pub fn copy_from(&self, other: &FxSettings) {
        self.set_on(other.on());
        self.set_kind(other.kind());
        self.set_target(other.target());
        self.set_depth(other.depth());
        self.set_beat_ms(other.beat_ms());
    }
}

/// A stereo delay line with feedback, read between samples so a change of
/// time glides rather than clicks.
pub(crate) struct Echo {
    line: Vec<f32>,
    write: usize,
    delay: Smoothed,
}

impl Echo {
    pub(crate) fn new(rate: u32) -> Self {
        let frames = (MAX_DELAY_SECONDS * rate as f32) as usize + 2;
        Self { line: vec![0.0; frames * 2], write: 0, delay: Smoothed::new(rate as f32 / 2.0, rate) }
    }

    fn frames(&self) -> usize {
        self.line.len() / 2
    }

    fn tap(&self, back: f32) -> (f32, f32) {
        let frames = self.frames();
        let back = back.clamp(1.0, (frames - 2) as f32);
        let whole = back as usize;
        let frac = back - whole as f32;
        let at = |n: usize| {
            let i = (self.write + frames - n) % frames;
            (self.line.get(i * 2).copied().unwrap_or(0.0), self.line.get(i * 2 + 1).copied().unwrap_or(0.0))
        };
        let (a, b) = (at(whole), at(whole + 1));
        (a.0 + (b.0 - a.0) * frac, a.1 + (b.1 - a.1) * frac)
    }

    /// One frame: what comes back, after feeding in `input`.
    pub(crate) fn run(&mut self, input: (f32, f32), delay_frames: f32, feedback: f32) -> (f32, f32) {
        let back = self.delay.step(delay_frames);
        let out = self.tap(back);
        let frames = self.frames();
        if let Some(slot) = self.line.get_mut(self.write * 2) {
            *slot = flush(input.0 + out.0 * feedback);
        }
        if let Some(slot) = self.line.get_mut(self.write * 2 + 1) {
            *slot = flush(input.1 + out.1 * feedback);
        }
        self.write = (self.write + 1) % frames;
        out
    }

    fn clear(&mut self) {
        self.line.fill(0.0);
    }
}

/// A feedback comb with a damped loop, Freeverb's building block.
struct Comb {
    line: Vec<f32>,
    at: usize,
    store: f32,
}

impl Comb {
    fn new(frames: usize) -> Self {
        Self { line: vec![0.0; frames.max(1)], at: 0, store: 0.0 }
    }

    fn run(&mut self, input: f32, feedback: f32, damp: f32) -> f32 {
        let Some(slot) = self.line.get_mut(self.at) else { return 0.0 };
        let out = *slot;
        self.store = flush(out * (1.0 - damp) + self.store * damp);
        *slot = flush(input + self.store * feedback);
        self.at = (self.at + 1) % self.line.len();
        out
    }
}

struct Allpass {
    line: Vec<f32>,
    at: usize,
}

impl Allpass {
    fn new(frames: usize) -> Self {
        Self { line: vec![0.0; frames.max(1)], at: 0 }
    }

    fn run(&mut self, input: f32) -> f32 {
        let Some(slot) = self.line.get_mut(self.at) else { return input };
        let held = *slot;
        *slot = flush(input + held * 0.5);
        self.at = (self.at + 1) % self.line.len();
        held - input
    }
}

/// Freeverb's tunings, at 44.1 kHz; the right side is spread by 23 samples.
const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASSES: [usize; 4] = [556, 441, 341, 225];
const SPREAD: usize = 23;
/// Freeverb's input gain and wet scale: eight combs ring far louder than
/// what goes in, and the wet side is brought back up after the allpasses.
const REVERB_IN: f32 = 0.015;
const REVERB_WET: f32 = 3.0;

pub(crate) struct Reverb {
    combs: [Vec<Comb>; 2],
    allpasses: [Vec<Allpass>; 2],
}

impl Reverb {
    pub(crate) fn new(rate: u32) -> Self {
        let scale = |n: usize| (n as f32 * rate as f32 / 44_100.0) as usize;
        let side = |spread: usize| {
            (
                COMBS.iter().map(|n| Comb::new(scale(n + spread))).collect::<Vec<_>>(),
                ALLPASSES.iter().map(|n| Allpass::new(scale(n + spread))).collect::<Vec<_>>(),
            )
        };
        let (left_c, left_a) = side(0);
        let (right_c, right_a) = side(SPREAD);
        Self { combs: [left_c, right_c], allpasses: [left_a, right_a] }
    }

    pub(crate) fn run(&mut self, input: (f32, f32), room: f32) -> (f32, f32) {
        // Mono in, as Freeverb takes it; the two sides differ by their tunings.
        let mono = (input.0 + input.1) * REVERB_IN;
        let mut out = [0.0_f32; 2];
        for (side, slot) in out.iter_mut().enumerate() {
            let (Some(combs), Some(allpasses)) = (self.combs.get_mut(side), self.allpasses.get_mut(side)) else {
                continue;
            };
            let mut sum = 0.0;
            for comb in combs.iter_mut() {
                sum += comb.run(mono, room, 0.2);
            }
            for allpass in allpasses.iter_mut() {
                sum = allpass.run(sum);
            }
            *slot = sum * REVERB_WET;
        }
        (out[0], out[1])
    }

    fn clear(&mut self) {
        for comb in self.combs.iter_mut().flatten() {
            comb.line.fill(0.0);
            comb.store = 0.0;
        }
        for allpass in self.allpasses.iter_mut().flatten() {
            allpass.line.fill(0.0);
        }
    }
}

/// The unit as the callback owns it.
pub struct BeatFx {
    echo: Echo,
    reverb: Reverb,
    send: Smoothed,
    wet: Smoothed,
    rate: f32,
    kind: FxKind,
    target: FxTarget,
}

impl BeatFx {
    pub fn new(rate: u32) -> Self {
        Self {
            echo: Echo::new(rate),
            reverb: Reverb::new(rate),
            send: Smoothed::new(0.0, rate),
            wet: Smoothed::new(0.0, rate),
            rate: rate.max(1) as f32,
            kind: FxKind::Echo,
            target: FxTarget::Master,
        }
    }

    /// Runs the unit over `out` in place, if `here` is where it is inserted.
    /// A change of effect or of insert point starts from silence: a tail
    /// carried from one to the other would be heard where it does not belong.
    pub fn process(&mut self, out: &mut [f32], settings: &FxSettings, here: FxTarget) {
        let target = settings.target();
        if target != here {
            return;
        }
        let kind = settings.kind();
        if kind != self.kind || target != self.target {
            self.kind = kind;
            self.target = target;
            self.echo.clear();
            self.reverb.clear();
        }
        let send = if settings.on() { 1.0 } else { 0.0 };
        if send == 0.0 && self.send.value() <= 0.0 && self.wet.value() <= 0.0 {
            return;
        }
        let depth = settings.depth();
        let beat_frames = settings.beat_ms() * self.rate / 1000.0;
        // The tail outlives the switch: the wet level only goes when the
        // unit is off and its echo has had time to die away.
        let wet_target = depth;
        let feedback = 0.3 + 0.45 * depth;
        let room = 0.7 + 0.27 * (settings.beat_ms() / 2000.0).clamp(0.0, 1.0);
        for frame in out.chunks_exact_mut(2) {
            let [left, right] = frame else { continue };
            let gate = self.send.step(send);
            let wet = self.wet.step(wet_target);
            let input = (*left * gate, *right * gate);
            let effect = match kind {
                FxKind::Echo => self.echo.run(input, beat_frames, feedback),
                FxKind::Reverb => self.reverb.run(input, room),
            };
            *left += effect.0 * wet;
            *right += effect.1 * wet;
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::unwrap_used)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn click(frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * 2];
        out[0] = 1.0;
        out[1] = 1.0;
        out
    }

    #[test]
    fn off_and_never_on_is_the_signal_untouched() {
        let settings = FxSettings::default();
        let mut fx = BeatFx::new(RATE);
        let mut audio = click(4_800);
        let before = audio.clone();
        fx.process(&mut audio, &settings, FxTarget::Master);
        assert_eq!(audio, before);
    }

    #[test]
    fn an_echo_repeats_one_beat_later() {
        let settings = FxSettings::default();
        settings.set_on(true);
        settings.set_beat_ms(100.0);
        settings.set_depth(1.0);
        let mut fx = BeatFx::new(RATE);
        // Past the gate's and the wet level's smoothing first.
        let mut warm = vec![0.0; 9_600];
        fx.process(&mut warm, &settings, FxTarget::Master);
        let mut audio = click(9_600);
        fx.process(&mut audio, &settings, FxTarget::Master);
        let beat = 4_800;
        let (peak_at, peak) = audio
            .chunks_exact(2)
            .enumerate()
            .skip(10)
            .fold((0, 0.0_f32), |(at, best), (i, f)| if f[0].abs() > best { (i, f[0].abs()) } else { (at, best) });
        assert!(peak > 0.5, "the echo came back at {peak}");
        assert!(peak_at.abs_diff(beat) <= 1, "the echo came back at frame {peak_at}, not {beat}");
    }

    #[test]
    fn an_echo_switched_off_rings_out_and_goes_quiet() {
        let settings = FxSettings::default();
        settings.set_on(true);
        settings.set_beat_ms(50.0);
        let mut fx = BeatFx::new(RATE);
        let mut audio = vec![0.5; 9_600];
        fx.process(&mut audio, &settings, FxTarget::Master);
        settings.set_on(false);
        let mut tail = vec![0.0; 4_800];
        fx.process(&mut tail, &settings, FxTarget::Master);
        assert!(tail.iter().any(|s| s.abs() > 0.01), "the echo was cut, not rung out");
        let mut later = vec![0.0; RATE as usize * 4];
        fx.process(&mut later, &settings, FxTarget::Master);
        let end = later.get(later.len() - 960..).unwrap();
        assert!(end.iter().all(|s| s.abs() < 1e-3), "the echo never died away");
    }

    #[test]
    fn a_reverb_spreads_a_click_into_a_tail() {
        let settings = FxSettings::default();
        settings.set_on(true);
        settings.set_kind(FxKind::Reverb);
        settings.set_depth(1.0);
        let mut fx = BeatFx::new(RATE);
        let mut warm = vec![0.0; 9_600];
        fx.process(&mut warm, &settings, FxTarget::Master);
        let mut audio = click(RATE as usize / 2);
        fx.process(&mut audio, &settings, FxTarget::Master);
        let late = audio.get(RATE as usize / 4..).unwrap();
        assert!(late.iter().any(|s| s.abs() > 1e-4), "no tail a quarter second on");
        assert!(audio.iter().all(|s| s.is_finite() && s.abs() < 2.0));
    }

    #[test]
    fn a_reverb_on_music_is_loud_enough_to_hear() {
        // A sustained tone through a full-depth reverb: the tail has to sit
        // within a few decibels of the dry signal, or nobody hears it.
        let settings = FxSettings::default();
        settings.set_on(true);
        settings.set_kind(FxKind::Reverb);
        settings.set_depth(1.0);
        let mut fx = BeatFx::new(RATE);
        let mut audio: Vec<f32> = (0..RATE as usize)
            .flat_map(|i| {
                let v = 0.5 * (i as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin();
                [v, v]
            })
            .collect();
        fx.process(&mut audio, &settings, FxTarget::Master);
        settings.set_on(false);
        let mut tail = vec![0.0; 9_600];
        fx.process(&mut tail, &settings, FxTarget::Master);
        let peak = tail.iter().fold(0.0_f32, |a, s| a.max(s.abs()));
        assert!(peak > 0.1, "the reverb's tail peaked at {peak}");
    }

    #[test]
    fn the_unit_only_works_where_it_is_inserted() {
        let settings = FxSettings::default();
        settings.set_on(true);
        settings.set_target(FxTarget::A);
        let mut fx = BeatFx::new(RATE);
        let mut audio = vec![0.5; 9_600];
        let before = audio.clone();
        fx.process(&mut audio, &settings, FxTarget::B);
        assert_eq!(audio, before);
    }
}
