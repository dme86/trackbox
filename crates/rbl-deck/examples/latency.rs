//! Mixer control → audio latency: how long after a fader or crossfader move
//! is stored the first audio callback that renders it starts.
//!
//! Both decks play synthetic audio into a null sink pulled at device pace,
//! 256 frames at 48 kHz, while another thread moves the faders at random
//! moments. What is measured stops at the start of the callback: the device's
//! own buffer and converter latency come on top and are not visible here.
//!
//! `cargo run -p rbl-deck --example latency [seconds]` (default 10)
#![allow(clippy::pedantic, clippy::print_stdout, clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use rbl_deck::{Deck, DeckEvent, Engine, NullSink, Sink};

const RATE: u32 = 48_000;
const BUFFER: usize = 256;

fn wav(path: &std::path::Path, seconds: u32, hz: f32) {
    let frames = RATE * seconds;
    let data = frames * 4;
    let mut out = Vec::with_capacity(44 + data as usize);
    for chunk in [&b"RIFF"[..], &(36 + data).to_le_bytes(), b"WAVEfmt ", &16_u32.to_le_bytes(), &1_u16.to_le_bytes(),
        &2_u16.to_le_bytes(), &RATE.to_le_bytes(), &(RATE * 4).to_le_bytes(), &4_u16.to_le_bytes(),
        &16_u16.to_le_bytes(), b"data", &data.to_le_bytes()]
    {
        out.extend_from_slice(chunk);
    }
    for n in 0..frames {
        let s = ((n as f32 * hz * std::f32::consts::TAU / RATE as f32).sin() * 12_000.0) as i16;
        out.extend_from_slice(&s.to_le_bytes());
        out.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, out).unwrap();
}

fn main() {
    let seconds: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(10);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone.wav");
    wav(&path, 30, 220.0);

    let (loaded_tx, loaded_rx) = mpsc::channel();
    let events: rbl_deck::EventSink = Arc::new(move |event: DeckEvent| {
        let _ = loaded_tx.send(event);
    });
    let (sink_tx, sink_rx) = mpsc::channel();
    let engine = Engine::with_sink(
        move |render| {
            let sink = Arc::new(NullSink::new(RATE, render));
            sink_tx.send(Arc::clone(&sink)).unwrap();
            Ok(sink as Arc<dyn Sink>)
        },
        &events,
    )
    .unwrap();
    let sink = sink_rx.recv().unwrap();
    for deck in Deck::ALL {
        engine.load(deck, &path);
        engine.set_tempo(deck, 1.03);
    }
    for _ in 0..2 {
        loaded_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    std::thread::sleep(Duration::from_millis(100));
    engine.play(Deck::A);
    engine.play(Deck::B);

    let period = Duration::from_secs_f64(BUFFER as f64 / f64::from(RATE));
    let done = AtomicBool::new(false);
    let mut samples = Vec::new();
    let mut late = 0_u64;
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut state = 0x9E37_79B9_7F4A_7C15_u64;
            let end = Instant::now() + Duration::from_secs(seconds);
            while Instant::now() < end {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let at = (state % 1000) as f32 / 1000.0;
                match state % 3 {
                    0 => engine.mixer().set_crossfade(at),
                    1 => engine.mixer().channels[0].set_volume(at),
                    _ => engine.mixer().channels[1].set_filter(at),
                }
                std::thread::sleep(Duration::from_micros(500 + state % 15_000));
            }
            done.store(true, Ordering::SeqCst);
        });
        let mut next = Instant::now();
        let mut seen = engine.control_latency().count;
        while !done.load(Ordering::SeqCst) {
            let now = Instant::now();
            if now < next {
                std::thread::sleep(next - now);
            }
            let started = Instant::now();
            sink.pull(BUFFER);
            if started.elapsed() > period {
                late += 1;
            }
            let latency = engine.control_latency();
            if latency.count != seen {
                seen = latency.count;
                samples.push(latency.last);
            }
            next += period;
        }
    });

    samples.sort();
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    println!("{BUFFER} frames @ {RATE} Hz: one callback every {:.2} ms; {seconds} s run", ms(period));
    println!("control moves measured: {}", samples.len());
    if samples.is_empty() {
        return;
    }
    println!(
        "control -> callback latency ms: min {:.3}  p50 {:.3}  p90 {:.3}  p99 {:.3}  max {:.3}",
        ms(samples[0]),
        ms(at(0.5)),
        ms(at(0.9)),
        ms(at(0.99)),
        ms(*samples.last().unwrap()),
    );
    let engine_max = engine.control_latency().max;
    println!("engine accessor max: {:.3} ms; callbacks over their deadline: {late}", ms(engine_max));
    let health = engine.audio_health();
    println!("decks ran dry in {} callbacks", health.starved);
}
