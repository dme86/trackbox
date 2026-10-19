//! The DJ controller host: finds a DDJ-FLX4 among the MIDI ports, keeps it
//! alive, applies what it asks for, and lights its LEDs.
//!
//! One thread owns the connection. midir's input callback only forwards the
//! bytes to it, so nothing slow runs on the MIDI thread. Continuous controls
//! and the jog wheels go straight to the engine — that is where latency is
//! felt — and every intent but the jogs is also sent to the interface as
//! `controller:intent`, which runs the deck logic (CUE, sync, hot cues,
//! loops) the keyboard uses and keeps the on-screen mixer in step.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;
#[cfg(any(target_os = "macos", windows))]
use std::time::Instant;

use parking_lot::Mutex;
use rbl_control::{Deck as CtlDeck, DeckLeds};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime};

/// What the interface knows about a deck's LEDs that the engine does not.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedPatch {
    pub sync: Option<bool>,
    pub hot_cues: Option<[bool; 8]>,
    /// The loop point the jog moves, whose button blinks.
    pub loop_adjust: Option<LoopPoint>,
}

/// Which end of a loop the jog moves, if any.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LoopPoint {
    None,
    In,
    Out,
}

/// Who is connected, for the interface.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerStatus {
    pub name: Option<String>,
}

#[derive(Default)]
pub struct Controller {
    leds: Mutex<[DeckLeds; 2]>,
    connected: Mutex<Option<String>>,
    /// CDJ: the platter only bends. Off, the default, is vinyl: touched, it scratches.
    cdj_jog: AtomicBool,
    quit: AtomicBool,
    /// Disconnects when the host thread has ended, its LEDs put out.
    stopped: Mutex<Option<Receiver<()>>>,
}

impl Controller {
    pub fn patch_leds(&self, deck: CtlDeck, patch: LedPatch) {
        let mut leds = self.leds.lock();
        if let Some(state) = leds.get_mut(deck.index()) {
            if let Some(sync) = patch.sync {
                state.sync = sync;
            }
            if let Some(hot_cues) = patch.hot_cues {
                state.hot_cues = hot_cues;
            }
            if let Some(adjust) = patch.loop_adjust {
                state.loop_adjust = match adjust {
                    LoopPoint::None => 0,
                    LoopPoint::In => 1,
                    LoopPoint::Out => 2,
                };
            }
        }
    }

    /// Whether the jog is moving a loop point on this deck, so its turns are the interface's.
    pub fn adjusting_loop(&self, deck: CtlDeck) -> bool {
        self.leds.lock().get(deck.index()).is_some_and(|state| state.loop_adjust != 0)
    }

    pub fn status(&self) -> ControllerStatus {
        ControllerStatus { name: self.connected.lock().clone() }
    }

    pub fn set_jog_vinyl(&self, on: bool) {
        self.cdj_jog.store(!on, Ordering::Relaxed);
    }

    /// Ends the host thread, which puts the controller's LEDs out; waits a
    /// moment for it so the process does not exit first.
    pub fn shutdown(&self) {
        self.quit.store(true, Ordering::Relaxed);
        if let Some(stopped) = self.stopped.lock().take() {
            let _ = stopped.recv_timeout(Duration::from_millis(500));
        }
    }
}

pub fn deck_of(deck: CtlDeck) -> rbl_deck::Deck {
    match deck {
        CtlDeck::A => rbl_deck::Deck::A,
        CtlDeck::B => rbl_deck::Deck::B,
    }
}

/// Starts the host thread. Without MIDI support in this build it does nothing,
/// and the interface never hears of a controller.
pub fn start<R: Runtime>(app: &AppHandle<R>) {
    #[cfg(any(target_os = "macos", windows))]
    {
        use tauri::Manager;

        let handle = app.clone();
        let (done, stopped) = std::sync::mpsc::channel::<()>();
        *app.state::<Arc<Controller>>().stopped.lock() = Some(stopped);
        let thread = std::thread::Builder::new().name("controller".to_owned()).spawn(move || {
            host::run(&handle);
            drop(done);
        });
        if let Err(e) = thread {
            tracing::error!(error = %e, "the controller host could not be started");
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = app;
}

/// How often the ports are looked at for a controller arriving or leaving.
#[cfg(any(target_os = "macos", windows))]
const SCAN_EVERY: Duration = Duration::from_secs(1);
/// **[MIXXX]** The keepalive's period.
#[cfg(any(target_os = "macos", windows))]
const KEEPALIVE_EVERY: Duration = Duration::from_millis(200);
/// How long the thread waits for MIDI before it looks at its timers.
#[cfg(any(target_os = "macos", windows))]
const POLL: Duration = Duration::from_millis(5);

/// How often the LEDs are compared with the decks.
#[cfg(any(target_os = "macos", windows))]
const LEDS_EVERY: Duration = Duration::from_millis(30);
/// How far a channel meter falls per LED pass, out of 127: about 0.6 s from
/// full to dark, so a meter reads as a level and not as a flicker.
#[cfg(any(target_os = "macos", windows))]
const METER_FALL: f32 = 6.5;

/// Pitch bend per tick of one message. **[ASSUME]** set by feel.
#[cfg(any(target_os = "macos", windows))]
const BEND_PER_TICK: f32 = 0.01;
#[cfg(any(target_os = "macos", windows))]
const MAX_BEND: f32 = 0.2;
/// A bend ends this long after the wheel stops.
#[cfg(any(target_os = "macos", windows))]
const BEND_HOLD: Duration = Duration::from_millis(60);

#[cfg(any(target_os = "macos", windows))]
mod host {
    use std::collections::HashMap;
    use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};

    use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
    use rbl_control::flx4::{self, Flx4};
    use rbl_control::{Intent, JogMode, Short};
    use tauri::{Emitter, Manager};

    use super::{
        deck_of, Arc, Controller, ControllerStatus, CtlDeck, DeckLeds, Instant, Ordering, Runtime, AppHandle,
        BEND_HOLD, BEND_PER_TICK, KEEPALIVE_EVERY, LEDS_EVERY, MAX_BEND, METER_FALL, POLL, SCAN_EVERY,
    };
    use crate::player::Player;

    struct Connected {
        name: String,
        _input: MidiInputConnection<()>,
        output: MidiOutputConnection,
        /// What each LED was last told, so only changes are sent.
        sent: HashMap<(u8, u8), u8>,
        #[cfg(target_os = "macos")]
        _awake: Awake,
    }

    impl Drop for Connected {
        /// Dark rather than frozen on the last deck shown. A pulled cable
        /// fails the first send, and the rest are not tried.
        fn drop(&mut self) {
            for message in flx4::all_off() {
                if self.output.send(&message).is_err() {
                    break;
                }
            }
        }
    }

    /// Keeps App Nap off while a controller is connected: a napping app's
    /// timers are throttled, and the keepalive and LEDs run on them.
    #[cfg(target_os = "macos")]
    struct Awake(objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2::runtime::NSObjectProtocol>>);

    #[cfg(target_os = "macos")]
    impl Awake {
        fn begin() -> Self {
            use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
            let options = NSActivityOptions::UserInitiated | NSActivityOptions::LatencyCritical;
            let reason = NSString::from_str("A DJ controller is connected");
            Self(NSProcessInfo::processInfo().beginActivityWithOptions_reason(options, &reason))
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for Awake {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            // SAFETY: the token is the one `beginActivityWithOptions:reason:` returned, as `endActivity:` requires.
            unsafe { objc2_foundation::NSProcessInfo::processInfo().endActivity(&self.0) };
        }
    }

    impl Connected {
        fn send(&mut self, message: &[u8]) {
            if let Err(e) = self.output.send(message) {
                tracing::debug!(error = %e, "a message to the controller was not sent");
            }
        }

        fn light(&mut self, [status, data, value]: Short) {
            if self.sent.get(&(status, data)) != Some(&value) {
                self.sent.insert((status, data), value);
                self.send(&[status, data, value]);
            }
        }
    }

    /// A jog wheel's bend, which ends when the wheel stops, and whether the
    /// interface was told its platter is held.
    struct Jog {
        bending: bool,
        last: Instant,
        touched: bool,
    }

    impl Default for Jog {
        fn default() -> Self {
            Self { bending: false, last: Instant::now(), touched: false }
        }
    }

    fn find_input(name: Option<&str>) -> Option<(MidiInput, midir::MidiInputPort, String)> {
        let input = MidiInput::new("Trackbox controller").ok()?;
        let found = input.ports().into_iter().find_map(|port| {
            let port_name = input.port_name(&port).ok()?;
            let wanted = name.map_or_else(|| flx4::matches(&port_name), |name| port_name == name);
            wanted.then_some((port, port_name))
        });
        found.map(|(port, port_name)| (input, port, port_name))
    }

    fn connect(messages: &Sender<Vec<u8>>) -> Option<Connected> {
        let (input, in_port, name) = find_input(None)?;
        let output = MidiOutput::new("Trackbox controller").ok()?;
        let out_port = output.ports().into_iter().find(|port| {
            output.port_name(port).is_ok_and(|found| flx4::matches(&found))
        })?;
        let forward = messages.clone();
        let input = input
            .connect(
                &in_port,
                "Trackbox controller in",
                move |_, bytes, ()| {
                    let _ = forward.send(bytes.to_vec());
                },
                (),
            )
            .map_err(|e| tracing::warn!(error = %e, port = %name, "the controller's input would not open"))
            .ok()?;
        let output = output
            .connect(&out_port, "Trackbox controller out")
            .map_err(|e| tracing::warn!(error = %e, port = %name, "the controller's output would not open"))
            .ok()?;
        tracing::info!(port = %name, "controller connected");
        Some(Connected {
            name,
            _input: input,
            output,
            sent: HashMap::new(),
            #[cfg(target_os = "macos")]
            _awake: Awake::begin(),
        })
    }

    fn announce<R: Runtime>(app: &AppHandle<R>, controller: &Controller, name: Option<String>) {
        (*controller.connected.lock()).clone_from(&name);
        let _ = app.emit("controller:status", ControllerStatus { name });
    }

    pub fn run<R: Runtime>(app: &AppHandle<R>) {
        let controller = Arc::clone(app.state::<Arc<Controller>>().inner());
        let (tx, rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = std::sync::mpsc::channel();
        let mut connected: Option<Connected> = None;
        let mut decoder = Flx4::new();
        let mut jogs = [Jog::default(), Jog::default()];
        let mut last_scan: Option<Instant> = None;
        let mut last_keepalive = Instant::now();
        let mut last_leds = Instant::now();
        let started = Instant::now();
        let mut meters = [0.0_f32; 2];
        // Each deck's load last seen; `None` until the first look after a connect.
        let mut loads: Option<[u64; 2]> = None;

        loop {
            if controller.quit.load(Ordering::Relaxed) {
                return;
            }
            if last_scan.is_none_or(|at| at.elapsed() >= SCAN_EVERY) {
                last_scan = Some(Instant::now());
                match connected.as_ref() {
                    Some(live) if find_input(Some(&live.name)).is_none() => {
                        tracing::info!(port = %live.name, "controller disconnected");
                        connected = None;
                        release_jogs(app, &mut jogs);
                        announce(app, &controller, None);
                    }
                    None => {
                        if let Some(mut fresh) = connect(&tx) {
                            decoder = Flx4::new();
                            fresh.send(&flx4::KEEPALIVE);
                            last_keepalive = Instant::now();
                            announce(app, &controller, Some(fresh.name.clone()));
                            connected = Some(fresh);
                            loads = None;
                            safe_start(app);
                        }
                    }
                    Some(_) => {}
                }
            }

            let cdj = controller.cdj_jog.load(Ordering::Relaxed);
            match rx.recv_timeout(POLL) {
                Ok(bytes) => {
                    handle(app, &mut decoder, &mut jogs, &bytes, cdj);
                    while let Ok(more) = rx.try_recv() {
                        handle(app, &mut decoder, &mut jogs, &more, cdj);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }

            settle_jogs(app, &mut jogs);
            if cdj {
                release_touches(app, &mut jogs);
            }

            if let Some(live) = connected.as_mut() {
                if last_keepalive.elapsed() >= KEEPALIVE_EVERY {
                    last_keepalive = Instant::now();
                    live.send(&flx4::KEEPALIVE);
                    // [OBS] The hardware drops LEDs it was sent around a
                    // keepalive; lit only once, they stay dark. The lit ones
                    // again; the dark ones are known dark and stay unsent.
                    live.sent.retain(|_, value| *value == 0);
                } else if last_leds.elapsed() < LEDS_EVERY {
                    continue;
                }
                last_leds = Instant::now();
                let blink = (started.elapsed().as_millis() / 500) % 2 == 0;
                let (leds, fx_on, master_cue, smart_cfx, load_ids) = engine_leds(app, *controller.leds.lock(), blink);
                if let Some(seen) = loads {
                    for ((deck, was), now) in CtlDeck::ALL.into_iter().zip(seen).zip(load_ids) {
                        if now != 0 && now != was {
                            live.send(&flx4::track_loaded(deck));
                        }
                    }
                }
                loads = Some(load_ids);
                for deck in CtlDeck::ALL {
                    if let Some(mut state) = leds.get(deck.index()).copied() {
                        state.pad_mode = decoder.pad_mode(deck);
                        state.held_pads = decoder.held_pads(deck);
                        if let Some(shown) = meters.get_mut(deck.index()) {
                            // Up at once, down at a steady rate.
                            *shown = f32::from(state.level).max(*shown - METER_FALL).max(0.0);
                            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "0..=127")]
                            let level = shown.round() as u8;
                            state.level = level;
                        }
                        for message in flx4::leds(deck, &state) {
                            live.light(message);
                        }
                    }
                }
                for message in flx4::fx_leds(fx_on) {
                    live.light(message);
                }
                live.light(flx4::master_cue_led(master_cue));
                live.light(flx4::smart_cfx_led(smart_cfx));
            }
        }
    }

    /// The hardware does not report where its faders are until they move
    /// [OBS], so the channels start closed and the headphones on CUE rather
    /// than playing at a level nobody set.
    fn safe_start<R: Runtime>(app: &AppHandle<R>) {
        let mut start = vec![Intent::HeadphoneMix { value: 0.0 }];
        start.extend(CtlDeck::ALL.map(|deck| Intent::Volume { deck, value: 0.0 }));
        for intent in start {
            direct(app, intent);
            let _ = app.emit("controller:intent", intent);
        }
    }

    /// The interface's half of the LEDs, with the engine's on top, whether
    /// BEAT FX, MASTER CUE and SMART CFX are on, and each deck's load id.
    fn engine_leds<R: Runtime>(
        app: &AppHandle<R>,
        mut leds: [DeckLeds; 2],
        blink: bool,
    ) -> ([DeckLeds; 2], bool, bool, bool, [u64; 2]) {
        let Some(engine) = app.state::<Arc<Player>>().opened() else {
            return ([DeckLeds::default(); 2], false, false, false, [0; 2]);
        };
        let snapshot = engine.snapshot();
        let load_ids = [snapshot.a.load_id, snapshot.b.load_id];
        for (i, (state, deck)) in leds.iter_mut().zip([snapshot.a, snapshot.b]).enumerate() {
            state.play = deck.playing;
            state.cue = deck.loaded && !deck.playing;
            state.looping = deck.looping;
            state.blink = blink;
            let channel = engine.mixer().channels.get(i);
            state.channel_cue = channel.is_some_and(rbl_deck::ChannelSettings::cue);
            state.level = channel.map_or(0, |c| meter(c.take_level()));
        }
        (leds, engine.fx().on(), engine.mixer().master_cue(), engine.mixer().smart_cfx_on(), load_ids)
    }

    /// A peak as the channel meter's 0 to 127: −36 dB to full scale.
    fn meter(peak: f32) -> u8 {
        if peak <= 0.0 {
            return 0;
        }
        let db = 20.0 * peak.log10();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to 0..=127")]
        let level = (((db + 36.0) / 36.0).clamp(0.0, 1.0) * 127.0).round() as u8;
        level
    }

    fn handle<R: Runtime>(app: &AppHandle<R>, decoder: &mut Flx4, jogs: &mut [Jog; 2], bytes: &[u8], cdj: bool) {
        let intent = match decoder.decode(bytes) {
            None => return,
            // CDJ: the platter bends as the side does, and a touch means nothing.
            Some(Intent::JogTouch { .. }) if cdj => return,
            Some(Intent::Jog { deck, mode: JogMode::Scratch, ticks }) if cdj => {
                Intent::Jog { deck, mode: JogMode::Bend, ticks }
            }
            Some(intent) => intent,
        };
        if let Intent::JogTouch { deck, touched } = intent {
            if let Some(jog) = jogs.get_mut(deck.index()) {
                jog.touched = touched;
            }
        }
        // A bend moves a playing deck at once, here. A scratch, a search and
        // a stopped deck's nudge go to the interface, which drags the deck the
        // way the mouse does and so draws the waveform under the hand.
        if let Intent::Jog { deck, mode: JogMode::Bend, ticks } = intent {
            if playing(app, deck) && !app.state::<Arc<Controller>>().adjusting_loop(deck) {
                bend(app, jogs, deck, ticks);
                return;
            }
        }
        if intent.is_direct() {
            direct(app, intent);
        }
        let _ = app.emit("controller:intent", intent);
    }

    fn playing<R: Runtime>(app: &AppHandle<R>, deck: CtlDeck) -> bool {
        app.state::<Arc<Player>>().opened().is_some_and(|engine| {
            let snapshot = engine.snapshot();
            if deck == CtlDeck::A { snapshot.a.playing } else { snapshot.b.playing }
        })
    }

    fn bend<R: Runtime>(app: &AppHandle<R>, jogs: &mut [Jog; 2], deck: CtlDeck, ticks: i32) {
        let Some(engine) = app.state::<Arc<Player>>().opened() else { return };
        let Some(jog) = jogs.get_mut(deck.index()) else { return };
        #[allow(clippy::cast_precision_loss, reason = "a handful of ticks")]
        let amount = (ticks as f32 * BEND_PER_TICK).clamp(-MAX_BEND, MAX_BEND);
        engine.set_bend(deck_of(deck), 1.0 + amount);
        jog.bending = true;
        jog.last = Instant::now();
    }

    fn direct<R: Runtime>(app: &AppHandle<R>, intent: Intent) {
        // The mixer is opened for a knob, so the hardware's positions reach
        // the engine even before anything has played.
        let Ok(engine) = app.state::<Arc<Player>>().engine(app) else { return };
        let mixer = engine.mixer();
        match intent {
            Intent::Trim { deck, value } => {
                if let Some(channel) = mixer.channels.get(deck.index()) {
                    channel.set_trim(value);
                }
            }
            Intent::Eq { deck, band, value } => {
                if let Some(channel) = mixer.channels.get(deck.index()) {
                    channel.set_band(band_of(band), value);
                }
            }
            Intent::Filter { deck, value } => {
                if let Some(channel) = mixer.channels.get(deck.index()) {
                    channel.set_filter(value);
                }
            }
            Intent::Volume { deck, value } => {
                if let Some(channel) = mixer.channels.get(deck.index()) {
                    channel.set_volume(value);
                }
            }
            Intent::Crossfader { value } => mixer.set_crossfade(value),
            Intent::HeadphoneMix { value } => mixer.set_headphone_mix(value),
            Intent::MasterCue => mixer.set_master_cue(!mixer.master_cue()),
            Intent::SmartCfx => {
                mixer.set_smart_cfx_on(!mixer.smart_cfx_on());
                crate::commands::emit_smart_cfx(app, mixer);
            }
            _ => {}
        }
    }

    /// Ends bends the wheel has stopped making.
    fn settle_jogs<R: Runtime>(app: &AppHandle<R>, jogs: &mut [Jog; 2]) {
        if !jogs.iter().any(|jog| jog.bending && jog.last.elapsed() >= BEND_HOLD) {
            return;
        }
        let Some(engine) = app.state::<Arc<Player>>().opened() else { return };
        for (deck, jog) in CtlDeck::ALL.into_iter().zip(jogs.iter_mut()) {
            if jog.bending && jog.last.elapsed() >= BEND_HOLD {
                jog.bending = false;
                engine.set_bend(deck_of(deck), 1.0);
            }
        }
    }

    /// A controller pulled out mid-scratch must not leave a deck held or bent.
    fn release_jogs<R: Runtime>(app: &AppHandle<R>, jogs: &mut [Jog; 2]) {
        if let Some(engine) = app.state::<Arc<Player>>().opened() {
            for deck in CtlDeck::ALL {
                engine.set_bend(deck_of(deck), 1.0);
            }
        }
        for jog in jogs.iter_mut() {
            jog.bending = false;
            jog.touched = false;
        }
        for deck in CtlDeck::ALL {
            let _ = app.emit("controller:intent", Intent::JogTouch { deck, touched: false });
        }
    }

    /// Lets go of a platter the interface still holds after the switch to CDJ.
    fn release_touches<R: Runtime>(app: &AppHandle<R>, jogs: &mut [Jog; 2]) {
        for (deck, jog) in CtlDeck::ALL.into_iter().zip(jogs.iter_mut()) {
            if std::mem::take(&mut jog.touched) {
                let _ = app.emit("controller:intent", Intent::JogTouch { deck, touched: false });
            }
        }
    }

    fn band_of(band: rbl_control::Band) -> rbl_deck::Band {
        match band {
            rbl_control::Band::High => rbl_deck::Band::High,
            rbl_control::Band::Mid => rbl_deck::Band::Mid,
            rbl_control::Band::Low => rbl_deck::Band::Low,
        }
    }
}
