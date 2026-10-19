# Roadmap: DDJ-FLX4 – open items

As of 2026-10-18 · branch `feat/ddj-flx4-performance`

Completed work is removed from this list; it lives in the commits and in
[docs/user/controller-ddj-flx4.md](docs/user/controller-ddj-flx4.md).

Issues: [#6](https://github.com/dme86/trackbox/issues/6), [#7](https://github.com/dme86/trackbox/issues/7), [#8](https://github.com/dme86/trackbox/issues/8)

---

## 1. On the hardware (user)

- [ ] **Hardware acceptance list**: run through it once and record the FLX4 firmware version.
  1. Plug in → detected, audio on 1/2 + 3/4, LEDs on, status "DDJ-FLX4".
  2. Load a track on A and B (BROWSE + LOAD); load animation visible.
  3. Play A, preview B with channel CUE, sweep HEADPHONES MIX.
  4. Match B with SYNC, nudge with the jog edge, bring it in with the channel fader and crossfader.
  5. EQ, CFX, trim on both channels; LED chains and channel meters follow.
  6. Set/jump/delete hot cues; beat loop, IN/OUT, 4BEAT/EXIT (4-beat loop or exit), SHIFT + 4BEAT/EXIT (reloop), loop adjust (SHIFT + IN/OUT + jog), beat jump.
  7. Scratch on both platters (vinyl), bend (CDJ mode), search with SHIFT.
  8. Change tempo range; tempo fader after sync without a jump (pickup).
  9. Pull the USB cable during playback and plug it back in (decks stop, message, decks reload).
  10. Plug in and out ten times in a row without errors.
  11. PAD FX1 and PAD FX2: every pad on both decks; tails ring out, roll returns to the live position; pad LEDs per mode, held pad dark.
  12. SMART CFX button on the FLX4: mixer header follows; every kind (echo, reverb, noise, crush) on both CFX knobs; centre is clean.
  13. Pad mode buttons: the screen switches to HOT CUE / the PADS tab with the mode's pads; BEAT JUMP in an active loop moves the loop.
- [ ] **2-hour soak** with both decks, loops, scratch and pad FX: `cargo run --release -p rbl-deck --example soak` or a real set with REC; the dropout meter must stay at 0.
- [ ] **External latency measurement** with a loopback recording (button/fader → output). Measured internally: control command → callback p50 2.8 ms, p99 6.9 ms.
- [ ] **Tune by ear**: pad FX and Smart CFX levels (echo feedback/return, reverb size, noise level, crush amount) are estimates.

## 2. Decisions (user)

- [ ] **Device loss**: with an explicitly chosen output, Trackbox no longer falls back to another device; this also applies to the preview player in EXPORT mode. Keep it that way?
- [ ] **Merge upstream** (272 commits behind `chrisle/rbxport`). Trial merge: `crates/rbl-deck` merges cleanly; 11 conflicts in CI/release/branding and UI (`ci.yml`, `release-publish.yml`, `docs/development/releases.md`, `e2e/hot-cues.spec.ts`, `perf-budgets.json`, `menu.rs`, `tauri.conf.json`, `App.tsx`, `MixerStrip.test.tsx`, `Player.tsx`, `AboutPane.tsx`). Before or after the PRs?

## 3. Open implementation

Checked against the code on 2026-10-18.

| Item | State | Priority |
| --- | --- | --- |
| Translations of the new strings | ~150 entries are English in all 17 languages; `pnpm locales --translate-missing` needs network access (Google Translate). German could be done by hand. | Should |
| Make mixer template strings translatable | `Deck ${…} level/gain/EQ/filter/…`, `Kill ${label}`, `${n}% master`, `Recording to …` are not collected (template literals instead of `t(…)`). | Should |
| "MIDI port in use" message | If the port does not open (e.g. Windows while rekordbox runs), only a log entry is written; the UI shows no controller instead of an error. | Should |
| Load lock | LOAD onto a playing deck loads without asking (rekordbox asks or locks). | Should |
| Controller integration tests | No test with a virtual MIDI port, and no E2E test that drives `controller:intent` through the mock. | Should |
| STEMS on the SAMPLER pads | The SAMPLER pad mode is reserved for stems (mute/solo of vocals, drums, bass, other per deck); needs stem separation first. The PADS tab already shows the mode as STEMS (planned). | Planned |
| PAD FX tempo follow | `beatMs` is fixed when a pad is pressed and not resent if the tempo changes while it is held. | Could |
| Roll history | While a roll plays, the history ring is not written; a new roll right after release can reach back across that gap. | Could |
| Jog smoothing (alpha-beta) | Scratch uses tick integration without a filter; only needed if jitter is audible on the hardware. | Could |
| SHIFT + PLAY (stutter), SHIFT + CUE/LOOP CALL | Decoded (`ShiftPlay`, `ShiftCallPrevious/Next`) but do nothing; check the assignment against the FLX4 manual. | Could |
| Metronome on headphones only | Setting is missing; the click goes to the master (and therefore into the recording). | Could |
| SMART FADER | Decoded (`96 01`) but not supported. | Could |
| FLAC MD5 | STREAMINFO MD5 is 0 (allowed, `flac -t` warns). Would need an MD5 implementation or dependency. | Could |

## 4. Platforms and open facts

- [ ] **Windows**: does WASAPI (shared) offer the FLX4's 4 channels, or only ASIO (`cpal` feature, ASIO SDK licence)? Without 4 channels only **Split** remains. Confirm the port name on Windows.
- [ ] **Linux**: detection and 4-channel output untested.
- [ ] **[VERIFY] against the FLX4 manual / rekordbox**: CUE/LOOP CALL inside a loop (½/×2 assumed), headphones with no CUE active (silent assumed), KEYBOARD assignment.

## 5. Deliberately out of scope

Sampler (one-shot samples), KEYBOARD mode, four decks, previewing on a second audio device
(drift compensation), generic MIDI learn.

## 6. Hardware facts (reference, observed on the device)

- MIDI port `DDJ-FLX4`; audio 4 channels, 44.1/48 kHz, f32; master 1/2, headphones 3/4.
- No position dump (keepalive and every SysEx request tried); controls count as unknown until first moved.
- Resend LEDs after every keepalive. TEMPO "+" = `0x3FFF`. MASTER CUE `96 63`. SMART CFX `96 00`, SMART FADER `96 01`. SHIFT + jog `B0 29`, SHIFT + touch `90 67`.
- Sources: Mixxx mapping [Pioneer-DDJ-FLX4.midi.xml](https://github.com/mixxxdj/mixxx/blob/main/res/controllers/Pioneer-DDJ-FLX4.midi.xml) (facts only), [FLX4 manual](https://support.alphatheta.com/articles/12267724407961).
