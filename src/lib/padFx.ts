/**
 * PAD FX: Trackbox's own bank for the controller pads in PAD FX1 and PAD FX2,
 * and which held pad is heard. Momentary: an effect lasts while its pad is held.
 */
import type { PadFx, PadMode } from "@/ipc/types";

/** Pads 1–8, left to right and top row first. */
const BANKS: Partial<Record<PadMode, readonly PadFx[]>> = {
  padFx1: [
    { kind: "echo", beats: 0.25 }, { kind: "echo", beats: 0.5 }, { kind: "echo", beats: 1 }, { kind: "echo", beats: 2 },
    { kind: "reverb", beats: 1 }, { kind: "reverb", beats: 4 }, { kind: "trans", beats: 0.25 }, { kind: "trans", beats: 0.125 },
  ],
  padFx2: [
    { kind: "roll", beats: 0.0625 }, { kind: "roll", beats: 0.125 }, { kind: "roll", beats: 0.25 }, { kind: "roll", beats: 0.5 },
    { kind: "roll", beats: 1 }, { kind: "hpf", beats: 2 }, { kind: "lpf", beats: 2 }, { kind: "crush", beats: 1 },
  ],
};

/** The effect on `pad` (0–7) in `mode`, or null outside the PAD FX modes. */
export function padFxFor(mode: PadMode, pad: number): PadFx | null {
  return BANKS[mode]?.[pad] ?? null;
}

export interface HeldPad {
  pad: number;
  fx: PadFx;
}

/** Held pads, oldest first: the last one pressed is heard. */
export function pressPad(held: readonly HeldPad[], pad: number, fx: PadFx): HeldPad[] {
  return [...held.filter((entry) => entry.pad !== pad), { pad, fx }];
}

export function releasePad(held: readonly HeldPad[], pad: number): HeldPad[] {
  return held.filter((entry) => entry.pad !== pad);
}

export function heardPadFx(held: readonly HeldPad[]): PadFx | null {
  return held.at(-1)?.fx ?? null;
}

/** `ECHO 1/4`, `REVERB L`, `HPF`: what a pad says on screen. */
export function padFxLabel(fx: PadFx): string {
  const beats = fx.beats >= 1 ? String(fx.beats) : `1/${Math.round(1 / fx.beats)}`;
  switch (fx.kind) {
    case "reverb": return fx.beats > 1 ? "REVERB L" : "REVERB S";
    case "hpf": return "HPF";
    case "lpf": return "LPF";
    case "crush": return "CRUSH";
    default: return `${fx.kind.toUpperCase()} ${beats}`;
  }
}
