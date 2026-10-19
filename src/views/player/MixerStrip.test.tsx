/** @vitest-environment jsdom */
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { DEFAULT_PREFERENCES } from "@/lib/preferences";
import type { BeatFxSettings, ControllerIntent, Meters, RecordingStatus, SmartCfxKind, SmartCfxState } from "@/ipc/types";
import { PreferencesProvider } from "@/store/usePreferences";
import { MixerStrip, recordingClock } from "./MixerStrip";

declare global { var IS_REACT_ACT_ENVIRONMENT: boolean; }

const setChannelKill = vi.fn(() => Promise.resolve());
const setChannelBand = vi.fn(() => Promise.resolve());
const setChannelVolume = vi.fn(() => Promise.resolve());
const setEqCurve = vi.fn(() => Promise.resolve());
const setChannelCue = vi.fn(() => Promise.resolve());
const setChannelFilter = vi.fn(() => Promise.resolve());
const setHeadphoneMix = vi.fn(() => Promise.resolve());
const setCrossfaderOn = vi.fn(() => Promise.resolve());
const setBeatFx = vi.fn((_fx: BeatFxSettings) => Promise.resolve());
const setSmartCfxOn = vi.fn((_on: boolean) => Promise.resolve());
const setSmartCfxKind = vi.fn((_kind: SmartCfxKind) => Promise.resolve());
const smartCfxListeners = new Set<(state: SmartCfxState) => void>();
const update = vi.fn();
const intents = new Set<(intent: ControllerIntent) => void>();
const statuses = new Set<(status: { name: string | null }) => void>();
const meterListeners = new Set<(meters: Meters) => void>();
const recordListeners = new Set<(status: RecordingStatus) => void>();
const recording = (active: boolean, seconds = 0): RecordingStatus =>
  ({ path: "/Music/Trackbox Recordings/Trackbox 1.flac", seconds, droppedFrames: 0, error: null, active });
const startRecording = vi.fn((_folder: string | null) => Promise.resolve(recording(true)));
const stopRecording = vi.fn(() => Promise.resolve(recording(false, 65)));
vi.mock("@/ipc/client", () => ({
  getBackend: () => Promise.resolve({
    startRecording, stopRecording,
    recordingStatus: () => Promise.resolve(null),
    onRecordingStatus: (listener: (status: RecordingStatus) => void) => {
      recordListeners.add(listener);
      return () => recordListeners.delete(listener);
    },
    setChannelKill, setChannelBand, setChannelVolume, setEqCurve, setChannelCue, setChannelFilter, setHeadphoneMix,
    setCrossfaderOn, setBeatFx, setSmartCfxOn, setSmartCfxKind,
    smartCfxState: () => Promise.resolve({ on: false, kind: "echo" }),
    onSmartCfx: (listener: (state: SmartCfxState) => void) => {
      smartCfxListeners.add(listener);
      return () => smartCfxListeners.delete(listener);
    },
    setChannelTrim: () => Promise.resolve(), setCrossfade: () => Promise.resolve(), setJogVinyl: () => Promise.resolve(),
    onControllerIntent: (listener: (intent: ControllerIntent) => void) => {
      intents.add(listener);
      return () => intents.delete(listener);
    },
    onControllerStatus: (listener: (status: { name: string | null }) => void) => {
      statuses.add(listener);
      return () => statuses.delete(listener);
    },
    controllerStatus: () => Promise.resolve({ name: null }),
    onMeters: (listener: (meters: Meters) => void) => {
      meterListeners.add(listener);
      return () => meterListeners.delete(listener);
    },
  }),
}));

const send = (intent: ControllerIntent) => act(() => { for (const listener of intents) listener(intent); });

let host: HTMLDivElement;
let root: Root;

function mount(
  overrides: Record<string, { key: string; shiftKey?: boolean }>,
  performance = false,
  audio: Partial<typeof DEFAULT_PREFERENCES.audio> = {},
) {
  const preferences = {
    ...DEFAULT_PREFERENCES,
    keyboard: { ...DEFAULT_PREFERENCES.keyboard, overrides },
    audio: { ...DEFAULT_PREFERENCES.audio, ...audio },
  };
  act(() => root.render(
    <PreferencesProvider value={{ preferences, update, reset: vi.fn() }}>
      <MixerStrip performance={performance} bpmX100={{ a: 12_000, b: 12_800 }} masterDeck="b" />
    </PreferencesProvider>,
  ));
}

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  setChannelKill.mockClear();
  setChannelBand.mockClear();
  setChannelVolume.mockClear();
  setEqCurve.mockClear();
  setChannelCue.mockClear();
  setChannelFilter.mockClear();
  setHeadphoneMix.mockClear();
  setCrossfaderOn.mockClear();
  setBeatFx.mockClear();
  setSmartCfxOn.mockClear();
  setSmartCfxKind.mockClear();
  update.mockClear();
});
afterEach(() => { act(() => root.unmount()); host.remove(); });

const press = (init: KeyboardEventInit) =>
  act(() => { window.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, ...init })); });

it("records from REC into the chosen folder, shows the running time and stops on a second press", async () => {
  startRecording.mockClear();
  stopRecording.mockClear();
  mount({}, true, { recordingFolder: "/Volumes/Sets" });
  await act(async () => {});
  const rec = () => [...host.querySelectorAll("button")].find((b) => b.textContent?.startsWith("REC"));
  expect(rec()?.getAttribute("aria-pressed")).toBe("false");
  act(() => rec()?.click());
  await act(async () => {});
  expect(startRecording).toHaveBeenCalledWith("/Volumes/Sets");
  expect(rec()?.getAttribute("aria-pressed")).toBe("true");
  act(() => { for (const listener of recordListeners) listener(recording(true, 3_725)); });
  expect(rec()?.textContent).toBe("REC 1:02:05");
  act(() => rec()?.click());
  await act(async () => {});
  expect(stopRecording).toHaveBeenCalled();
  expect(rec()?.textContent).toBe("REC");
  expect(rec()?.title).toContain("Trackbox 1.flac");
});

it("says when the recording could not start and when the disk left gaps", async () => {
  startRecording.mockImplementationOnce(() => Promise.reject(new Error("The recording could not be started.")));
  mount({}, true);
  await act(async () => {});
  const rec = () => [...host.querySelectorAll("button")].find((b) => b.textContent?.startsWith("REC"));
  act(() => rec()?.click());
  await act(async () => {});
  expect(host.querySelector('[role="alert"]')?.textContent).toContain("could not be started");
  act(() => { for (const listener of recordListeners) listener({ ...recording(true, 10), droppedFrames: 512 }); });
  expect(host.querySelector('[role="alert"]')?.textContent).toContain("gaps");
});

it("prints a recording's length as minutes or hours", () => {
  expect(recordingClock(0)).toBe("0:00");
  expect(recordingClock(65.9)).toBe("1:05");
  expect(recordingClock(3_725)).toBe("1:02:05");
});

it("meters each channel from the engine's meter tick, and stays empty without readings", async () => {
  mount({}, true);
  await act(async () => {});
  const level = (deck: string) =>
    host.querySelector<HTMLElement>(`[role="meter"][aria-label="Deck ${deck} level"]`)?.getAttribute("aria-valuenow");
  expect(level("A")).toBe("0");
  expect(level("B")).toBe("0");
  const tick = (meters: Partial<Meters>) => act(() => {
    for (const listener of meterListeners) {
      listener({ peakLeft: 0, peakRight: 0, master: 1, reduction: 0, ...meters });
    }
  });
  // An engine without channel peaks leaves the bars alone.
  tick({ peakLeft: 1, peakRight: 1 });
  expect(level("A")).toBe("0");
  // -6 dBFS is -7 on the 1 dB scale: 37 of 44 dB.
  tick({ channelPeaks: [0.5, 0] });
  expect(level("A")).toBe(String(Math.round((37 / 44) * 100)));
  expect(level("B")).toBe("0");
});

it("toggles the assigned deck's band kill from the keyboard", async () => {
  mount({ "a.eqKillLow": { key: "z" }, "b.eqKillHigh": { key: "u" } });
  press({ key: "z" });
  await act(async () => {});
  expect(setChannelKill).toHaveBeenCalledWith("a", "low", true);
  press({ key: "z" });
  await act(async () => {});
  expect(setChannelKill).toHaveBeenLastCalledWith("a", "low", false);
  press({ key: "u" });
  await act(async () => {});
  expect(setChannelKill).toHaveBeenLastCalledWith("b", "high", true);
});

it("does nothing while the kill keys are unbound", async () => {
  mount({});
  press({ key: "z" });
  await act(async () => {});
  expect(setChannelKill).not.toHaveBeenCalled();
});

it("exposes actual post-EQ channel faders and neutral 3-band EQ only in PERFORMANCE", async () => {
  mount({}, true);
  const left = host.querySelector<HTMLElement>('[role="slider"][aria-label="Deck A channel volume"]');
  const right = host.querySelector<HTMLElement>('[role="slider"][aria-label="Deck B channel volume"]');
  expect(left).not.toBeNull();
  expect(right).not.toBeNull();
  expect(left?.getAttribute("aria-valuenow")).toBe("1");
  const high = host.querySelector<HTMLElement>('[role="slider"][aria-label="Deck A HIGH EQ"]');
  const low = host.querySelector<HTMLElement>('[role="slider"][aria-label="Deck B LOW EQ"]');
  expect(high?.getAttribute("aria-valuenow")).toBe("0.5");
  expect(low?.getAttribute("aria-valuenow")).toBe("0.5");

  act(() => {
    high?.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    left?.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
  });
  await act(async () => {});
  expect(setChannelBand).toHaveBeenCalledWith("a", "high", 0.5);
  expect(setChannelVolume).toHaveBeenCalledWith("a", 1);

  act(() => { high?.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowUp", bubbles: true })); });
  act(() => { left?.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true })); });
  await act(async () => {});
  expect(setChannelBand).toHaveBeenLastCalledWith("a", "high", 0.525);
  expect(setChannelVolume).toHaveBeenLastCalledWith("a", 0.95);

  const isolator = [...host.querySelectorAll("button")].find((button) => button.textContent === "ISOLATOR");
  act(() => isolator?.click());
  await act(async () => {});
  expect(update).toHaveBeenCalledWith("audio", { eqCurve: "isolator" });
});

it("sends the remembered EQ curve and crossfader to the engine", async () => {
  mount({}, true, { eqCurve: "isolator", crossfader: false });
  await act(async () => {});
  expect(setEqCurve).toHaveBeenCalledWith(true);
  expect(setCrossfaderOn).toHaveBeenCalledWith(false);
  expect(host.querySelector('[aria-label="Crossfader"]')).toBeNull();
  const toggle = [...host.querySelectorAll("button")].find((button) => button.textContent === "X-FADER");
  act(() => toggle?.click());
  expect(update).toHaveBeenCalledWith("audio", { crossfader: true });
});

it("times BEAT FX to the beat and follows the controller's FX section", async () => {
  mount({}, true);
  await act(async () => {});
  // One beat at the master deck's 128 BPM.
  expect(setBeatFx).toHaveBeenLastCalledWith({ on: false, kind: "echo", target: "master", depth: 0.5, beatMs: 60_000 / 128 });
  send({ type: "fxChannel", deck: "a", selected: true });
  send({ type: "fxSelect", previous: false });
  send({ type: "fxBeat", longer: true });
  send({ type: "fxDepth", value: 0.25 });
  send({ type: "fxToggle", off: false });
  await act(async () => {});
  // Two beats at deck A's 120 BPM, on deck A.
  expect(setBeatFx).toHaveBeenLastCalledWith({ on: true, kind: "reverb", target: "a", depth: 0.25, beatMs: 1_000 });
  send({ type: "fxToggle", off: true });
  await act(async () => {});
  expect(setBeatFx.mock.lastCall?.[0].on).toBe(false);
});

it("keeps the measured EXPORT mixer compact and without PERFORMANCE-only controls", async () => {
  mount({});
  await act(async () => {});
  expect(host.querySelector('[data-performance]')).toBeNull();
  expect(host.querySelector('[aria-label="Deck A channel volume"]')).toBeNull();
  expect(host.querySelector('[aria-label="Deck B LOW EQ"]')).toBeNull();
  expect(host.querySelector('button[aria-label="Deck A headphone cue"]')).toBeNull();
  expect(host.querySelector('[aria-label="Headphones mix"]')).toBeNull();
});

it("sends a channel to the headphones and sets the filter and the headphones mix", async () => {
  mount({}, true);
  const cue = host.querySelector<HTMLButtonElement>('button[aria-label="Deck B headphone cue"]');
  act(() => cue?.click());
  await act(async () => {});
  expect(setChannelCue).toHaveBeenCalledWith("b", true);
  expect(cue?.getAttribute("aria-pressed")).toBe("true");

  const filter = host.querySelector<HTMLElement>('[role="slider"][aria-label="Deck A filter"]');
  expect(filter?.getAttribute("aria-valuenow")).toBe("0.5");
  act(() => { filter?.dispatchEvent(new MouseEvent("dblclick", { bubbles: true })); });
  await act(async () => {});
  expect(setChannelFilter).toHaveBeenCalledWith("a", 0.5);

  const mix = host.querySelector<HTMLElement>('[role="slider"][aria-label="Headphones mix"]');
  act(() => { mix?.dispatchEvent(new MouseEvent("dblclick", { bubbles: true })); });
  await act(async () => {});
  expect(setHeadphoneMix).toHaveBeenCalledWith(0);
});

it("follows a controller's knobs and toggles CUE from its button", async () => {
  mount({}, true);
  await act(async () => {});
  send({ type: "volume", deck: "a", value: 0.25 });
  send({ type: "eq", deck: "b", band: "low", value: 0 });
  send({ type: "filter", deck: "a", value: 0.1 });
  send({ type: "headphoneMix", value: 0.8 });
  send({ type: "channelCue", deck: "a" });
  await act(async () => {});
  const now = (label: string) =>
    host.querySelector(`[role="slider"][aria-label="${label}"]`)?.getAttribute("aria-valuenow");
  expect(now("Deck A channel volume")).toBe("0.25");
  expect(now("Deck B LOW EQ")).toBe("0");
  expect(now("Deck A filter")).toBe("0.1");
  expect(now("Headphones mix")).toBe("0.8");
  expect(setChannelCue).toHaveBeenCalledWith("a", true);
  // The engine already has the knob positions; the strip does not send them back.
  expect(setChannelVolume).not.toHaveBeenCalled();
});

it("sends the remembered SMART CFX effect, toggles it and follows the controller's button", async () => {
  mount({}, true, { smartCfxKind: "noise" });
  await act(async () => {});
  expect(setSmartCfxKind).toHaveBeenCalledWith("noise");
  const smart = () => [...host.querySelectorAll("button")].find((b) => b.textContent === "SMART CFX");
  const filterTitle = () => host.querySelector('[aria-label="Deck A filter"]')?.getAttribute("title");
  expect(smart()?.getAttribute("aria-pressed")).toBe("false");
  expect(filterTitle()).toContain("Left: low pass");

  act(() => smart()?.click());
  await act(async () => {});
  expect(setSmartCfxOn).toHaveBeenCalledWith(true);
  expect(smart()?.getAttribute("aria-pressed")).toBe("true");
  expect(filterTitle()).toBe("Smart CFX: low pass + noise ← → high pass + noise");

  const effect = host.querySelector<HTMLSelectElement>('select[aria-label="Smart CFX effect"]');
  expect(effect?.value).toBe("noise");
  act(() => {
    if (!effect) return;
    effect.value = "crush";
    effect.dispatchEvent(new Event("change", { bubbles: true }));
  });
  expect(update).toHaveBeenCalledWith("audio", { smartCfxKind: "crush" });

  // The FLX4's SMART CFX button, reported by the engine.
  act(() => { for (const listener of smartCfxListeners) listener({ on: false, kind: "noise" }); });
  expect(smart()?.getAttribute("aria-pressed")).toBe("false");
  expect(filterTitle()).toContain("Left: low pass");
});

it("shows a connected controller in the mixer header", async () => {
  mount({}, true);
  await act(async () => {});
  act(() => { for (const listener of statuses) listener({ name: "DDJ-FLX4" }); });
  expect(host.textContent).toContain("DDJ-FLX4");
});

it("marks a controller's knobs as unknown until they move", async () => {
  mount({}, true);
  await act(async () => {});
  const high = () => host.querySelector('[aria-label="Deck A HIGH EQ"]');
  expect(high()?.hasAttribute("data-pending")).toBe(false);
  act(() => { for (const listener of statuses) listener({ name: "DDJ-FLX4" }); });
  expect(high()?.hasAttribute("data-pending")).toBe(true);
  expect(host.querySelector('[aria-label="Crossfader"]')?.hasAttribute("data-pending")).toBe(true);
  send({ type: "eq", deck: "a", band: "high", value: 0.7 });
  send({ type: "crossfader", value: 0.2 });
  await act(async () => {});
  expect(high()?.hasAttribute("data-pending")).toBe(false);
  expect(host.querySelector('[aria-label="Deck A MID EQ"]')?.hasAttribute("data-pending")).toBe(true);
  expect(host.querySelector('[aria-label="Crossfader"]')?.hasAttribute("data-pending")).toBe(false);
});
