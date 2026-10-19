//! Where the mixed audio goes.
//!
//! Behind a trait so the engine can be tested without an audio device: the
//! null sink pulls the same callback the real one does, on the calling thread,
//! so a test can ask for exactly 512 frames and look at them.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use crate::health::{AudioHealth, HealthMeter};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::{DeckError, Result};

/// Fills a stereo interleaved master buffer and, after it, a stereo cue
/// buffer for the headphones. The cue buffer is the same length as the
/// master, or empty when the sink has nowhere to send it. Called on the audio
/// thread, so it obeys the realtime rules: no allocation, no lock, no
/// syscall, no panic.
pub type Render = Box<dyn FnMut(&mut [f32], &mut [f32]) + Send>;

/// Where the headphones are heard, if anywhere.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CueRouting {
    /// No headphones: the master alone, on the first two outputs.
    #[default]
    Off,
    /// One device with separate output pairs, counted from zero. A DDJ-FLX4
    /// is master on 0/1 and headphones on 2/3.
    Lanes { master: (u16, u16), cue: (u16, u16) },
    /// One stereo output: the cue in the left ear and the master in the
    /// right, each summed to mono.
    Split,
}

impl CueRouting {
    /// How many outputs a device needs to carry this.
    #[must_use]
    pub fn channels_needed(self) -> u16 {
        match self {
            Self::Off | Self::Split => 2,
            Self::Lanes { master, cue } => {
                master.0.max(master.1).max(cue.0).max(cue.1).saturating_add(1)
            }
        }
    }

    #[must_use]
    pub fn has_cue(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// What output `at` carries, given one frame of master and of cue.
    fn lane(self, at: usize, master: (f32, f32), cue: (f32, f32)) -> f32 {
        match self {
            Self::Off => match at {
                0 => master.0,
                1 => master.1,
                _ => 0.0,
            },
            Self::Split => match at {
                0 => f32::midpoint(cue.0, cue.1),
                1 => f32::midpoint(master.0, master.1),
                _ => 0.0,
            },
            Self::Lanes { master: (ml, mr), cue: (cl, cr) } => {
                let at = u16::try_from(at).unwrap_or(u16::MAX);
                if at == ml {
                    master.0
                } else if at == mr {
                    master.1
                } else if at == cl {
                    cue.0
                } else if at == cr {
                    cue.1
                } else {
                    0.0
                }
            }
        }
    }
}

pub trait Sink: Send + Sync {
    fn health(&self) -> AudioHealth { AudioHealth::default() }
    /// Where the headphones actually went, which is not always what was asked
    /// for: a device with too few outputs is opened without them.
    fn cue_routing(&self) -> CueRouting { CueRouting::Off }
    /// The rate everything downstream of the resampler runs at.
    fn sample_rate(&self) -> u32;
    /// Whether the output has stopped for good — unplugged, or reconfigured
    /// under the stream — so nothing will be heard until the engine is
    /// rebuilt on a device that is there.
    fn lost(&self) -> bool { false }
    /// Starts pulling. Called when a deck starts playing.
    fn start(&self) -> Result<()>;
    /// Stops pulling, so an idle app costs nothing.
    fn stop(&self) -> Result<()>;
}

/// How long the stream runs on after the last deck has stopped.
///
/// A stop is a fade, and a fade needs callbacks to happen in: pausing the
/// stream the instant the transport asks cuts the last two milliseconds off,
/// which is the click the fade exists to prevent. Sixty milliseconds is
/// several callbacks at any buffer size a device is likely to choose, and a
/// stream that lives that much longer costs nothing anyone can measure.
pub const LINGER: Duration = Duration::from_millis(60);

/// The same wait, in frames, for a sink that is pulled by hand.
pub const fn linger_frames(sample_rate: u32) -> usize {
    (sample_rate as usize * LINGER.as_millis() as usize) / 1000
}

/// What the thread that owns the cpal stream is asked to do.
enum Ask {
    Start,
    Stop,
    Quit,
}

/// The real device.
///
/// The stream lives on its own thread because `cpal::Stream` is not `Send` on
/// every platform — Core Audio's is not — and the engine has to be `Send` and
/// `Sync` to sit in Tauri's state. The thread owns it and takes instructions.
pub struct CpalSink {
    health: Arc<HealthMeter>,
    ask: Sender<Ask>,
    sample_rate: u32,
    routing: CueRouting,
    running: AtomicBool,
    /// Set by the stream's error callback, which is not the audio callback.
    lost: Arc<AtomicBool>,
}

impl CpalSink {
    /// Opens the default output device.
    pub fn open(render: Render) -> Result<Self> {
        Self::open_named(render, None)
    }

    /// Opens one output by id, or the default when `wanted` is `None`.
    ///
    /// By id rather than by position: a device list renumbers whenever
    /// something is plugged in, and a stored index would pick a different box
    /// after a reboot. An id that is not there is `DeckError::Disconnected`,
    /// never the default: see `pick`.
    pub fn open_named(render: Render, wanted: Option<String>) -> Result<Self> {
        Self::open_with(render, wanted, StreamWish::default())
    }

    /// The same, asking the device for a rate and a buffer size.
    ///
    /// Asked, not demanded: a device that does not offer the rate is opened
    /// at its default, and a buffer size outside what it supports is left to
    /// it, with a warning either way. The rate actually opened is what
    /// `sample_rate` reports and what the decks resample to.
    pub fn open_with(render: Render, wanted: Option<String>, wish: StreamWish) -> Result<Self> {
        let (ask_tx, ask_rx) = std::sync::mpsc::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();

        let health = Arc::new(HealthMeter::default());
        let callback_health = Arc::clone(&health);
        let lost = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&lost);
        std::thread::Builder::new()
            .name("rbl-deck-device".to_owned())
            .spawn(move || device_thread(render, wanted.as_deref(), wish, &ask_rx, &ready_tx, callback_health, flag))
            .map_err(DeckError::Io)?;

        // The rate decides what the decoders resample to, so opening is not
        // finished until the device has said what it is.
        let (sample_rate, routing) = ready_rx
            .recv()
            .map_err(|_| DeckError::Device("the audio thread stopped while starting".to_owned()))??;

        Ok(Self { health, ask: ask_tx, sample_rate, routing, running: AtomicBool::new(false), lost })
    }
}

impl Sink for CpalSink {
    fn health(&self) -> AudioHealth { self.health.snapshot(self.running.load(Ordering::Relaxed)) }
    fn lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }
    fn cue_routing(&self) -> CueRouting {
        self.routing
    }
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn start(&self) -> Result<()> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.ask
            .send(Ask::Start)
            .map_err(|_| DeckError::Device("the audio thread has stopped".to_owned()))
    }

    fn stop(&self) -> Result<()> {
        if !self.running.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        self.ask
            .send(Ask::Stop)
            .map_err(|_| DeckError::Device("the audio thread has stopped".to_owned()))
    }
}

impl Drop for CpalSink {
    fn drop(&mut self) {
        let _ = self.ask.send(Ask::Quit);
    }
}

/// Owns the stream and does as it is told.
fn device_thread(
    render: Render,
    wanted: Option<&str>,
    wish: StreamWish,
    ask: &Receiver<Ask>,
    ready: &Sender<Result<(u32, CueRouting)>>,
    health: Arc<HealthMeter>,
    lost: Arc<AtomicBool>,
) {
    let stream = match build_stream(render, wanted, wish, health, lost) {
        Ok((stream, rate, routing)) => {
            if ready.send(Ok((rate, routing))).is_err() {
                return;
            }
            stream
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };

    // A stop is not taken at once: see `LINGER`. Until it is due, the thread
    // waits on the channel rather than on the clock, so a start that arrives
    // in the meantime simply cancels it.
    let mut due: Option<Instant> = None;
    loop {
        let next = match due {
            Some(at) => match ask.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(next) => next,
                Err(RecvTimeoutError::Timeout) => {
                    due = None;
                    if let Err(e) = stream.pause() {
                        tracing::error!(error = %e, "the audio device would not stop");
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match ask.recv() {
                Ok(next) => next,
                Err(_) => break,
            },
        };
        match next {
            Ask::Start => {
                due = None;
                if let Err(e) = stream.play() {
                    tracing::error!(error = %e, "the audio device would not start");
                }
            }
            Ask::Stop => due = Some(Instant::now() + LINGER),
            Ask::Quit => break,
        }
    }
    // Dropping the stream here, on the thread that built it.
    drop(stream);
}

/// Builds an output stream on the default device, in whatever format it wants.
/// One output the audio can go to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    /// What to store and what to open by: cpal's own id, which it documents as
    /// stable across runs, disconnections and reboots. A position in a list is
    /// not — plugging an interface in renumbers everything after it.
    pub id: String,
    /// What to show. The device's own name, which is what a person recognises.
    pub name: String,
}

/// Every output the default host offers.
///
/// A device that will not give an id is skipped: it cannot be stored, so
/// offering it would be offering a choice that does not survive a restart.
#[must_use]
pub fn output_devices() -> Vec<AudioDevice> {
    let host = cpal::default_host();
    let Ok(devices) = host.output_devices() else { return Vec::new() };
    devices
        .filter_map(|device| {
            let id = device.id().ok()?;
            Some(AudioDevice { id: id.to_string(), name: device.to_string() })
        })
        .collect()
}

/// The one the engine opens when nothing has been chosen.
#[must_use]
pub fn default_output_device() -> Option<AudioDevice> {
    let device = cpal::default_host().default_output_device()?;
    let id = device.id().ok()?;
    Some(AudioDevice { id: id.to_string(), name: device.to_string() })
}

/// What Preferences › Audio asks of the device: a sample rate and a buffer
/// size, either of which may be left to the device, and where the headphones go.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamWish {
    pub sample_rate: Option<u32>,
    /// Frames per callback.
    pub buffer_frames: Option<u32>,
    pub cue: CueRouting,
}

/// The sample formats `build` can write.
fn writable(format: cpal::SampleFormat) -> bool {
    matches!(
        format,
        cpal::SampleFormat::F32 | cpal::SampleFormat::I16 | cpal::SampleFormat::U16 | cpal::SampleFormat::I32
    )
}

/// The device's configuration for a wish: its default, at the wished rate
/// if it offers that rate, with the wished buffer if that is in its range,
/// and widened to enough outputs for the headphones if it has them.
///
/// A device's default configuration can be stereo even when it has more
/// outputs, so separate headphone lanes are looked for among everything it
/// supports. A device without enough outputs is opened without headphones,
/// with a warning, and the routing that comes back says so.
fn configure(
    device: &cpal::Device,
    wish: StreamWish,
) -> Result<(cpal::StreamConfig, cpal::SampleFormat, CueRouting)> {
    let supported = device
        .default_output_config()
        .map_err(|e| DeckError::Device(e.to_string()))?;
    let mut format = supported.sample_format();
    let mut buffer_range = *supported.buffer_size();
    let mut config: cpal::StreamConfig = supported.into();
    let mut routing = wish.cue;

    let needed = routing.channels_needed();
    if config.channels < needed {
        let rate = wish.sample_rate.unwrap_or(config.sample_rate);
        let wider = device.supported_output_configs().ok().and_then(|ranges| {
            ranges
                .filter(|range| {
                    range.channels() >= needed
                        && writable(range.sample_format())
                        && range.min_sample_rate() <= rate
                        && rate <= range.max_sample_rate()
                })
                .min_by_key(|range| (range.channels(), range.sample_format() != format))
        });
        if let Some(range) = wider {
            config.channels = range.channels();
            config.sample_rate = rate;
            format = range.sample_format();
            buffer_range = *range.buffer_size();
        } else {
            tracing::warn!(
                needed,
                offered = config.channels,
                "the audio device has too few outputs for separate headphones; opening without them"
            );
            routing = CueRouting::Off;
        }
    }
    if let Some(rate) = wish.sample_rate {
        // Among the device's ranges, one at the default format that covers
        // the rate; a device that cannot run there keeps its default.
        let offers = device.supported_output_configs().is_ok_and(|mut ranges| {
            ranges.any(|range| {
                range.sample_format() == format
                    && range.channels() == config.channels
                    && range.min_sample_rate() <= rate
                    && rate <= range.max_sample_rate()
            })
        });
        if offers {
            config.sample_rate = rate;
        } else {
            tracing::warn!(rate, opened = config.sample_rate, "the audio device does not offer that sample rate");
        }
    }
    if let Some(frames) = wish.buffer_frames {
        match buffer_range {
            cpal::SupportedBufferSize::Range { min, max } if (min..=max).contains(&frames) => {
                config.buffer_size = cpal::BufferSize::Fixed(frames);
            }
            cpal::SupportedBufferSize::Range { min, max } => {
                tracing::warn!(frames, min, max, "the audio device does not offer that buffer size");
            }
            cpal::SupportedBufferSize::Unknown => {
                // The device will not say; asking is harmless, and it may take it.
                config.buffer_size = cpal::BufferSize::Fixed(frames);
            }
        }
    }
    Ok((config, format, routing))
}

/// Which output to open. Nothing chosen is the host's default, which the OS
/// moves for us when it changes. Something chosen is that device or an error:
/// falling back would move a live set onto whatever else is there — the
/// laptop's speakers — which is worse than a silence the DJ is told about.
fn pick<D>(
    wanted: Option<&str>,
    devices: impl IntoIterator<Item = (String, D)>,
    default: impl FnOnce() -> Option<D>,
) -> Result<D> {
    match wanted {
        None => default().ok_or(DeckError::NoDevice),
        Some(id) => devices
            .into_iter()
            .find_map(|(found, device)| (found == id).then_some(device))
            .ok_or_else(|| DeckError::Disconnected(id.to_owned())),
    }
}

fn build_stream(
    render: Render,
    wanted: Option<&str>,
    wish: StreamWish,
    health: Arc<HealthMeter>,
    lost: Arc<AtomicBool>,
) -> Result<(cpal::Stream, u32, CueRouting)> {
    let host = cpal::default_host();
    let devices = host
        .output_devices()
        .into_iter()
        .flatten()
        .filter_map(|device| Some((device.id().ok()?.to_string(), device)));
    let device = pick(wanted, devices, || host.default_output_device())?;
    let (config, format, routing) = configure(&device, wish)?;
    let rate = config.sample_rate;
    let channels = config.channels;
    tracing::debug!(device = %device, rate, channels, ?routing, buffer = ?config.buffer_size, "audio output opened");

    // Called by cpal off the audio callback on Core Audio, which pauses the
    // stream itself before reporting a disconnect; on any host a store is safe.
    let error = move |e: cpal::Error| match e.kind() {
        cpal::ErrorKind::Xrun => tracing::warn!(error = %e, "audio buffer underrun or overrun"),
        cpal::ErrorKind::DeviceChanged => tracing::info!(error = %e, "audio output changed"),
        cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::StreamInvalidated => {
            lost.store(true, Ordering::Release);
            tracing::error!(error = %e, "the audio output went away; output stopped");
        }
        _ => tracing::error!(error = %e, "audio device error"),
    };
    let stream = match format {
        cpal::SampleFormat::F32 => build::<f32>(&device, config, routing, render, error, health),
        cpal::SampleFormat::I16 => build::<i16>(&device, config, routing, render, error, health),
        cpal::SampleFormat::U16 => build::<u16>(&device, config, routing, render, error, health),
        cpal::SampleFormat::I32 => build::<i32>(&device, config, routing, render, error, health),
        other => Err(DeckError::Device(format!("this device wants {other} samples, which we do not write"))),
    }?;
    Ok((stream, rate, routing))
}

/// The device's own buffer never reaches this in practice; a callback asking
/// for more is served in several passes rather than by allocating.
const SCRATCH_FRAMES: usize = 4096;

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    routing: CueRouting,
    mut render: Render,
    error: impl FnMut(cpal::Error) + Send + 'static,
    health: Arc<HealthMeter>,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    // Allocated here, on the control thread, and only written to inside the
    // callback: the callback itself never allocates.
    let mut scratch = vec![0.0_f32; SCRATCH_FRAMES * 2];
    let mut cue_scratch = vec![0.0_f32; if routing.has_cue() { SCRATCH_FRAMES * 2 } else { 0 }];
    let lanes = config.channels.max(1) as usize;
    let rate = config.sample_rate;

    device
        .build_output_stream(
            config,
            move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
                let started = Instant::now();
                for chunk in out.chunks_mut(SCRATCH_FRAMES * lanes) {
                    let frames = chunk.len() / lanes;
                    // The chunk is bounded by the scratch, so this cannot be
                    // short; silence is the right answer if it ever were.
                    let Some(stereo) = scratch.get_mut(..frames * 2) else {
                        for sample in chunk.iter_mut() {
                            *sample = T::from_sample(0.0_f32);
                        }
                        continue;
                    };
                    stereo.fill(0.0);
                    let cue = cue_scratch.get_mut(..frames * 2).unwrap_or(&mut []);
                    render(stereo, cue);
                    // Master and cue into however many lanes the device has:
                    // lanes neither is routed to stay silent.
                    for (i, lane) in chunk.chunks_mut(lanes).enumerate() {
                        let pair = |buffer: &[f32]| {
                            (
                                buffer.get(i * 2).copied().unwrap_or(0.0),
                                buffer.get(i * 2 + 1).copied().unwrap_or(0.0),
                            )
                        };
                        let master = pair(&*stereo);
                        let heard = pair(&*cue);
                        for (at, sample) in lane.iter_mut().enumerate() {
                            *sample = T::from_sample(routing.lane(at, master, heard));
                        }
                    }
                }
                health.record(started.elapsed(), out.len() / lanes, rate);
            },
            error,
            None,
        )
        .map_err(|e| DeckError::Device(e.to_string()))
}

/// A sink with no device behind it: the test pulls it by hand.
pub struct NullSink {
    render: Mutex<Render>,
    sample_rate: u32,
    running: AtomicBool,
    /// Frames still to be pulled after a stop, so a fade has somewhere to
    /// happen. The device does the same thing: see `LINGER`.
    linger: AtomicUsize,
    lost: AtomicBool,
}

impl NullSink {
    pub fn new(sample_rate: u32, render: Render) -> Self {
        Self {
            render: Mutex::new(render),
            sample_rate,
            running: AtomicBool::new(false),
            linger: AtomicUsize::new(0),
            lost: AtomicBool::new(false),
        }
    }

    /// Unplugs it: from here on it is never pulled again, as a dead stream is not.
    pub fn lose(&self) {
        self.lost.store(true, Ordering::SeqCst);
    }

    /// Pulls `frames` stereo frames, as a device callback would.
    ///
    /// Silence once the stream is down. A stream that was just stopped is
    /// still pulled for `LINGER`, which is where the decks fade themselves
    /// out; the real device behaves the same way.
    pub fn pull(&self, frames: usize) -> Vec<f32> {
        self.pull_both(frames, false).0
    }

    /// The same, with headphones: the master and the cue, both stereo.
    pub fn pull_with_cue(&self, frames: usize) -> (Vec<f32>, Vec<f32>) {
        self.pull_both(frames, true)
    }

    fn pull_both(&self, frames: usize, phones: bool) -> (Vec<f32>, Vec<f32>) {
        let mut out = vec![0.0_f32; frames * 2];
        let mut cue = vec![0.0_f32; if phones { frames * 2 } else { 0 }];
        if self.lost.load(Ordering::SeqCst) {
            return (out, cue);
        }
        if !self.running.load(Ordering::SeqCst) {
            let left = self.linger.load(Ordering::SeqCst);
            if left == 0 {
                return (out, cue);
            }
            self.linger.store(left.saturating_sub(frames), Ordering::SeqCst);
        }
        if let Ok(mut render) = self.render.lock() {
            render(&mut out, &mut cue);
        }
        (out, cue)
    }

    pub fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

impl Sink for NullSink {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }

    fn start(&self) -> Result<()> {
        self.linger.store(0, Ordering::SeqCst);
        self.running.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        if self.running.swap(false, Ordering::SeqCst) {
            self.linger.store(linger_frames(self.sample_rate), Ordering::SeqCst);
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn a_stopped_null_sink_gives_silence_and_does_not_pull() {
        let sink = NullSink::new(44_100, Box::new(|out, _cue| out.fill(0.5)));
        assert_eq!(sink.pull(4), vec![0.0; 8]);
        sink.start().expect("start");
        assert_eq!(sink.pull(4), vec![0.5; 8]);
    }

    #[test]
    fn a_null_sink_hands_over_a_cue_buffer_only_when_asked() {
        let sink = NullSink::new(
            44_100,
            Box::new(|out, cue| {
                out.fill(0.5);
                cue.fill(0.25);
            }),
        );
        sink.start().expect("start");
        assert_eq!(sink.pull(4), vec![0.5; 8]);
        assert_eq!(sink.pull_with_cue(4), (vec![0.5; 8], vec![0.25; 8]));
    }

    #[test]
    fn a_lost_null_sink_is_never_pulled_again() {
        let sink = NullSink::new(44_100, Box::new(|out, _cue| out.fill(0.5)));
        sink.start().expect("start");
        assert!(!sink.lost());
        sink.lose();
        assert!(sink.lost());
        assert_eq!(sink.pull(4), vec![0.0; 8]);
    }

    #[test]
    fn nothing_chosen_is_the_default_and_its_absence_is_no_device() {
        let none = std::iter::empty::<(String, &str)>();
        assert_eq!(pick(None, none.clone(), || Some("speakers")).unwrap(), "speakers");
        assert!(matches!(pick(None, none, || None::<&str>), Err(DeckError::NoDevice)));
    }

    #[test]
    fn a_chosen_device_is_opened_when_it_is_there() {
        let devices = [("builtin".to_owned(), "speakers"), ("flx4".to_owned(), "DDJ-FLX4")];
        assert_eq!(pick(Some("flx4"), devices, || Some("speakers")).unwrap(), "DDJ-FLX4");
    }

    #[test]
    fn a_chosen_device_that_is_gone_never_falls_back_to_the_default() {
        let devices = [("builtin".to_owned(), "speakers")];
        let picked = pick(Some("flx4"), devices, || Some("speakers"));
        assert!(matches!(picked, Err(DeckError::Disconnected(ref id)) if id == "flx4"), "{picked:?}");
    }

    #[test]
    fn off_is_the_master_on_the_first_pair_and_nothing_else() {
        let master = (0.1, 0.2);
        let cue = (0.3, 0.4);
        let lanes: Vec<f32> = (0..4).map(|at| CueRouting::Off.lane(at, master, cue)).collect();
        assert_eq!(lanes, vec![0.1, 0.2, 0.0, 0.0]);
        assert_eq!(CueRouting::Off.channels_needed(), 2);
        assert!(!CueRouting::Off.has_cue());
    }

    #[test]
    fn lanes_put_master_and_cue_where_they_are_asked_for() {
        // The DDJ-FLX4: master on 1/2, headphones on 3/4.
        let flx4 = CueRouting::Lanes { master: (0, 1), cue: (2, 3) };
        let lanes: Vec<f32> = (0..6).map(|at| flx4.lane(at, (0.1, 0.2), (0.3, 0.4))).collect();
        assert_eq!(lanes, vec![0.1, 0.2, 0.3, 0.4, 0.0, 0.0]);
        assert_eq!(flx4.channels_needed(), 4);

        let swapped = CueRouting::Lanes { master: (2, 3), cue: (0, 1) };
        let lanes: Vec<f32> = (0..4).map(|at| swapped.lane(at, (0.1, 0.2), (0.3, 0.4))).collect();
        assert_eq!(lanes, vec![0.3, 0.4, 0.1, 0.2]);
    }

    #[test]
    fn split_is_the_cue_left_and_the_master_right_each_in_mono() {
        let lanes: Vec<f32> =
            (0..3).map(|at| CueRouting::Split.lane(at, (0.2, 0.4), (0.6, 0.8))).collect();
        assert!((lanes[0] - 0.7).abs() < 1e-6, "cue: {}", lanes[0]);
        assert!((lanes[1] - 0.3).abs() < 1e-6, "master: {}", lanes[1]);
        assert_eq!(lanes[2], 0.0);
    }
}
