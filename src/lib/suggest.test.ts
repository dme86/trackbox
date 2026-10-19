import { describe, expect, it } from "vitest";

import type { RowDto } from "@/ipc/types";
import { bpmMatch, keyMatch, rankSuggestions } from "./suggest";

const row = (id: string, key: string, bpmX100: number, extra: Partial<RowDto> = {}): RowDto => ({
  id, trackNo: 0, title: id, artist: "", album: "", genre: "", label: "", comment: "",
  bpmX100, key, durationSec: 300, rating: 0, analysed: 1, dateAdded: "", releaseDate: "",
  hotCues: [], artworkHue: 0, hasArtwork: false, ...extra,
});

const reference = { id: "ref", key: "Am", bpmX100: 12_800, genre: "House", artist: "X" };
const t = (text: string, values?: Readonly<Record<string, string | number>>) =>
  text.replace(/\{(\w+)\}/g, (_, key: string) => String(values?.[key] ?? `{${key}}`));

describe("keyMatch", () => {
  it("ranks the wheel's moves", () => {
    expect(keyMatch("Am", "Am", t).score).toBe(1);
    expect(keyMatch("C", "Am", t).reason).toBe("Relative key");
    expect(keyMatch("Em", "Am", t).score).toBe(0.85);
    expect(keyMatch("Em", "Am", t).reason).toBe("9A: one step round the wheel");
    expect(keyMatch("G", "Am", t).score).toBe(0.6);
    expect(keyMatch("Bm", "Am", t).score).toBe(0.5);
    expect(keyMatch("Ebm", "Am", t).score).toBe(0);
  });
});

describe("bpmMatch", () => {
  it("counts double and half tempo as close", () => {
    expect(bpmMatch(12_800, 12_800).score).toBe(1);
    expect(bpmMatch(6_400, 12_800).score).toBe(1);
    expect(bpmMatch(13_440, 12_800).percent).toBeCloseTo(5, 6);
    expect(bpmMatch(15_000, 12_800).score).toBe(0);
  });
});

describe("rankSuggestions", () => {
  it("puts the best mix first and leaves out the decks' own tracks", () => {
    const ranked = rankSuggestions(reference, [
      row("far", "Ebm", 15_000),
      row("ok", "Em", 13_000),
      row("best", "Am", 12_800, { genre: "house" }),
      row("loaded", "Am", 12_800),
      row("ref", "Am", 12_800),
    ], new Set(), new Set(["loaded"]), t);
    expect(ranked.map((s) => s.row.id)).toEqual(["best", "ok"]);
    expect(ranked[0]?.reasons).toContain("Same genre");
  });

  it("lifts what followed it in past sets", () => {
    const ranked = rankSuggestions(reference, [row("a", "Em", 12_800), row("b", "Em", 12_800)], new Set(["b"]), new Set(), t);
    expect(ranked[0]?.row.id).toBe("b");
    expect(ranked[0]?.reasons).toContain("Followed it in your past sets");
  });
});
