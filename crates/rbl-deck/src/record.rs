//! The mix recording: the master as the device is given it, after the level
//! and the limiter, written to a 24-bit FLAC file.
//!
//! The callback only copies into a ring; a writer thread drains it and does
//! the encoding and the file I/O.

use std::fs::File;
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::flac::FlacWriter;

/// How much of the master the ring holds before the callback starts dropping:
/// ten seconds at 96 kHz stereo, enough to ride out a slow disk.
pub const RECORD_SECONDS_BUFFERED: u32 = 10;
const RING_SAMPLES: usize = 96_000 * 2 * RECORD_SECONDS_BUFFERED as usize;
/// How often the writer looks at the ring between wake-ups.
const DRAIN_EVERY: Duration = Duration::from_millis(50);
const FULL_SCALE: f32 = 8_388_607.0;

/// What the interface shows about a recording.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingStatus {
    pub path: PathBuf,
    /// Seconds written to the file so far.
    pub seconds: f64,
    /// Frames the callback could not hand over because the ring was full: a
    /// gap in the file.
    pub dropped_frames: u64,
    /// Why the recording stopped on its own, if it did.
    pub error: Option<String>,
    pub active: bool,
}

struct Shared {
    armed: AtomicBool,
    dropped: AtomicU64,
}

/// The callback's end of the recording.
pub(crate) struct RecordTap {
    producer: Producer<f32>,
    shared: Arc<Shared>,
}

impl RecordTap {
    /// Realtime: copies the buffer into the ring or counts it as dropped.
    pub(crate) fn push(&mut self, out: &[f32]) {
        if !self.shared.armed.load(Ordering::Acquire) {
            return;
        }
        match self.producer.write_chunk_uninit(out.len()) {
            Ok(chunk) => {
                chunk.fill_from_iter(out.iter().copied());
            }
            Err(_) => {
                self.shared.dropped.fetch_add((out.len() / 2) as u64, Ordering::Relaxed);
            }
        }
    }
}

struct Progress {
    frames: AtomicU64,
    error: Mutex<Option<String>>,
    done: AtomicBool,
}

struct Running {
    path: PathBuf,
    rate: u32,
    stop: Arc<AtomicBool>,
    progress: Arc<Progress>,
    thread: JoinHandle<Consumer<f32>>,
}

enum State {
    Idle(Consumer<f32>),
    Running(Running),
    /// The ring's reader went with a writer thread that did not come back.
    Lost,
}

/// The engine's end: starts and stops recordings.
pub(crate) struct Recorder {
    shared: Arc<Shared>,
    state: Mutex<State>,
    last: Mutex<Option<RecordingStatus>>,
}

pub(crate) fn channel() -> (RecordTap, Recorder) {
    let (producer, consumer) = RingBuffer::new(RING_SAMPLES);
    let shared = Arc::new(Shared { armed: AtomicBool::new(false), dropped: AtomicU64::new(0) });
    (
        RecordTap { producer, shared: Arc::clone(&shared) },
        Recorder { shared, state: Mutex::new(State::Idle(consumer)), last: Mutex::new(None) },
    )
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Recorder {
    /// Whether a recording is running, which keeps the device running.
    pub(crate) fn armed(&self) -> bool {
        matches!(&*locked(&self.state), State::Running(running) if !running.progress.done.load(Ordering::Acquire))
    }

    /// Starts writing the master to a new file at `path`; an existing file is
    /// never overwritten.
    pub(crate) fn start(&self, path: &Path, rate: u32) -> io::Result<()> {
        let mut state = locked(&self.state);
        if matches!(&*state, State::Running(_)) {
            return Err(io::Error::other("a recording is already running"));
        }
        let consumer = match std::mem::replace(&mut *state, State::Lost) {
            State::Idle(mut consumer) => {
                // Whatever a previous recording's last callback left behind is not this one's.
                discard(&mut consumer);
                consumer
            }
            other => {
                *state = other;
                return Err(io::Error::other("the recorder is not available"));
            }
        };
        let file = match File::options().write(true).create_new(true).open(path) {
            Ok(file) => file,
            Err(e) => {
                *state = State::Idle(consumer);
                return Err(e);
            }
        };
        let writer = match FlacWriter::new(BufWriter::new(file), rate) {
            Ok(writer) => writer,
            Err(e) => {
                *state = State::Idle(consumer);
                return Err(e);
            }
        };
        self.shared.dropped.store(0, Ordering::Relaxed);
        // Armed before the thread exists, so the file starts at this call.
        self.shared.armed.store(true, Ordering::Release);
        let stop = Arc::new(AtomicBool::new(false));
        let progress =
            Arc::new(Progress { frames: AtomicU64::new(0), error: Mutex::new(None), done: AtomicBool::new(false) });
        let thread = std::thread::Builder::new()
            .name("rbl-record".to_owned())
            .spawn({
                let shared = Arc::clone(&self.shared);
                let stop = Arc::clone(&stop);
                let progress = Arc::clone(&progress);
                move || write_loop(consumer, writer, &shared, &stop, &progress)
            })
            .inspect_err(|_| self.shared.armed.store(false, Ordering::Release))?;
        *state = State::Running(Running { path: path.to_owned(), rate, stop, progress, thread });
        *locked(&self.last) = None;
        Ok(())
    }

    /// Stops the recording and finishes the file. The last status, if any.
    pub(crate) fn stop(&self) -> Option<RecordingStatus> {
        let mut state = locked(&self.state);
        if !matches!(&*state, State::Running(_)) {
            return locked(&self.last).clone();
        }
        let State::Running(running) = std::mem::replace(&mut *state, State::Lost) else {
            return None;
        };
        running.stop.store(true, Ordering::Release);
        running.thread.thread().unpark();
        let status = status_of(&running, &self.shared, false);
        let (path, rate, progress) = (running.path, running.rate, running.progress);
        if let Ok(consumer) = running.thread.join() {
            *state = State::Idle(consumer);
        } else {
            tracing::error!("the recording thread panicked");
        }
        let status = RecordingStatus {
            seconds: progress.frames.load(Ordering::Relaxed) as f64 / f64::from(rate.max(1)),
            error: locked(&progress.error).clone(),
            path,
            ..status
        };
        *locked(&self.last) = Some(status.clone());
        Some(status)
    }

    pub(crate) fn status(&self) -> Option<RecordingStatus> {
        match &*locked(&self.state) {
            State::Running(running) => {
                Some(status_of(running, &self.shared, !running.progress.done.load(Ordering::Acquire)))
            }
            _ => locked(&self.last).clone(),
        }
    }
}

impl Drop for Recorder {
    /// A file is always finished, even when the engine goes with the device.
    fn drop(&mut self) {
        self.stop();
    }
}

fn status_of(running: &Running, shared: &Shared, active: bool) -> RecordingStatus {
    RecordingStatus {
        path: running.path.clone(),
        seconds: running.progress.frames.load(Ordering::Relaxed) as f64 / f64::from(running.rate.max(1)),
        dropped_frames: shared.dropped.load(Ordering::Relaxed),
        error: locked(&running.progress.error).clone(),
        active,
    }
}

fn write_loop(
    mut consumer: Consumer<f32>,
    mut writer: FlacWriter<BufWriter<File>>,
    shared: &Shared,
    stop: &AtomicBool,
    progress: &Progress,
) -> Consumer<f32> {
    let mut samples: Vec<i32> = Vec::new();
    let mut failed = None;
    loop {
        let stopping = stop.load(Ordering::Acquire);
        if stopping {
            shared.armed.store(false, Ordering::Release);
        }
        drain(&mut consumer, &mut samples);
        if let Err(e) = writer.write(&samples) {
            failed = Some(e);
            break;
        }
        progress.frames.store(writer.frames(), Ordering::Relaxed);
        if stopping {
            break;
        }
        std::thread::park_timeout(DRAIN_EVERY);
    }
    shared.armed.store(false, Ordering::Release);
    let finished = writer.finish().and_then(|out| out.into_inner().map_err(io::IntoInnerError::into_error)).and_then(|file| file.sync_all());
    if let Some(e) = failed.or(finished.err()) {
        tracing::error!(error = %e, "the recording stopped");
        *locked(&progress.error) = Some(e.to_string());
    }
    progress.done.store(true, Ordering::Release);
    consumer
}

/// Everything in the ring, as 24-bit samples.
fn drain(consumer: &mut Consumer<f32>, samples: &mut Vec<i32>) {
    samples.clear();
    // Whole frames only: a callback's buffer always is, but the ring may be
    // read between the two halves of one.
    let available = consumer.slots() & !1;
    let Ok(chunk) = consumer.read_chunk(available) else { return };
    let (first, second) = chunk.as_slices();
    samples.extend(first.iter().chain(second).map(|s| to_i24(*s)));
    chunk.commit_all();
}

fn discard(consumer: &mut Consumer<f32>) {
    let n = consumer.slots();
    if let Ok(chunk) = consumer.read_chunk(n) {
        chunk.commit_all();
    }
}

fn to_i24(sample: f32) -> i32 {
    ((sample.clamp(-1.0, 1.0) * FULL_SCALE).round() as i32).clamp(-8_388_608, 8_388_607)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_master_is_quantised_to_24_bits_without_wrapping() {
        assert_eq!(to_i24(0.0), 0);
        assert_eq!(to_i24(1.0), 8_388_607);
        assert_eq!(to_i24(-1.0), -8_388_607);
        assert_eq!(to_i24(2.0), 8_388_607);
        assert_eq!(to_i24(0.5), 4_194_304);
    }

    #[test]
    fn an_unarmed_tap_copies_nothing_and_a_full_ring_counts_what_it_drops() {
        let (mut tap, recorder) = channel();
        tap.push(&[0.5; 64]);
        assert_eq!(tap.producer.slots(), RING_SAMPLES, "nothing written while not recording");
        recorder.shared.armed.store(true, Ordering::Release);
        let big = vec![0.25_f32; RING_SAMPLES];
        tap.push(&big);
        tap.push(&[0.25; 64]);
        assert_eq!(recorder.shared.dropped.load(Ordering::Relaxed), 32);
        recorder.shared.armed.store(false, Ordering::Release);
    }
}
