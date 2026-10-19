# rbl-control

[Project documentation](../../docs/README.md) · [Architecture](../../docs/development/architecture.md)

This crate turns DJ controller MIDI into intents and deck state into LED
messages. It is pure: no MIDI ports, threads, engine, or Tauri. Everything
can be tested with byte arrays. The one supported controller is the Pioneer
DJ DDJ-FLX4. The app integrates it through `src-tauri/src/controller.rs`;
the user guide is [DDJ-FLX4 controller](../../docs/user/controller-ddj-flx4.md).

## Start here

1. Read the types in [`src/lib.rs`](src/lib.rs): `Intent`, `Button`, `PadMode`, `JogMode`, `DeckLeds`.
2. Read the FLX4 profile in [`src/flx4.rs`](src/flx4.rs): `Flx4::decode`, `leds`, `all_off`.
3. Read the host in `src-tauri/src/controller.rs` for how intents reach the engine and the interface.
4. Follow the project [conventions](../../docs/development/conventions.md) and [contribution process](../../CONTRIBUTING.md).

## Code map

| Code | Responsibility |
| --- | --- |
| `lib.rs` | Controller-independent types: decks, buttons, pad modes, jog modes, intents, LED state. |
| `flx4.rs` | DDJ-FLX4 port and audio-device matching, the keepalive, MIDI decoding, and LED encoding. |
| `examples/midi-capture.rs` | Hardware capture tool: prints raw messages next to the decoded intent. |

`Flx4::decode` takes one MIDI message and returns at most one `Intent`. It
keeps the state the protocol needs: running status, the MSB half of each
14-bit control until its LSB arrives, the last value of each control (a
repeated value is dropped), the pad mode button last pressed per deck, and
the pads held per deck (PAD FX is momentary, so pad releases are intents
too). SysEx and unmapped messages return `None`.

`Intent::is_direct` marks the continuous mixer controls (trim, EQ, filter,
channel fader, crossfader, headphone mix, MASTER CUE, SMART CFX). The host
applies those to the engine at once and emits `mixer:smart-cfx` for SMART
CFX; everything else, apart from jog bends, goes to the interface as
`controller:intent`. The 4BEAT/EXIT button keeps its Mixxx wire name
`reloop`.

`flx4::leds` returns every LED message for one deck. Pads are written at the
current pad mode's base (`mode << 4 | pad`): hot cues in HOT CUE, all lit in
PAD FX (a held pad dark), BEAT JUMP, BEAT LOOP and KEY SHIFT, dark in SAMPLER
and KEYBOARD; the other modes' pads are sent dark so a mode change clears
them. The host remembers what it sent and only sends changes; at each
keepalive it forgets the lit ones, because the hardware drops LEDs around it
[OBS], and keeps the dark ones dark without resending. `all_off` puts out
every LED the profile lights, for disconnect and quit. `track_loaded` starts
the jog ring's load animation.

## Threading and real-time notes

Nothing here runs on the audio callback. `leds` and `all_off` allocate, so
keep them off real-time threads. The host in `src-tauri/src/controller.rs`
owns the threads:

- midir's input callback only forwards bytes over a channel.
- One `controller` thread decodes, applies direct intents to the engine's
  mixer controls, emits events, sends the 200 ms keepalive, refreshes LEDs
  every 30 ms, and rescans ports every second for hot-plug.
- The engine's mixer setters are the control side of `rbl-deck`; the audio
  callback is not reached from this crate.

The host is built on macOS and Windows only. Other platforms have no
controller support.

## Evidence markers

Values in `flx4.rs` are marked by source. `[OBS]` is the project's marker
(see [conventions](../../docs/development/conventions.md)); the other two are
this crate's:

- **[MIXXX]**: taken as hardware facts from the Mixxx DDJ-FLX4 mapping
  (GPL-2.0-or-later); no code is copied.
- **[OBS]**: observed on a real unit with `midi-capture`.
- **[VERIFY]**: not yet confirmed on hardware, such as the exact port name on
  each OS, the PAD FX note ranges and the SMART CFX LED address. SMART CFX
  (`96 00`) and SMART FADER (`96 01`) presses are [OBS]; SMART FADER is not
  supported.

Observed and worth knowing before changing anything:

- The keepalive does not make the FLX4 report knob and fader positions, nor
  does the DDJ-400 style request (`--query` below). The host therefore starts
  channel faders at zero and the headphone mix at CUE on connect.
- The tempo fader reads `0x3FFF` at its `+` end.
- MASTER CUE is a note the software toggles, not a hardware switch.
- Every BEAT FX lever move sends all four channel-select notes; LEVEL/DEPTH is
  14-bit on MIDI channel 5 or 6 depending on the lever.

## Testing

Unit tests decode byte arrays and check LED output; they need no hardware:

```sh
cargo test -p rbl-control
cargo clippy -p rbl-control --all-targets
```

`midi-capture` talks to a connected FLX4 (macOS and Windows; midir is a
dev-dependency only). It lists the MIDI ports, connects, sends the keepalive,
and prints each incoming message with a millisecond timestamp, its bytes,
and `Flx4::decode`'s result:

```sh
cargo run -p rbl-control --example midi-capture -- [seconds] [flags]
```

| Argument | Effect |
| --- | --- |
| `seconds` | Capture length; must come first. Default 10. |
| `--no-keepalive` | Sends no keepalive. |
| `--query` | Sends the DDJ-400 "report every control" SysEx with FLX4 model bytes. No position dump was observed. |
| `--sysex F0,00,...,F7` | Sends one message given as comma-separated hex bytes, to probe the hardware. |
| `--leds` | Lights every LED this crate drives, resent every 200 ms, for a visual check. |

Example: `cargo run -p rbl-control --example midi-capture -- 40 --leds`.
Close other apps that use the controller first. A capture establishes what
one unit sent on one OS; it is not evidence for other platforms or firmware.
