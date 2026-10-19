/**
 * Track Suggestion: the library's tracks ranked by how well they would mix
 * out of the one playing, worked out here from what the library knows —
 * key, BPM, genre, rating, and what followed it in past sets.
 */
import type { RowDto } from "@/ipc/types";
import { toCamelot } from "./camelot";

/** `useTranslation()`'s function. */
type Translate = (text: string, values?: Readonly<Record<string, string | number>>) => string;

export interface SuggestionReference {
  id: string;
  /** The key it sounds in, key shift included. */
  key: string;
  /** What it is playing at, tempo included, ×100. */
  bpmX100: number;
  genre: string;
  artist: string;
}

export interface Suggestion {
  row: RowDto;
  /** 0 to 100. */
  match: number;
  reasons: string[];
}

/** How far off the playing BPM a track still scores, in percent. */
const BPM_REACH_PERCENT = 8;

/** How a key mixes out of another, 0 to 1, and why. */
export function keyMatch(key: string, reference: string, t: Translate): { score: number; reason: string | null } {
  const ours = toCamelot(reference);
  const theirs = toCamelot(key);
  if (!ours || !theirs) return { score: 0.3, reason: null };
  const n = Number(ours.slice(0, -1));
  const m = Number(theirs.slice(0, -1));
  const sameLetter = ours.slice(-1) === theirs.slice(-1);
  const step = ((m - n + 12) % 12);
  if (step === 0) return sameLetter ? { score: 1, reason: t("Same key") } : { score: 0.9, reason: t("Relative key") };
  if (step === 1 || step === 11) {
    return sameLetter
      ? { score: 0.85, reason: t("{key}: one step round the wheel", { key: theirs }) }
      : { score: 0.6, reason: t("{key}: diagonal mix", { key: theirs }) };
  }
  if (step === 2 && sameLetter) return { score: 0.5, reason: t("{key}: energy boost", { key: theirs }) };
  return { score: 0, reason: null };
}

/** How close a BPM is to the playing one, 0 to 1, double and half tempo included. */
export function bpmMatch(bpmX100: number, reference: number): { score: number; percent: number | null } {
  if (bpmX100 <= 0 || reference <= 0) return { score: 0.3, percent: null };
  const percent = Math.min(
    ...[0.5, 1, 2].map((factor) => Math.abs((bpmX100 * factor - reference) / reference) * 100),
  );
  return { score: Math.max(0, 1 - percent / BPM_REACH_PERCENT), percent };
}

/**
 * The candidates ranked best first, with what makes each one fit. `followed`
 * are tracks that came after the reference in past sets.
 */
export function rankSuggestions(
  reference: SuggestionReference,
  candidates: readonly RowDto[],
  followed: ReadonlySet<string>,
  exclude: ReadonlySet<string>,
  t: Translate,
  limit = 30,
): Suggestion[] {
  const seen = new Set<string>();
  const out: Suggestion[] = [];
  for (const row of candidates) {
    if (row.id === reference.id || exclude.has(row.id) || row.missing === true || seen.has(row.id)) continue;
    seen.add(row.id);
    const reasons: string[] = [];
    const key = keyMatch(row.key, reference.key, t);
    if (key.reason) reasons.push(key.reason);
    const bpm = bpmMatch(row.bpmX100, reference.bpmX100);
    if (bpm.percent !== null && bpm.score > 0) {
      reasons.push(bpm.percent < 0.05 ? t("Same tempo") : t("{percent} % tempo apart", { percent: bpm.percent.toFixed(1) }));
    }
    let score = 0.5 * key.score + 0.4 * bpm.score;
    if (row.genre !== "" && row.genre.toLowerCase() === reference.genre.toLowerCase()) {
      score += 0.1;
      reasons.push(t("Same genre"));
    }
    const after = followed.has(row.id);
    if (after) {
      score += 0.2;
      reasons.push(t("Followed it in your past sets"));
    }
    score += 0.02 * Math.min(Math.max(row.rating, 0), 5);
    if (row.artist !== "" && row.artist === reference.artist) score -= 0.1;
    if (key.score === 0 && bpm.score === 0 && !after) continue;
    out.push({ row, match: Math.round(Math.min(Math.max(score, 0), 1) * 100), reasons });
  }
  return out.sort((a, b) => b.match - a.match).slice(0, limit);
}
