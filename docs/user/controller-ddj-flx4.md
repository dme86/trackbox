# DDJ-FLX4 controller

[Documentation](../README.md)

Trackbox supports the Pioneer DJ DDJ-FLX4 as a MIDI controller and, through
its built-in audio interface, as the output for master and headphones. It is
the only supported controller. Controller support is built on macOS and
Windows; other platforms do not detect it.

## Setup

1. Connect the FLX4 over USB. Trackbox looks for it every second, so it can be
   plugged in before or after launch. A MIDI port whose name contains
   `DDJ-FLX4` is used. While it is connected, macOS App Nap is held off so the
   controller's timers are not throttled.
2. Open Preferences (the gear icon) › Audio and choose the DDJ-FLX4 as the
   output device. Keep **Headphones** on **Automatic (DDJ-FLX4: outputs 3/4)**.
   The FLX4 tested offered 44100 and 48000 Hz only.
3. Choose **PERFORMANCE** in the top bar. It shows both decks and the full
   mixer. When a controller connects in EXPORT, a notice offers **Switch to
   PERFORMANCE**; Trackbox never switches by itself.
4. Move every channel fader, the HEADPHONES MIX knob, and the other mixer
   controls once. See [positions at startup](#positions-at-startup).

Hardware controls also act in EXPORT mode on the decks it shows, but only
PERFORMANCE lays out both decks and the mixer they map to.

Drag the black line under the decks down for taller waveforms and a shorter
track list; double-click it to go back. Each arrangement (one deck, two
stacked, two side by side) keeps its own height, and Preferences › View ›
**Reset panel sizes** resets all of them.

### Headphones routing

Preferences › Audio › Headphones decides where the cue is heard. Changes take
effect the next time a deck plays.

| Setting | Behavior |
| --- | --- |
| Automatic | If the output device's name contains `DDJ-FLX4`: master on outputs 1/2, cue on 3/4. Any other device: no separate headphones. |
| Off | Master on outputs 1/2 only. |
| Outputs 3/4, master on 1/2 | The same lanes on any device with at least four outputs. |
| Split | One stereo output: cue summed to mono on the left, master summed to mono on the right. |

A device with too few outputs for the chosen routing opens without separate
headphones. The channel CUE buttons and HEADPHONES MIX then have no audible
effect.

## Control mapping

"SHIFT +" means holding SHIFT on the controller. Controls marked
*not mapped* are decoded or sent by the hardware but do nothing in Trackbox.

### Decks

| Control | Action |
| --- | --- |
| PLAY/PAUSE | Play or pause. |
| SHIFT + PLAY/PAUSE | Not mapped. |
| CUE | Held: plays from the cue point while held, then returns (as the on-screen CUE). |
| SHIFT + CUE | Back to the start of the track. |
| BEAT SYNC | Toggle sync to the master deck. |
| BEAT SYNC, held | Make this deck the master. |
| SHIFT + BEAT SYNC | Next tempo range: ±6, ±10, ±16 %, WIDE (−50 % to +100 %). |
| TEMPO fader | Tempo within the deck's range, catching on whole BPMs. Ignored while synced. Takes over only after it reaches or crosses the deck's current tempo. |
| Channel CUE | Toggle that channel's headphone cue (pre-fader). |
| SHIFT + channel CUE | Toggle quantize (Q) for the deck. |

### Loops and memory cues

| Control | Action |
| --- | --- |
| LOOP IN | Set the loop in point. With no loop playing it also sets the cue point. |
| LOOP OUT | Set the out point after LOOP IN and start the loop. |
| SHIFT + LOOP IN / LOOP OUT | While a loop plays: the jog wheel moves that loop point. The button blinks. Press again to stop; the mode also ends with the loop. |
| 4BEAT/EXIT | No loop playing: start a 4-beat loop. In a loop: exit it. |
| SHIFT + 4BEAT/EXIT | Reloop: re-enter the last loop, or exit the one playing. |
| CUE/LOOP CALL ◀ / ▶ | In a loop: halve or double it. Otherwise: previous or next memory cue. |
| SHIFT + CUE/LOOP CALL ◀ / ▶ | Not mapped. |

In loop-point adjust mode, one jog turn moves the point about 1.8 s, and
SHIFT + jog moves it about eight times as far. The playhead is moved back
into the loop if the change leaves it outside.

### Jog wheels

Preferences › Audio › Mixer › **Jog wheel: vinyl mode (scratch when touched)**
chooses the behavior. It is on by default.

| Action | Vinyl mode | CDJ mode |
| --- | --- | --- |
| Touch and turn the platter | Scratch: the deck follows the hand, as dragging the waveform does. | Pitch bend. |
| Turn the side, playing deck | Pitch bend, up to ±20 %, released 60 ms after the wheel stops. | Same. |
| Turn the wheel, stopped deck | Move the playhead (one turn ≈ 1.8 s). | Same. |
| SHIFT + turn | Fast search, about 70 s per turn. | Same. |

When a track loads, the jog ring plays its load animation.

### Pads

Pads are numbered 1–8, left to right, top row first. The pad mode buttons are
switched by the hardware; SHIFT + a mode button selects the second mode.

| Mode | Pads | SHIFT + pad |
| --- | --- | --- |
| HOT CUE | Set hot cue A–H on an empty pad, call it on a set pad. | Delete that hot cue. |
| BEAT LOOP (SHIFT + BEAT JUMP) | Loop of 1/4, 1/2, 1, 2, 4, 8, 16, 32 beats. The active length again exits the loop. | Same as without SHIFT. |
| BEAT JUMP | Odd pads jump back, even pads forward: 2, 2, 4, 4, 8, 8, 16, 16 beats. In an active loop the loop moves with the jump (LOOP MOVE). | Same as without SHIFT. |
| KEY SHIFT (SHIFT + SAMPLER) | Key 0, +1, +2, +3, −3, −2, −1, 0 semitones. | Same as without SHIFT. |
| PAD FX1, PAD FX2 | A pad effect while the pad is held; see [PAD FX](#pad-fx). | Same as without SHIFT. |
| SAMPLER, KEYBOARD | Not mapped. SAMPLER is planned to control stems. | Not mapped. |

The screen follows the pad mode buttons. The second tab beside the deck is
named after the current mode and is laid out like the controller: the four
mode buttons with their SHIFT modes printed below them, then the eight pads in
two rows of four (pads 1–4 / hot cues A–D on top). It shows the hot cues with
their colour, time and name in HOT CUE, the effects of PAD FX1/FX2 with the
sounding one lit, the jump sizes of BEAT JUMP (with a LOOP MOVE note in a
loop), the loop lengths of BEAT LOOP and the semitones of KEY SHIFT. The pads
there work with the mouse too; a PAD FX pad sounds while it is held. Choosing
a mode on screen does not change the mode on the controller.

Pad LEDs by mode:

- HOT CUE: lit where a hot cue is set.
- PAD FX1, PAD FX2, BEAT JUMP, BEAT LOOP, KEY SHIFT: all lit. In PAD FX, the
  held pad goes dark while its effect plays.
- SAMPLER, KEYBOARD: dark.

### PAD FX

Trackbox has its own effect bank for the two PAD FX modes. rekordbox's bank
is configurable and is not copied. Effects are timed to the deck's playing
BPM. On a deck without a BPM (no beatgrid), the PAD FX pads do nothing.

| Pad | PAD FX1 | PAD FX2 |
| --- | --- | --- |
| 1 | Echo 1/4 beat | Roll 1/16 beat |
| 2 | Echo 1/2 beat | Roll 1/8 beat |
| 3 | Echo 1 beat | Roll 1/4 beat |
| 4 | Echo 2 beats | Roll 1/2 beat |
| 5 | Reverb, short | Roll 1 beat |
| 6 | Reverb, long | High-pass sweep (2 beats) |
| 7 | Trans 1/4 beat | Low-pass sweep (2 beats) |
| 8 | Trans 1/8 beat | Bit crush |

- Momentary: the effect plays while the pad is held and stops when it is
  released. Echo and reverb tails ring out after release.
- When a roll is released, the deck plays on from where it would have been
  without the roll.
- With several pads held, the last one pressed plays. Releasing it returns to
  another held pad; releasing a pad that is not playing changes nothing.
- Unloading the deck, loading another track, or disconnecting the controller
  releases the effect.

### Mixer

| Control | Action |
| --- | --- |
| TRIM | Channel gain: −26 dB to +6 dB, unity at the centre, silent at the far left. |
| EQ HI / MID / LOW | Channel EQ. Preferences › Audio › Mixer › EQ curve chooses EQ (−26 dB) or ISOLATOR (silence) at the bottom. |
| CFX | Channel filter: left low pass, right high pass, centre off. With SMART CFX on, the chosen effect is added; see [SMART CFX](#smart-cfx). |
| Channel fader | Channel volume. |
| Crossfader | Crossfade A ↔ B. With X-FADER off (mixer header, or Preferences › Audio › Mixer › Crossfader), both decks play at their channel faders. |
| HEADPHONES MIX | Cue ↔ master in the headphones. |
| MASTER CUE | Toggle the master in the headphones. |
| Channel level LEDs | Trackbox lights them from the channel level before the fader, −36 dB to 0 dB. |

### BEAT FX

| Control | Action |
| --- | --- |
| BEAT FX SELECT (± SHIFT) | Switch between ECHO and REVERB, the only two effects. |
| BEAT ◀ / ▶ | Shorter or longer beat: 1/8 to 8 beats, timed to the deck's BPM. |
| CH SELECT lever | 1 or 2: that channel. 1&2: the master (timed to the master deck). |
| LEVEL/DEPTH | Effect level. |
| BEAT FX ON/OFF | Toggle the effect. SHIFT + ON/OFF switches it off. |

### SMART CFX

The SMART CFX button on the FLX4, or **SMART CFX** in the PERFORMANCE mixer
header, turns it on or off for both channels. The header button follows the
hardware button. On/off lasts until Trackbox quits; the chosen effect is
remembered.

With SMART CFX on, each CFX knob drives a low pass plus the effect when
turned left, and a high pass plus the effect when turned right. The centre is
off. Choose the effect next to the SMART CFX button:

| Effect | Added to the filter |
| --- | --- |
| ECHO | Echo (default). |
| REVERB | Reverb. |
| NOISE | Noise. |
| CRUSH | Bit crush. |

While it is on, the FILTER label under each channel shows the effect and the
knob's tooltip says what it does. SMART FADER is not supported.

### Browser

| Control | Action |
| --- | --- |
| BROWSE, turn | Move in the focused area: the source rail, the playlist tree, the track list, or Track Suggestion. |
| SHIFT + BROWSE, turn | Move eight rows at a time. |
| BROWSE, press | Move focus: rail → tree → track list → Track Suggestion (when its panel is open) → rail. The focused area is outlined. |
| SHIFT + BROWSE, press | In the tree: open or close the selected folder. |
| LOAD (deck 1 / deck 2) | Load the selected track onto that deck. With Track Suggestion focused, load the highlighted suggestion. |

Track Suggestion ranks library tracks by key, BPM, genre, rating, and past
sets against the master deck's track, on this computer. Open its panel with
the Track Suggestion button in the right rail.

### MASTER deck

Hold BEAT SYNC to make a deck the master. MASTER also moves on its own to the
deck that is heard: when the master stops while the other deck plays, or the
other deck starts while the master is stopped.

### Recording the mix

The FLX4 has no record button; use **REC** in the PERFORMANCE mixer header.
It records the master output as the speakers get it (after the master level
and the limiter, metronome clicks included, headphones not) to a 24-bit FLAC
file named `Trackbox <date> <time>.flac`, in Music › Trackbox Recordings
unless another folder is set in Preferences › Audio › Recording. While REC is
on, the button shows the running time and the audio device stays open, so
pauses are recorded as silence. Press REC again to finish the file. If the
disk falls behind, the mixer says the recording has gaps; if the audio output
changes or goes away, the recording is stopped and the file is kept.

## Behavior on the hardware

### Positions at startup

The FLX4 does not report where its knobs and faders are, and no request found
so far makes it do so. Each on-screen control shows a pending marker until
its hardware control moves. On connect, Trackbox sets both channel faders to
zero and HEADPHONES MIX to CUE, so nothing plays at a level that was not set.
Move the channel faders to hear the decks. Other mixer controls keep their
software values until moved.

### Other behavior

- The TEMPO fader does not jump the music: it takes over only once it reaches
  the deck's tempo (see [Decks](#decks)).
- LEDs follow the decks: PLAY, CUE, BEAT SYNC, channel CUE, loop buttons
  (lit with no loop, blinking in a loop), 4BEAT/EXIT, pads (see
  [Pads](#pads)), the active pad mode, BEAT FX ON/OFF, MASTER CUE, and the
  channel level meters. All are switched off when the controller disconnects
  or Trackbox quits.
- Pulling the cable mid-scratch releases the platter and any pitch bend.
- MASTER LEVEL, HEADPHONES LEVEL, SMART FADER, and any other control not
  listed above are not decoded by Trackbox. Whether a level knob acts inside
  the FLX4's own audio interface has not been verified.

## Limitations

- Only the DDJ-FLX4, only two decks, macOS and Windows only.
- Positions are unknown until each control moves ([above](#positions-at-startup)).
- Separate headphone cue needs four outputs, such as the FLX4's own interface.
  On a stereo device, **Split** is the only headphone option.
- Using the controller with rekordbox or another DJ app at the same time is
  not supported. On macOS, Core MIDI lets several apps open the same port, so
  both apps would act on every press. On Windows, MIDI ports are commonly
  opened by one app at a time; the app that started first may hold the
  controller. Quit the other app first.
- The SAMPLER and KEYBOARD pad modes, and the SHIFT layers marked
  *not mapped*, do nothing yet.
- Some values come from the Mixxx mapping and are not yet confirmed on every
  unit, such as the BEAT JUMP pad layout and the port name on Windows.

## Troubleshooting

| Problem | Check |
| --- | --- |
| Controller not detected | Use macOS or Windows. Reconnect the cable, avoiding unpowered hubs. Quit other DJ apps (see [Limitations](#limitations)); Trackbox retries every second. |
| No sound after connecting | Move both channel faders up; they start at zero. Check the crossfader position, or switch X-FADER off. |
| Nothing in the headphones | Output device is the DDJ-FLX4 and Headphones is Automatic, then play a deck again. Press a channel CUE, and turn HEADPHONES MIX towards CUE. |
| A knob shows the pending marker | Move that control; the hardware cannot report it until then. |
| TEMPO fader does nothing | Turn BEAT SYNC off, then move the fader through the deck's current tempo. |
| Platter scratches instead of bending | Turn off vinyl mode in Preferences › Audio › Mixer. |
| "The audio output went away; both decks stopped." | The chosen output disconnected. Trackbox does not fall back to another device. Reconnect it: the next control you use reloads both decks at their positions. Or choose another output in Preferences › Audio. |
| Both apps react to the controller | Quit rekordbox or the other DJ app. |

Developers can inspect the raw MIDI with the capture tool in the
[controller crate](../../crates/rbl-control/README.md#testing).
