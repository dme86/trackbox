//! Soak test: two decks for hours against a sink pulled at device pace.
//!
//! Synthetic tracks, loops set and exited, tempo changes with key lock on and
//! off, filter / EQ / crossfader sweeps, a seek back to the top when a deck
//! nears its end. Reports, once a minute and at the end:
//!
//! - callbacks over their deadline (render took longer than the buffer lasts)
//! - pulls the harness itself made late (OS scheduling, not the engine)
//! - callbacks in which a playing deck ran dry (includes the refill after a seek)
//! - allocations inside the render callback (must be 0)
//! - heap in use (counting allocator) and resident memory (`ps`) against the
//!   level after warm-up
//!
//! Not run by CI: run it by hand, in release, on a quiet machine.
//!
//! `cargo run --release -p rbl-deck --example soak [minutes]` (default 120)
#![allow(clippy::pedantic, clippy::print_stdout, clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Counting allocations means being the allocator; never shipped.
#![allow(unsafe_code, reason = "a counting allocator is the only way to count allocations")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use rbl_deck::{Band, Deck, DeckEvent, Engine, Render, Sink};

const RATE: u32 = 48_000;
const BUFFER: usize = 256;
const TRACK_SECONDS: u32 = 90;
/// The first callbacks build the strips, limiter and smoothing at the
/// device's rate, once, by design; allocations there are counted apart.
const SETUP_PULLS: u64 = 20;

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<usize> = const { Cell::new(0) };
}
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counting;

fn grew(by: usize) {
    let live = LIVE.fetch_add(by, Ordering::Relaxed) + by;
    PEAK.fetch_max(live, Ordering::Relaxed);
    let _ = WATCHING.try_with(|watching| {
        if watching.get() {
            let _ = COUNT.try_with(|count| count.set(count.get() + 1));
        }
    });
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        grew(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) };
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        grew(new);
        unsafe { System.realloc(ptr, layout, new) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        grew(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A null sink that owns its buffers, so a pull allocates nothing of its own
/// and every allocation counted is the engine's. Master and headphones.
struct Paced {
    render: Mutex<Render>,
    master: Mutex<Vec<f32>>,
    cue: Mutex<Vec<f32>>,
    running: AtomicBool,
}

impl Paced {
    /// One callback; how long it took and what it allocated.
    fn pull(&self) -> (Duration, usize) {
        let mut master = self.master.lock().unwrap();
        let mut cue = self.cue.lock().unwrap();
        let mut render = self.render.lock().unwrap();
        master.fill(0.0);
        if !self.running.load(Ordering::SeqCst) {
            return (Duration::ZERO, 0);
        }
        COUNT.with(|c| c.set(0));
        WATCHING.with(|w| w.set(true));
        let started = Instant::now();
        render(&mut master, &mut cue);
        let took = started.elapsed();
        WATCHING.with(|w| w.set(false));
        (took, COUNT.with(Cell::get))
    }
}

impl Sink for Paced {
    fn sample_rate(&self) -> u32 {
        RATE
    }
    fn start(&self) -> rbl_deck::Result<()> {
        self.running.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn stop(&self) -> rbl_deck::Result<()> {
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }
}

/// Something with a kick, a bass line and hats, so every EQ band has work.
fn track(path: &std::path::Path, bpm: f32, root: f32) {
    let frames = RATE * TRACK_SECONDS;
    let data = frames * 4;
    let mut out = Vec::with_capacity(44 + data as usize);
    for chunk in [&b"RIFF"[..], &(36 + data).to_le_bytes(), b"WAVEfmt ", &16_u32.to_le_bytes(), &1_u16.to_le_bytes(),
        &2_u16.to_le_bytes(), &RATE.to_le_bytes(), &(RATE * 4).to_le_bytes(), &4_u16.to_le_bytes(),
        &16_u16.to_le_bytes(), b"data", &data.to_le_bytes()]
    {
        out.extend_from_slice(chunk);
    }
    let beat = 60.0 / bpm;
    let tau = std::f32::consts::TAU;
    let mut noise = 0x2545_F491_u32;
    for n in 0..frames {
        let t = n as f32 / RATE as f32;
        let in_beat = t % beat;
        let kick = (tau * 50.0 * in_beat).sin() * (-in_beat * 14.0).exp();
        let bass = (tau * root * t).sin() * 0.25;
        noise ^= noise << 13;
        noise ^= noise >> 17;
        noise ^= noise << 5;
        let hat = (noise as f32 / u32::MAX as f32 - 0.5) * (-((t + beat / 2.0) % beat) * 60.0).exp() * 0.3;
        let s = ((kick * 0.6 + bass + hat) * 20_000.0).clamp(-32_767.0, 32_767.0) as i16;
        out.extend_from_slice(&s.to_le_bytes());
        out.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, out).unwrap();
}

/// Resident memory in KiB, from `ps`, where there is one.
fn rss_kib() -> Option<u64> {
    let out = std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output().ok()?;
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

fn mib(bytes: usize) -> f64 {
    bytes as f64 / 1_048_576.0
}

#[derive(Default)]
struct Tally {
    pulls: u64,
    overruns: u64,
    late: u64,
    setup_allocations: usize,
    rt_allocations: usize,
    worst: Duration,
    seeks: u64,
    loops: u64,
}

fn main() {
    let minutes: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(120);
    let dir = tempfile::tempdir().unwrap();
    let paths = [dir.path().join("a.wav"), dir.path().join("b.wav")];
    track(&paths[0], 124.0, 55.0);
    track(&paths[1], 128.0, 61.7);

    let (events_tx, events_rx) = mpsc::channel();
    let events: rbl_deck::EventSink = Arc::new(move |event: DeckEvent| {
        let _ = events_tx.send(event);
    });
    let (sink_tx, sink_rx) = mpsc::channel();
    let engine = Engine::with_sink(
        move |render| {
            let sink = Arc::new(Paced {
                render: Mutex::new(render),
                master: Mutex::new(vec![0.0; BUFFER * 2]),
                cue: Mutex::new(vec![0.0; BUFFER * 2]),
                running: AtomicBool::new(false),
            });
            sink_tx.send(Arc::clone(&sink)).unwrap();
            Ok(sink as Arc<dyn Sink>)
        },
        &events,
    )
    .unwrap();
    let sink = sink_rx.recv().unwrap();
    for (deck, path) in Deck::ALL.into_iter().zip(&paths) {
        engine.load(deck, path);
    }
    for _ in 0..2 {
        match events_rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            DeckEvent::Loaded { .. } => {}
            DeckEvent::Error { message, .. } => panic!("load failed: {message}"),
        }
    }
    engine.mixer().channels[0].set_cue(true);
    std::thread::sleep(Duration::from_millis(200));
    engine.play(Deck::A);
    engine.play(Deck::B);

    let period = Duration::from_secs_f64(BUFFER as f64 / f64::from(RATE));
    let total = Duration::from_secs(minutes * 60);
    let start = Instant::now();
    let mut tally = Tally::default();
    let mut next = start;
    let mut next_report = start + Duration::from_secs(60);
    let mut next_control = start;
    let mut next_transport = start;
    let mut baseline: Option<(usize, Option<u64>)> = None;
    let mut rss_peak = 0_u64;
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut rand = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % 10_000) as f32 / 10_000.0
    };
    println!("soak: {minutes} min, {BUFFER} frames @ {RATE} Hz (one callback every {:.2} ms)", period.as_secs_f64() * 1e3);

    while start.elapsed() < total {
        let now = Instant::now();
        if now < next {
            std::thread::sleep(next - now);
        } else if now - next > period {
            // A device would have underrun here; the harness was not scheduled.
            tally.late += 1;
            next = now;
        }
        let (took, allocated) = sink.pull();
        tally.pulls += 1;
        if tally.pulls <= SETUP_PULLS {
            tally.setup_allocations += allocated;
        } else {
            tally.rt_allocations += allocated;
        }
        tally.worst = tally.worst.max(took);
        if took > period {
            tally.overruns += 1;
        }
        next += period;

        let now = Instant::now();
        let t = (now - start).as_secs_f32();
        // Sweeps: every 20 ms, like a hand on a knob.
        if now >= next_control {
            next_control = now + Duration::from_millis(20);
            let mixer = engine.mixer();
            mixer.set_crossfade(0.5 + 0.5 * (t * 0.05).sin());
            for (i, channel) in mixer.channels.iter().enumerate() {
                let phase = t * (0.11 + 0.07 * i as f32);
                channel.set_filter(0.5 + 0.45 * phase.sin());
                channel.set_band(Band::Low, 0.5 + 0.5 * (phase * 1.3).sin());
                channel.set_band(Band::Mid, 0.5 + 0.5 * (phase * 0.7).cos());
                channel.set_band(Band::High, 0.5 + 0.5 * (phase * 1.9).sin());
                channel.set_volume(0.7 + 0.3 * (phase * 0.5).cos());
            }
            mixer.set_headphone_mix(0.5 + 0.5 * (t * 0.2).sin());
        }
        // Transport: once a second, one deck gets a tempo, a key lock, a loop
        // or a loop exit; near its end it goes back to the top.
        if now >= next_transport {
            next_transport = now + Duration::from_secs(1);
            let deck = if rand() < 0.5 { Deck::A } else { Deck::B };
            let snap = engine.snapshot();
            let at = if deck == Deck::A { snap.a } else { snap.b };
            if at.total_frames > 0 && at.position_frames + u64::from(RATE) * 10 > at.total_frames {
                engine.seek_ms(deck, 0.0);
                tally.seeks += 1;
            } else {
                let pick = rand();
                if pick < 0.4 {
                    engine.set_tempo(deck, 0.92 + 0.16 * rand());
                } else if pick < 0.55 {
                    engine.set_master_tempo(deck, rand() < 0.5);
                } else if pick < 0.8 && !at.looping {
                    // From where the head is, so setting it is not a seek.
                    let beats = [1_u64, 2, 4, 8][(rand() * 4.0) as usize % 4];
                    let length = u64::from(RATE) * 60 / 126 * beats;
                    engine.set_loop_frames(deck, at.position_frames, at.position_frames + length);
                    tally.loops += 1;
                } else if at.looping {
                    engine.set_looping(deck, false);
                }
            }
        }
        if !engine.any_playing() {
            engine.play(Deck::A);
            engine.play(Deck::B);
        }

        if now >= next_report {
            next_report += Duration::from_secs(60);
            let heap = LIVE.load(Ordering::Relaxed);
            let rss = rss_kib();
            rss_peak = rss_peak.max(rss.unwrap_or(0));
            let base = *baseline.get_or_insert_with(|| {
                // The peak from here on: building the tracks is not growth.
                PEAK.store(heap, Ordering::Relaxed);
                (heap, rss)
            });
            println!(
                "{:>4} min  over-deadline {}  harness-late {}  dry {}  rt-allocs {}  worst {:.3} ms  heap {:.1} MiB ({:+.2})  rss {}",
                (now - start).as_secs() / 60,
                tally.overruns,
                tally.late,
                engine.audio_health().starved,
                tally.rt_allocations,
                tally.worst.as_secs_f64() * 1e3,
                mib(heap),
                mib(heap) - mib(base.0),
                rss.map_or("n/a".to_owned(), |r| format!("{:.1} MiB", r as f64 / 1024.0)),
            );
        }
    }

    let heap = LIVE.load(Ordering::Relaxed);
    let (base_heap, base_rss) = baseline.unwrap_or((heap, rss_kib()));
    let rss = rss_kib();
    println!("--- {minutes} min, {} callbacks", tally.pulls);
    println!("callbacks over deadline: {}  (worst {:.3} ms of {:.3} ms)", tally.overruns, tally.worst.as_secs_f64() * 1e3, period.as_secs_f64() * 1e3);
    println!("harness pulls late (scheduling): {}", tally.late);
    println!("callbacks a deck ran dry: {}  (seeks to the top: {}, loops set: {})", engine.audio_health().starved, tally.seeks, tally.loops);
    println!(
        "allocations in the render callback: {}  {}  (one-time setup in the first {SETUP_PULLS}: {})",
        tally.rt_allocations,
        if tally.rt_allocations == 0 { "(allocation-free)" } else { "(REALTIME CONTRACT BROKEN)" },
        tally.setup_allocations,
    );
    println!(
        "heap: after warm-up {:.1} MiB, now {:.1} MiB, peak since warm-up {:.1} MiB",
        mib(base_heap),
        mib(heap),
        mib(PEAK.load(Ordering::Relaxed))
    );
    if let (Some(base), Some(now)) = (base_rss, rss) {
        println!(
            "rss: after warm-up {:.1} MiB, now {:.1} MiB, peak sampled {:.1} MiB",
            base as f64 / 1024.0,
            now as f64 / 1024.0,
            rss_peak.max(now) as f64 / 1024.0
        );
    }
}
