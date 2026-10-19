//! Captures what a DDJ-FLX4 sends, with what the decoder makes of it.
//!
//! `cargo run -p rbl-control --example midi-capture -- [seconds]`
//!
//! Lists the MIDI ports, connects to the controller, sends the keepalive, and
//! prints each message with a timestamp in milliseconds. Move a control to
//! see its bytes.
#![allow(clippy::print_stdout, reason = "a capture tool prints what it captures")]

#[cfg(any(target_os = "macos", windows))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use rbl_control::flx4::{self, Flx4};

    let seconds: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(10);
    let input = midir::MidiInput::new("rbl-control capture")?;
    let output = midir::MidiOutput::new("rbl-control capture")?;
    for port in input.ports() {
        println!("in:  {}", input.port_name(&port)?);
    }
    for port in output.ports() {
        println!("out: {}", output.port_name(&port)?);
    }
    let in_port = input
        .ports()
        .into_iter()
        .find(|p| input.port_name(p).is_ok_and(|n| flx4::matches(&n)))
        .ok_or("no DDJ-FLX4 input port")?;
    let out_port = output
        .ports()
        .into_iter()
        .find(|p| output.port_name(p).is_ok_and(|n| flx4::matches(&n)))
        .ok_or("no DDJ-FLX4 output port")?;

    let (tx, rx) = mpsc::channel();
    let start = Instant::now();
    let _in = input.connect(
        &in_port,
        "capture in",
        move |_, bytes, ()| {
            let _ = tx.send((start.elapsed(), bytes.to_vec()));
        },
        (),
    )?;
    let mut out = output.connect(&out_port, "capture out")?;
    let keepalive = !std::env::args().any(|a| a == "--no-keepalive");
    if keepalive {
        out.send(&flx4::KEEPALIVE)?;
    }
    // `--query`: the DDJ-400's "report every control" request with the FLX4's model bytes.
    if std::env::args().any(|a| a == "--query") {
        out.send(&[0xF0, 0x00, 0x40, 0x05, 0x00, 0x00, 0x04, 0x05, 0x00, 0x03, 0x01, 0xF7])?;
    }
    // `--sysex F0,00,...,F7`: any other message, to probe the hardware.
    let args: Vec<String> = std::env::args().collect();
    if let Some(hex) = args.iter().position(|a| a == "--sysex").and_then(|i| args.get(i + 1)) {
        let bytes: Vec<u8> = hex.split(',').filter_map(|b| u8::from_str_radix(b.trim(), 16).ok()).collect();
        out.send(&bytes)?;
    }
    // `--leds`: every LED this crate drives, on, so a person can check them.
    // Sent again with every keepalive, in case the hardware resets them.
    let leds = std::env::args().any(|a| a == "--leds");
    let all = rbl_control::DeckLeds {
        play: true,
        cue: true,
        sync: true,
        looping: true,
        channel_cue: true,
        hot_cues: [true; 8],
        pad_mode: 0x1B,
        held_pads: [false; 8],
        blink: true,
        level: 127,
        loop_adjust: 0,
    };
    let light = |out: &mut midir::MidiOutputConnection| -> Result<(), midir::SendError> {
        if leds {
            for deck in rbl_control::Deck::ALL {
                for message in flx4::leds(deck, &all) {
                    out.send(&message)?;
                }
            }
        }
        Ok(())
    };
    light(&mut out)?;

    let mut decoder = Flx4::new();
    let mut last_keepalive = Instant::now();
    while start.elapsed() < Duration::from_secs(seconds) {
        if last_keepalive.elapsed() >= Duration::from_millis(200) {
            last_keepalive = Instant::now();
            if keepalive {
                out.send(&flx4::KEEPALIVE)?;
            }
            light(&mut out)?;
        }
        if let Ok((at, bytes)) = rx.recv_timeout(Duration::from_millis(20)) {
            let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
            println!("{:>8.1} {:<24} {:?}", at.as_secs_f64() * 1000.0, hex.join(" "), decoder.decode(&bytes));
        }
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn main() {
    println!("MIDI capture is built on macOS and Windows only.");
}
