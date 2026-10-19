/**
 * The mixer strip: a gain and three kill buttons per deck, and the crossfader
 * between them.
 *
 * Every size and colour here was measured off the two-player capture by
 * scanning its pixels — the strip is 48pt wide, the kill buttons 36×15 on a
 * 20pt pitch, the fader 53pt of travel with a 28×3 handle. See the `mixer*`
 * tokens.
 *
 * It exists only where there are two decks, which is what rekordbox does: the
 * one-player layout has no mixer and no crossfader, and a deck nobody has
 * touched a fader for plays at the level of its file.
 *
 * Nothing here holds audio state of its own. The engine owns the strip — see
 * `crates/rbl-deck/src/mixer.rs` — and these are the knobs that reach it.
 */
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";

import type { BeatFxSettings, DeckId, EqBand, RecordingStatus, SmartCfxKind } from "@/ipc/types";
import { getBackend } from "@/ipc/client";
import { errorMessage } from "@/lib/errorMessage";
import { detectPlatform, dispatchBinding, eqKillBand } from "@/lib/shortcuts";
import { VuMeter } from "@/lib/vuMeter";
import { useControllerIntent } from "@/store/useControllerIntent";
import { usePreferencesContext } from "@/store/usePreferences";
import { Fader, Knob } from "./MixerControls";
import styles from "./MixerStrip.module.css";

/** High to low, as the strip is drawn and as the mixer names them. */
const BANDS: ReadonlyArray<{ id: EqBand; label: string }> = [
  { id: "high", label: "HIGH" },
  { id: "mid", label: "MID" },
  { id: "low", label: "LOW" },
];

/** The knob's travel in pixels: a full sweep is this far under the pointer. */
const KNOB_TRAVEL = 120;

/** SMART CFX: what the CFX knob becomes, by the effect chosen in the mixer header. */
const SMART_CFX: Record<SmartCfxKind, { label: string; title: string }> = {
  echo: { label: "ECHO", title: "Smart CFX: low pass + echo ← → high pass + echo" },
  reverb: { label: "REVERB", title: "Smart CFX: low pass + reverb ← → high pass + reverb" },
  noise: { label: "NOISE", title: "Smart CFX: low pass + noise ← → high pass + noise" },
  crush: { label: "CRUSH", title: "Smart CFX: low pass + crush ← → high pass + crush" },
};
const SMART_CFX_KINDS = Object.keys(SMART_CFX) as SmartCfxKind[];

/**
 * How close to the middle the crossfader snaps.
 *
 * The centre is a position people aim for and cannot hit by hand on a 53pt
 * fader, so it is a detent — three pixels of it, which is under a millimetre.
 */
const DETENT = 3;

/** Trim as a dB reading, which is what the strip prints above the knob. */
function trimLabel(trim: number): string {
  if (trim <= 0) return "-∞dB";
  const db = 20 * Math.log10(trim);
  return `${db > 0 ? "+" : ""}${db.toFixed(1)}dB`;
}

/** How long without a reading before the meter falls on its own, as the master's does. */
const METER_SILENT_AFTER_MS = 100;

/** `4:05` or `1:04:05`. */
export function recordingClock(seconds: number): string {
  const whole = Math.max(0, Math.floor(seconds));
  const s = String(whole % 60).padStart(2, "0");
  const m = Math.floor(whole / 60) % 60;
  const h = Math.floor(whole / 3600);
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

/** REC: the master, as the room hears it, to a FLAC file. */
function RecordButton({ folder }: { folder: string | null }) {
  const [status, setStatus] = useState<RecordingStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    let live = true;
    let stop: (() => void) | undefined;
    void getBackend().then(async (backend) => {
      stop = backend.onRecordingStatus((next) => {
        if (!live) return;
        setStatus(next);
        setError(null);
      });
      const current = await backend.recordingStatus();
      if (live) setStatus(current);
      else stop();
    }).catch(() => {});
    return () => {
      live = false;
      stop?.();
    };
  }, []);
  const active = status?.active === true;
  const toggle = () => {
    setBusy(true);
    setError(null);
    void getBackend()
      .then((backend) => (active ? backend.stopRecording() : backend.startRecording(folder)))
      .then((next) => {
        if (next) setStatus(next);
      })
      .catch((cause: unknown) => setError(errorMessage(cause)))
      .finally(() => setBusy(false));
  };
  const gaps = (status?.droppedFrames ?? 0) > 0;
  const problem = error ?? status?.error ?? (gaps ? "The disk fell behind: the recording has gaps." : null);
  const title = active
    ? `Recording to ${status.path}`
    : status?.path
      ? `Last recording: ${status.path}`
      : "Record the master output to a FLAC file";
  return (
    <>
      <button type="button" className={styles.rec} aria-pressed={active} disabled={busy} title={title} onClick={toggle}>
        {active ? `REC ${recordingClock(status.seconds)}` : "REC"}
      </button>
      {problem ? <span role="alert" className={styles.recError}>{problem}</span> : null}
    </>
  );
}

/**
 * A channel's level, pre-fader, on the master meter's rekordbox scale
 * (-44 to 0 dBFS, 1 dB steps). Its own subscription, so a reading redraws
 * this bar and not the knobs. No engine is no readings, which is an empty bar.
 */
const ChannelMeter = memo(function ChannelMeter({ deck }: { deck: DeckId }) {
  const [shown, setShown] = useState(0);
  useEffect(() => {
    let live = true;
    let stop: (() => void) | undefined;
    let quiet: ReturnType<typeof setTimeout> | undefined;
    let falling: number | undefined;
    // No channel RMS is measured, so always the peak-only scale.
    const meter = new VuMeter("normal");
    let last = performance.now();
    const step = (peak: number) => {
      const now = performance.now();
      setShown(meter.step(peak, 0, now - last).peak);
      last = now;
    };
    const stand = () => {
      if (falling !== undefined) cancelAnimationFrame(falling);
      falling = undefined;
    };
    // The bar brings itself down, frame by frame, until it is empty.
    const fall = () => {
      step(0);
      falling = meter.active ? requestAnimationFrame(fall) : undefined;
    };
    void getBackend().then((backend) => {
      if (!live) return;
      stop = backend.onMeters((meters) => {
        const peak = meters.channelPeaks?.[deck === "a" ? 0 : 1];
        if (!live || peak === undefined) return;
        stand();
        step(peak);
        if (quiet !== undefined) clearTimeout(quiet);
        // The ticker stops with the music; the bar brings itself down.
        quiet = setTimeout(() => {
          falling = requestAnimationFrame(fall);
        }, METER_SILENT_AFTER_MS);
      });
    }).catch(() => {});
    return () => {
      live = false;
      stop?.();
      if (quiet !== undefined) clearTimeout(quiet);
      stand();
    };
  }, [deck]);
  return (
    <div
      className={styles.channelMeter}
      role="meter"
      aria-label={`Deck ${deck.toUpperCase()} level`}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(shown * 100)}
    >
      <span className={styles.channelMeterFill} style={{ clipPath: `inset(${(1 - shown) * 100}% 0 0 0)` }} aria-hidden />
    </div>
  );
});

/** One deck's half of the strip: the gain block and the three kill buttons. */
const Channel = memo(function Channel({
  deck, flipped, performance, controller = null, smartCfx = null,
}: {
  deck: DeckId;
  performance?: boolean;
  /** Deck B's half is the mirror of deck A's, as the capture has it. */
  flipped?: boolean;
  /** A connected controller, whose faders start closed until they move. */
  controller?: string | null;
  /** The SMART CFX effect on the CFX knob, or null for the plain filter. */
  smartCfx?: SmartCfxKind | null;
}) {
  const [trim, setTrim] = useState(1);
  const [volume, setVolume] = useState(1);
  // The FLX4 cannot report its knobs, so each is unknown until it moves.
  const [moved, setMoved] = useState<ReadonlySet<string>>(() => new Set());
  const pending = (control: string) => controller !== null && !moved.has(control);
  useEffect(() => {
    setMoved(new Set());
    if (controller) setVolume(0);
  }, [controller]);
  const [eq, setEq] = useState<Record<EqBand, number>>({ high: 0.5, mid: 0.5, low: 0.5 });
  const [filter, setFilter] = useState(0.5);
  const [cue, setCue] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [killed, setKilled] = useState<ReadonlySet<EqBand>>(() => new Set());
  const drag = useRef<{ y: number; from: number } | null>(null);

  const send = useCallback(
    (next: number) => {
      setTrim(next);
      void (async () => {
        const backend = await getBackend();
        await backend.setChannelTrim(deck, next);
      })().catch(() => {
        // A mixer that cannot reach the engine is a mixer with nothing behind
        // it; the knob still moves, and the deck plays as it did.
      });
    },
    [deck],
  );

  const adjustVolume = useCallback((next: number) => {
    setVolume(next);
    void getBackend().then((backend) => backend.setChannelVolume(deck, next))
      .then(() => setError(null))
      .catch(() => setError("Channel volume could not reach the audio engine."));
  }, [deck]);

  const adjustEq = useCallback((band: EqBand, next: number) => {
    setEq((current) => ({ ...current, [band]: next }));
    void getBackend().then((backend) => backend.setChannelBand(deck, band, next))
      .then(() => setError(null))
      .catch(() => setError("EQ could not reach the audio engine."));
  }, [deck]);

  const adjustFilter = useCallback((next: number) => {
    setFilter(next);
    void getBackend().then((backend) => backend.setChannelFilter(deck, next))
      .then(() => setError(null))
      .catch(() => setError("Filter could not reach the audio engine."));
  }, [deck]);

  const toggleCue = useCallback(() => {
    setCue((current) => {
      const next = !current;
      void getBackend().then((backend) => backend.setChannelCue(deck, next))
        .then(() => setError(null))
        .catch(() => setError("Headphone cue could not reach the audio engine."));
      return next;
    });
  }, [deck]);

  // The controller's own knobs already reached the engine; only the strip
  // has to follow them. CUE is a toggle, which the strip owns.
  useControllerIntent((intent) => {
    if (!("deck" in intent) || intent.deck !== deck) return;
    const control = intent.type === "eq" ? `eq-${intent.band}` : intent.type;
    if (!moved.has(control)) setMoved((current) => new Set(current).add(control));
    switch (intent.type) {
      case "trim": setTrim(intent.value); break;
      case "volume": setVolume(intent.value); break;
      case "eq": setEq((current) => ({ ...current, [intent.band]: intent.value })); break;
      case "filter": setFilter(intent.value); break;
      case "channelCue": toggleCue(); break;
      default: break;
    }
  });

  const toggle = useCallback(
    (band: EqBand) => {
      setKilled((current) => {
        const next = new Set(current);
        if (!next.delete(band)) next.add(band);
        void (async () => {
          const backend = await getBackend();
          await backend.setChannelKill(deck, band, next.has(band));
        })().catch(() => {});
        return next;
      });
    },
    [deck],
  );

  // The Keyboard pane's kill keys for this deck (unbound until assigned).
  const platform = useMemo(detectPlatform, []);
  const overrides = usePreferencesContext().preferences.keyboard.overrides;
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const hit = dispatchBinding(event, platform, event.target as HTMLElement | null, overrides);
      if (hit?.action === undefined || hit.deck !== deck) return;
      const band = eqKillBand(hit.action);
      if (band === null) return;
      event.preventDefault();
      if (!event.repeat) toggle(band);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [deck, overrides, platform, toggle]);

  const onKnobDown = useCallback(
    (event: React.PointerEvent<HTMLDivElement>) => {
      event.currentTarget.setPointerCapture(event.pointerId);
      drag.current = { y: event.clientY, from: trim };
    },
    [trim],
  );

  const onKnobMove = useCallback(
    (event: React.PointerEvent<HTMLDivElement>) => {
      const from = drag.current;
      if (!from) return;
      // Up is louder, which is the direction the knob turns.
      const moved = (from.y - event.clientY) / KNOB_TRAVEL;
      send(Math.min(Math.max(from.from + moved * 2, 0), 2));
    },
    [send],
  );

  const onKnobUp = useCallback(() => {
    drag.current = null;
  }, []);

  const gain = (
    <div className={styles.gain}>
      <span className={styles.readout}>{trimLabel(trim)}</span>
      <div
        className={styles.knob}
        role="slider"
        tabIndex={0}
        aria-label={`Deck ${deck.toUpperCase()} gain`}
        aria-valuemin={0}
        aria-valuemax={2}
        aria-valuenow={Number(trim.toFixed(2))}
        aria-valuetext={trimLabel(trim)}
        onPointerDown={onKnobDown}
        onPointerMove={onKnobMove}
        onPointerUp={onKnobUp}
        onPointerCancel={onKnobUp}
        onDoubleClick={() => send(1)}
        onKeyDown={(event) => {
          if (event.key === "ArrowUp") send(Math.min(trim + 0.05, 2));
          if (event.key === "ArrowDown") send(Math.max(trim - 0.05, 0));
        }}
      >
        {/* The tick, at the angle the gain is at: 0 points down-left, unity
            straight up, and the top of the travel down-right. */}
        <span
          className={styles.tick}
          style={{ rotate: `${(trim / 2) * 300 - 150}deg` }}
          aria-hidden
        />
      </div>
      {/* Auto Gain, which is what rekordbox's A-GAIN means. The knob is the
          trim until there is an analysed gain to switch it to. */}
      <span className={styles.caption}>A-GAIN</span>
    </div>
  );

  const bands = (
    <div className={styles.bands}>
      {BANDS.map((band) => (
        <button
          key={band.id}
          type="button"
          className={styles.band}
          aria-pressed={killed.has(band.id)}
          data-killed={killed.has(band.id) || undefined}
          onClick={() => toggle(band.id)}
        >
          {band.label}
        </button>
      ))}
    </div>
  );

  return (
    <div className={styles.channel} data-flipped={flipped || undefined}>
      {performance ? (
        <>
          <span className={styles.channelTitle}>DECK {deck.toUpperCase()}</span>
          {/* Knobs down one side, the fader beside them towards the centre:
              the whole channel in the height of its EQ section. */}
          <div className={styles.channelBody}>
            <div className={styles.knobColumn}>
              <div className={styles.knobRow}>
                <span className={styles.rowLabel}>TRIM</span>
                <Knob
                  label={`Deck ${deck.toUpperCase()} gain`}
                  value={trim} min={0} max={2} origin={1}
                  onChange={send} onReset={() => send(1)}
                  valueText={trimLabel(trim)} caption={trimLabel(trim)}
                  title="Trim; double-click for 0 dB"
                  pending={pending("trim")}
                />
              </div>
              <div className={styles.eqRows} role="group" aria-label={`Deck ${deck.toUpperCase()} EQ`}>
                {BANDS.map(({ id, label }) => (
                  <div key={id} className={styles.knobRow}>
                    <button
                      type="button"
                      className={styles.band}
                      aria-label={`Deck ${deck.toUpperCase()} ${label} kill`}
                      aria-pressed={killed.has(id)}
                      data-killed={killed.has(id) || undefined}
                      onClick={() => toggle(id)}
                      title={`Kill ${label}`}
                    >
                      {label}
                    </button>
                    <Knob
                      label={`Deck ${deck.toUpperCase()} ${label} EQ`}
                      value={eq[id]} origin={0.5}
                      onChange={(next) => adjustEq(id, next)} onReset={() => adjustEq(id, 0.5)}
                      valueText={`${Math.round(eq[id] * 100)}%`}
                      title="Double-click to reset to neutral"
                      pending={pending(`eq-${id}`)}
                    />
                  </div>
                ))}
              </div>
              <div className={styles.knobRow}>
                <span className={styles.rowLabel}>{smartCfx ? SMART_CFX[smartCfx].label : "FILTER"}</span>
                <Knob
                  label={`Deck ${deck.toUpperCase()} filter`}
                  value={filter} origin={0.5}
                  onChange={adjustFilter} onReset={() => adjustFilter(0.5)}
                  caption={Math.abs(filter - 0.5) < 0.02 ? "OFF" : filter < 0.5 ? "LPF" : "HPF"}
                  title={smartCfx
                    ? SMART_CFX[smartCfx].title
                    : "Left: low pass, right: high pass; double-click to turn off"}
                  pending={pending("filter")}
                />
              </div>
            </div>
            <div className={styles.faderColumn}>
              <button
                type="button"
                className={`${styles.band} ${styles.cueButton}`}
                aria-label={`Deck ${deck.toUpperCase()} headphone cue`}
                aria-pressed={cue}
                data-killed={cue || undefined}
                onClick={toggleCue}
                title="Hear this deck on the headphones, before its fader"
              >
                CUE
              </button>
              <div className={styles.faderRow}>
                <Fader
                  label={`Deck ${deck.toUpperCase()} channel volume`}
                  value={volume}
                  onChange={adjustVolume} onReset={() => adjustVolume(1)}
                  valueText={`${Math.round(volume * 100)}%`}
                  title="Channel fader (post-EQ); double-click to restore unity"
                  pending={pending("volume")}
                />
                <ChannelMeter deck={deck} />
              </div>
              <span className={styles.knobCaption}>{Math.round(volume * 100)}%</span>
            </div>
          </div>
          {error ? <span role="alert" className={styles.channelError}>{error}</span> : null}
        </>
      ) : (
        <>
          {flipped ? bands : gain}
          {flipped ? gain : bands}
        </>
      )}
    </div>
  );
});

export interface MixerStripProps {
  /** Told, so the app can keep the fader across a layout change. */
  onCrossfade?: (position: number) => void;
  /** Where the fader starts: 0 is deck A alone, 1 is deck B alone. */
  crossfade?: number;
  /** Expanded two-channel mixing surface; EXPORT keeps its compact strip. */
  performance?: boolean;
  /** What each deck is playing at, ×100, for BEAT FX to time itself to. */
  bpmX100?: Record<DeckId, number | null>;
  /** The sync master, whose BPM times an effect on the master. */
  masterDeck?: DeckId;
}

/** BEAT ◀ ▶ steps through these, in beats. */
const FX_BEATS = [0.125, 0.25, 0.5, 0.75, 1, 2, 4, 8];
const FX_DEFAULT_BEAT = 4;

function beatLabel(beats: number): string {
  if (beats >= 1) return String(beats);
  return beats === 0.75 ? "3/4" : `1/${Math.round(1 / beats)}`;
}

/** BEAT FX: one unit, ECHO or REVERB, on a deck or the master. */
function BeatFx({ bpmX100, masterDeck, depthPending }: {
  bpmX100: Record<DeckId, number | null>;
  masterDeck: DeckId;
  depthPending: boolean;
}) {
  const [on, setOn] = useState(false);
  const [kind, setKind] = useState<BeatFxSettings["kind"]>("echo");
  const [beat, setBeat] = useState(FX_DEFAULT_BEAT);
  const [lever, setLever] = useState<Record<DeckId, boolean>>({ a: false, b: false });
  const [depth, setDepth] = useState(0.5);
  const [error, setError] = useState<string | null>(null);
  // The lever on one channel is that channel; on both, or neither, the master.
  const target: BeatFxSettings["target"] = lever.a === lever.b ? "master" : lever.a ? "a" : "b";
  const bpm = (bpmX100[target === "master" ? masterDeck : target] ?? 12_000) / 100;
  const beats = FX_BEATS[beat] ?? 1;
  const beatMs = (beats * 60_000) / Math.max(bpm, 1);

  useEffect(() => {
    void getBackend()
      .then((backend) => backend.setBeatFx({ on, kind, target, depth, beatMs }))
      .then(() => setError(null))
      .catch(() => setError("BEAT FX could not reach the audio engine."));
  }, [on, kind, target, depth, beatMs]);

  const step = (longer: boolean) =>
    setBeat((at) => Math.min(Math.max(at + (longer ? 1 : -1), 0), FX_BEATS.length - 1));
  useControllerIntent((intent) => {
    switch (intent.type) {
      case "fxSelect": setKind((current) => (current === "echo" ? "reverb" : "echo")); break;
      case "fxBeat": step(intent.longer); break;
      case "fxToggle": setOn((current) => (intent.off ? false : !current)); break;
      case "fxChannel": setLever((current) => ({ ...current, [intent.deck]: intent.selected })); break;
      case "fxDepth": setDepth(intent.value); break;
      default: break;
    }
  });

  return (
    <div className={styles.beatFx} role="group" aria-label="Beat FX">
      <span>BEAT FX</span>
      <div role="group" aria-label="Beat FX effect">
        <button type="button" aria-pressed={kind === "echo"} onClick={() => setKind("echo")}>ECHO</button>
        <button type="button" aria-pressed={kind === "reverb"} onClick={() => setKind("reverb")}>REVERB</button>
      </div>
      <div role="group" aria-label="Beat FX beat">
        <button type="button" aria-label="Shorter beat" onClick={() => step(false)}>◀</button>
        <span className={styles.eqValue} aria-live="polite">{beatLabel(beats)}</span>
        <button type="button" aria-label="Longer beat" onClick={() => step(true)}>▶</button>
      </div>
      <div role="group" aria-label="Beat FX channel">
        <button type="button" aria-pressed={target === "a"} onClick={() => setLever({ a: true, b: false })}>A</button>
        <button type="button" aria-pressed={target === "b"} onClick={() => setLever({ a: false, b: true })}>B</button>
        <button type="button" aria-pressed={target === "master"} onClick={() => setLever({ a: false, b: false })}>MST</button>
      </div>
      <Knob
        label="Beat FX level/depth"
        value={depth}
        onChange={setDepth} onReset={() => setDepth(0.5)}
        valueText={`${Math.round(depth * 100)}%`}
        caption="LEVEL"
        pending={depthPending}
      />
      <button
        type="button"
        className={styles.fxOn}
        aria-pressed={on}
        aria-label="Beat FX on"
        onClick={() => setOn((current) => !current)}
      >
        ON
      </button>
      {error ? <span role="alert" className={styles.channelError}>{error}</span> : null}
    </div>
  );
}

const NO_BPM: Record<DeckId, number | null> = { a: null, b: null };

export function MixerStrip({
  crossfade = 0.5, onCrossfade, performance = false, bpmX100 = NO_BPM, masterDeck = "a",
}: MixerStripProps) {
  const [at, setAt] = useState(crossfade);
  const { preferences, update } = usePreferencesContext();
  const isolator = preferences.audio.eqCurve === "isolator";
  const crossfaderOn = preferences.audio.crossfader;
  const [curveError, setCurveError] = useState<string | null>(null);
  const [phonesMix, setPhonesMix] = useState(0);
  const [controller, setController] = useState<string | null>(null);
  const [moved, setMoved] = useState<ReadonlySet<string>>(() => new Set());
  useEffect(() => setMoved(new Set()), [controller]);
  const pending = (control: string) => controller !== null && !moved.has(control);
  const adjustPhonesMix = useCallback((next: number) => {
    setPhonesMix(next);
    void getBackend().then((backend) => backend.setHeadphoneMix(next)).catch(() => {});
  }, []);
  useControllerIntent((intent) => {
    // Already applied in the engine; the crossfader's effect below sends it
    // again, which is the same value and harmless.
    if (intent.type === "crossfader") setAt(intent.value);
    if (intent.type === "headphoneMix") setPhonesMix(intent.value);
    if ((intent.type === "crossfader" || intent.type === "headphoneMix" || intent.type === "fxDepth")
      && !moved.has(intent.type)) {
      setMoved((current) => new Set(current).add(intent.type));
    }
  });
  useEffect(() => {
    let live = true;
    let stop: (() => void) | undefined;
    void getBackend().then(async (backend) => {
      stop = backend.onControllerStatus((status) => setController(status.name));
      const status = await backend.controllerStatus();
      if (live) setController(status.name);
      else stop();
    }).catch(() => {});
    return () => {
      live = false;
      stop?.();
    };
  }, []);
  const chooseCurve = useCallback((next: boolean) => {
    update("audio", { eqCurve: next ? "isolator" : "eq" });
  }, [update]);
  // The remembered curve and crossfader reach the engine on start and on every change.
  useEffect(() => {
    void getBackend().then((backend) => backend.setEqCurve(isolator))
      .then(() => setCurveError(null))
      .catch(() => setCurveError("EQ curve could not reach the audio engine."));
  }, [isolator]);
  useEffect(() => {
    void getBackend().then((backend) => backend.setCrossfaderOn(crossfaderOn)).catch(() => {});
  }, [crossfaderOn]);
  const jogVinyl = preferences.audio.jogVinyl;
  useEffect(() => {
    void getBackend().then((backend) => backend.setJogVinyl(jogVinyl)).catch(() => {});
  }, [jogVinyl]);
  // SMART CFX: the effect is remembered, on/off lasts the session and follows the controller's button.
  const smartCfxKind = preferences.audio.smartCfxKind;
  const [smartCfxOn, setSmartCfxOn] = useState(false);
  const [smartCfxError, setSmartCfxError] = useState<string | null>(null);
  useEffect(() => {
    void getBackend().then((backend) => backend.setSmartCfxKind(smartCfxKind))
      .then(() => setSmartCfxError(null))
      .catch(() => setSmartCfxError("SMART CFX could not reach the audio engine."));
  }, [smartCfxKind]);
  useEffect(() => {
    let live = true;
    let stop: (() => void) | undefined;
    void getBackend().then(async (backend) => {
      stop = backend.onSmartCfx((state) => { if (live) setSmartCfxOn(state.on); });
      const state = await backend.smartCfxState();
      if (live) setSmartCfxOn(state.on);
      else stop();
    }).catch(() => {});
    return () => {
      live = false;
      stop?.();
    };
  }, []);
  const toggleSmartCfx = () => {
    const next = !smartCfxOn;
    setSmartCfxOn(next);
    void getBackend().then((backend) => backend.setSmartCfxOn(next))
      .then(() => setSmartCfxError(null))
      .catch(() => {
        setSmartCfxOn(!next);
        setSmartCfxError("SMART CFX could not reach the audio engine.");
      });
  };
  const track = useRef<HTMLDivElement>(null);

  useEffect(() => {
    void (async () => {
      const backend = await getBackend();
      await backend.setCrossfade(at);
    })().catch(() => {});
    onCrossfade?.(at);
  }, [at, onCrossfade]);

  const move = useCallback((clientY: number) => {
    const box = track.current?.getBoundingClientRect();
    if (!box || box.height === 0) return;
    const raw = (clientY - box.top) / box.height;
    const clamped = Math.min(Math.max(raw, 0), 1);
    // The detent, in the fader's own units.
    const snap = DETENT / box.height;
    setAt(Math.abs(clamped - 0.5) < snap ? 0.5 : clamped);
  }, []);

  return (
    <div className={styles.strip} role="group" aria-label="Mixer" data-performance={performance || undefined}>
      {performance ? (
        <div key="performance-header" className={styles.mixerHeader}>
          <span>MIXER</span>
          {controller ? (
            <span className={styles.killTitle} title={`Controller connected: ${controller}`}>
              {controller.includes("DDJ-FLX4") ? "DDJ-FLX4" : "MIDI"}
            </span>
          ) : null}
          <div role="group" aria-label="EQ curve">
            <button type="button" aria-pressed={!isolator} onClick={() => chooseCurve(false)}>EQ</button>
            <button type="button" aria-pressed={isolator} onClick={() => chooseCurve(true)}>ISOLATOR</button>
          </div>
          <button
            type="button"
            aria-pressed={crossfaderOn}
            title="Crossfader on; off leaves both decks at their channel faders"
            onClick={() => update("audio", { crossfader: !crossfaderOn })}
          >
            X-FADER
          </button>
          <button
            type="button"
            aria-pressed={smartCfxOn}
            title="Smart CFX: the CFX knobs add the chosen effect to the filter"
            onClick={toggleSmartCfx}
          >
            SMART CFX
          </button>
          <select
            aria-label="Smart CFX effect"
            value={smartCfxKind}
            onChange={(event) => update("audio", { smartCfxKind: event.currentTarget.value as SmartCfxKind })}
          >
            {SMART_CFX_KINDS.map((kind) => <option key={kind} value={kind}>{SMART_CFX[kind].label}</option>)}
          </select>
          <RecordButton folder={preferences.audio.recordingFolder} />
          {curveError ? <span role="alert">{curveError}</span> : null}
          {smartCfxError ? <span role="alert">{smartCfxError}</span> : null}
        </div>
      ) : null}
      <Channel key="a" deck="a" performance={performance} controller={controller} smartCfx={smartCfxOn ? smartCfxKind : null} />
      {performance ? (
        <div key="crossfader" className={styles.fader}>
          <Knob
            label="Headphones mix"
            value={phonesMix}
            onChange={adjustPhonesMix} onReset={() => adjustPhonesMix(0)}
            valueText={`${Math.round(phonesMix * 100)}% master`}
            caption="HP MIX"
            title="Headphones: cue on the left, master on the right"
            pending={pending("headphoneMix")}
          />
          {crossfaderOn ? (
            <>
              <span className={styles.deckLabel}>A</span>
              <Fader
                horizontal
                label="Crossfader"
                value={at}
                onChange={setAt} onReset={() => setAt(0.5)}
                detent={DETENT}
                pending={pending("crossfader")}
                valueText={at === 0.5 ? "Both decks" : at < 0.5 ? "Towards A" : "Towards B"}
              />
              <span className={styles.deckLabel}>B</span>
            </>
          ) : null}
        </div>
      ) : !crossfaderOn ? (
        <div key="crossfader" className={styles.fader} />
      ) : (
        <div key="crossfader" className={styles.fader}>
          <span className={styles.deckLabel}>A</span>
          <div
            ref={track}
            className={styles.faderTrack}
            role="slider"
            tabIndex={0}
            aria-label="Crossfader"
            aria-orientation="vertical"
            aria-valuemin={0}
            aria-valuemax={1}
            aria-valuenow={Number(at.toFixed(2))}
            aria-valuetext={!crossfaderOn ? "Off" : at === 0.5 ? "Both decks" : at < 0.5 ? "Towards A" : "Towards B"}
            aria-disabled={!crossfaderOn || undefined}
            onPointerDown={(event) => {
              event.currentTarget.setPointerCapture(event.pointerId);
              move(event.clientY);
            }}
            onPointerMove={(event) => {
              if (event.buttons === 0) return;
              move(event.clientY);
            }}
            onDoubleClick={() => setAt(0.5)}
            onKeyDown={(event) => {
              if (event.key === "ArrowUp") setAt((v) => Math.max(v - 0.05, 0));
              if (event.key === "ArrowDown") setAt((v) => Math.min(v + 0.05, 1));
            }}
          >
            <span className={styles.faderHandle} style={{ top: `${at * 100}%` }} aria-hidden />
          </div>
          <span className={styles.deckLabel}>B</span>
        </div>
      )}
      <Channel key="b" deck="b" flipped performance={performance} controller={controller} smartCfx={smartCfxOn ? smartCfxKind : null} />
      {performance ? <BeatFx key="beat-fx" bpmX100={bpmX100} masterDeck={masterDeck} depthPending={pending("fxDepth")} /> : null}
    </div>
  );
}
