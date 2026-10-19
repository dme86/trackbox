/**
 * The PERFORMANCE mixer's knobs and faders.
 *
 * The A-GAIN knob and the crossfader of the measured strip, made reusable:
 * the same ring, tick and handle, a size up and with the travel drawn as an
 * arc in the accent colour, so a mixer full of them reads at a glance.
 */
import { useRef, type CSSProperties } from "react";

import styles from "./MixerStrip.module.css";

/** A knob's travel in pixels: a full sweep is this far under the pointer. */
const KNOB_TRAVEL = 140;
/** Where the tick points at the two ends of the travel, either side of up. */
const SWEEP = 150;

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

/** Shown while a controller's knob has not been moved: the hardware cannot report where it is. */
const PENDING_HINT = "Not yet read from the controller: move it there to take it over";

export interface KnobProps {
  label: string;
  value: number;
  min?: number;
  max?: number;
  /** Where the arc starts: the centre detent of an EQ, the bottom of a level. */
  origin?: number;
  onChange: (value: number) => void;
  /** Double-click: back to where the knob rests. */
  onReset?: () => void;
  valueText?: string;
  /** Printed under the knob. */
  caption?: string;
  title?: string;
  /** A controller's knob that has not moved yet, so its position is unknown. */
  pending?: boolean;
}

export function Knob({
  label, value, min = 0, max = 1, origin = min, onChange, onReset, valueText, caption, title, pending = false,
}: KnobProps) {
  const drag = useRef<{ y: number; from: number } | null>(null);
  const span = max - min;
  const at = clamp((value - min) / span, 0, 1);
  const from = clamp((origin - min) / span, 0, 1);
  const degrees = (fraction: number) => fraction * SWEEP * 2;
  const low = degrees(Math.min(at, from));
  const high = degrees(Math.max(at, from));
  const arc: CSSProperties = {
    background: `conic-gradient(from ${360 - SWEEP}deg, var(--c-raised) 0deg ${low}deg, var(--c-accent) ${low}deg ${high}deg, var(--c-raised) ${high}deg ${SWEEP * 2}deg, transparent ${SWEEP * 2}deg)`,
  };
  const set = (next: number) => onChange(clamp(next, min, max));
  return (
    <div className={styles.knobBlock}>
      <div
        className={styles.perfKnob}
        role="slider"
        tabIndex={0}
        aria-label={label}
        aria-valuemin={min}
        aria-valuemax={max}
        aria-valuenow={Number(value.toFixed(2))}
        aria-valuetext={valueText}
        title={pending ? PENDING_HINT : title}
        data-pending={pending || undefined}
        onPointerDown={(event) => {
          event.currentTarget.setPointerCapture(event.pointerId);
          drag.current = { y: event.clientY, from: value };
        }}
        onPointerMove={(event) => {
          const held = drag.current;
          if (!held) return;
          // Up turns it clockwise, as a knob under a finger does.
          set(held.from + ((held.y - event.clientY) / KNOB_TRAVEL) * span);
        }}
        onPointerUp={() => { drag.current = null; }}
        onPointerCancel={() => { drag.current = null; }}
        onDoubleClick={onReset}
        onKeyDown={(event) => {
          if (event.key === "ArrowUp" || event.key === "ArrowRight") set(value + span / 40);
          else if (event.key === "ArrowDown" || event.key === "ArrowLeft") set(value - span / 40);
          else return;
          event.preventDefault();
        }}
      >
        <span className={styles.knobArc} style={arc} aria-hidden />
        <span className={styles.perfTick} style={{ rotate: `${at * SWEEP * 2 - SWEEP}deg` }} aria-hidden />
      </div>
      {caption ? <span className={styles.knobCaption}>{caption}</span> : null}
    </div>
  );
}

export interface FaderProps {
  label: string;
  /** 0 to 1. A vertical fader is 1 at the top, a horizontal one at the right. */
  value: number;
  onChange: (value: number) => void;
  onReset?: () => void;
  horizontal?: boolean;
  valueText?: string;
  /** Snaps to the middle within this many pixels, as a crossfader's detent does. */
  detent?: number;
  title?: string;
  pending?: boolean;
}

export function Fader({
  label, value, onChange, onReset, horizontal = false, valueText, detent = 0, title, pending = false,
}: FaderProps) {
  const move = (element: HTMLElement, clientX: number, clientY: number) => {
    const box = element.getBoundingClientRect();
    const length = horizontal ? box.width : box.height;
    if (length === 0) return;
    const raw = horizontal ? (clientX - box.left) / length : 1 - (clientY - box.top) / length;
    const next = clamp(raw, 0, 1);
    onChange(detent > 0 && Math.abs(next - 0.5) < detent / length ? 0.5 : next);
  };
  const at = clamp(value, 0, 1);
  return (
    <div
      className={horizontal ? styles.hFader : styles.vFader}
      role="slider"
      tabIndex={0}
      aria-label={label}
      aria-orientation={horizontal ? "horizontal" : "vertical"}
      aria-valuemin={0}
      aria-valuemax={1}
      aria-valuenow={Number(at.toFixed(2))}
      aria-valuetext={valueText}
      title={pending ? PENDING_HINT : title}
      data-pending={pending || undefined}
      onPointerDown={(event) => {
        event.currentTarget.setPointerCapture(event.pointerId);
        move(event.currentTarget, event.clientX, event.clientY);
      }}
      onPointerMove={(event) => {
        if (event.buttons === 0) return;
        move(event.currentTarget, event.clientX, event.clientY);
      }}
      onDoubleClick={onReset}
      onKeyDown={(event) => {
        const up = horizontal ? "ArrowRight" : "ArrowUp";
        const down = horizontal ? "ArrowLeft" : "ArrowDown";
        if (event.key === up) onChange(clamp(at + 0.05, 0, 1));
        else if (event.key === down) onChange(clamp(at - 0.05, 0, 1));
        else return;
        event.preventDefault();
      }}
    >
      {/* A channel fader shows how far up it is; a crossfader has no "up". */}
      {horizontal ? null : <span className={styles.faderFill} style={{ height: `${at * 100}%` }} aria-hidden />}
      <span
        className={styles.faderCap}
        style={horizontal ? { left: `${at * 100}%` } : { top: `${(1 - at) * 100}%` }}
        aria-hidden
      />
    </div>
  );
}
