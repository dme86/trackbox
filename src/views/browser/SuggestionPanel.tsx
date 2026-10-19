/**
 * Track Suggestion, beside the browser: what to play next out of the master
 * deck's track, ranked on this computer from the library's own data.
 */
import { useEffect, useMemo, useRef, useState } from "react";

import { Artwork } from "@/components/Artwork";
import { useTranslation } from "@/i18n";
import type { Backend, DeckId, KeyDisplay, RelatedCriterion, RowDto } from "@/ipc/types";
import { dragTracksToDesktop, getBackend, nativeTrackDragging } from "@/ipc/client";
import { formatBpm } from "@/lib/format";
import { formatKey } from "@/lib/preferences";
import { PAGE_SIZE } from "@/lib/rowCache";
import { rankSuggestions, type SuggestionReference } from "@/lib/suggest";
import { useControllerIntent } from "@/store/useControllerIntent";
import { setRowDragImage } from "./dragGhost";
import type { TrackDrag } from "./TrackTable";
import styles from "./SuggestionPanel.module.css";

/** How many related tracks are read per criterion: enough to rank, few enough to be quick. */
const CANDIDATES = PAGE_SIZE * 4;

async function related(backend: Backend, track: string, criterion: RelatedCriterion): Promise<RowDto[]> {
  const view = await backend.openView({ source: { kind: "related", track, criterion }, sort: "bpm", descending: false, query: "" });
  const rows: RowDto[] = [];
  for (let at = 0; at < Math.min(view.len, CANDIDATES); at += PAGE_SIZE) {
    rows.push(...await backend.fetchRows(view.viewId, at, Math.min(PAGE_SIZE, view.len - at)));
  }
  return rows;
}

function Sleeve({ row, className }: { row: Pick<RowDto, "id" | "hasArtwork">; className: string | undefined }) {
  return (
    <span className={className} aria-hidden>
      {row.hasArtwork ? <Artwork trackId={row.id} className={styles.sleeveImage} lazy /> : null}
    </span>
  );
}

export interface SuggestionPanelProps {
  /** The master deck's track as it sounds now; null with nothing loaded. */
  reference: (SuggestionReference & { title: string; hasArtwork: boolean; deck: DeckId }) | null;
  /** Tracks already on a deck. */
  exclude: readonly string[];
  libraryGeneration: number;
  keyDisplay: KeyDisplay;
  /** Which decks there are to load onto. */
  decks: readonly DeckId[];
  onLoad: (deck: DeckId, row: RowDto) => void;
  /** The controller's BROWSE knob is on this panel. */
  browseFocus?: boolean;
  /** A suggestion dragged to a deck or a playlist, as the track list's rows are. */
  onDragTracks?: (drag: TrackDrag | null) => void;
}

export function SuggestionPanel({
  reference, exclude, libraryGeneration, keyDisplay, decks, onLoad, browseFocus = false, onDragTracks,
}: SuggestionPanelProps) {
  const [found, setFound] = useState<{ id: string; rows: RowDto[]; followed: Set<string> } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [cursor, setCursor] = useState(0);
  const list = useRef<HTMLOListElement>(null);
  const t = useTranslation();
  const id = reference?.id ?? null;

  useEffect(() => {
    if (id === null) return;
    let live = true;
    setError(null);
    setCursor(0);
    void (async () => {
      const backend = await getBackend();
      const [byKey, byHistory] = await Promise.all([related(backend, id, "bpmKey"), related(backend, id, "suggestion")]);
      if (!live) return;
      // With no history behind it the backend answers `suggestion` with BPM + KEY again.
      const keyIds = new Set(byKey.map((row) => row.id));
      const fromHistory = byHistory.length !== byKey.length || byHistory.some((row) => !keyIds.has(row.id));
      setFound({ id, rows: [...byKey, ...byHistory], followed: new Set(fromHistory ? byHistory.map((row) => row.id) : []) });
    })().catch(() => {
      if (live) setError("The library could not be searched for suggestions.");
    });
    return () => { live = false; };
  }, [id, libraryGeneration]);

  const ranked = useMemo(
    () => (reference && found?.id === reference.id
      ? rankSuggestions(reference, found.rows, found.followed, new Set(exclude), t)
      : []),
    [reference, found, exclude, t],
  );
  // Into the deck that is not playing the reference.
  const target = decks.find((deck) => deck !== reference?.deck) ?? decks[0] ?? "a";
  const at = Math.min(cursor, Math.max(ranked.length - 1, 0));

  useControllerIntent((intent) => {
    if (!browseFocus || ranked.length === 0) return;
    if (intent.type === "browse") {
      setCursor(Math.min(Math.max(at + intent.steps, 0), ranked.length - 1));
    } else if (intent.type === "load") {
      const pick = ranked[at];
      if (pick) onLoad(intent.deck, pick.row);
    }
  });
  useEffect(() => {
    if (browseFocus) list.current?.querySelector("[data-cursor]")?.scrollIntoView({ block: "nearest" });
  }, [at, browseFocus]);

  // On macOS the shell drags a real file, which the decks take as a DOM drop;
  // elsewhere the row is an HTML drag source, as the track list's are.
  const dragNative = (row: RowDto, start: { x: number; y: number }) => {
    const move = (event: PointerEvent) => {
      if (Math.hypot(event.clientX - start.x, event.clientY - start.y) < 5) return;
      stop();
      onDragTracks?.({ ids: [row.id], row });
      void dragTracksToDesktop([row.id]).catch(() => {}).finally(() => {
        window.setTimeout(() => onDragTracks?.(null), 100);
      });
    };
    const stop = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", stop);
      window.removeEventListener("pointercancel", stop);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", stop);
    window.addEventListener("pointercancel", stop);
  };

  return (
    <aside className={styles.panel} aria-label="Track suggestion" data-browse-focus={browseFocus || undefined}>
      <header className={styles.header}>
        <span className={styles.heading}>TRACK SUGGESTION</span>
        {reference ? (
          <div className={styles.reference}>
            <Sleeve row={reference} className={styles.referenceSleeve} />
            <div className={styles.text}>
              <span className={styles.caption}>NEXT AFTER</span>
              <span className={styles.title}>{reference.title}</span>
              <span className={styles.artist}>
                {formatKey(reference.key, keyDisplay)} · {formatBpm(reference.bpmX100)} BPM · deck {reference.deck.toUpperCase()}
              </span>
            </div>
          </div>
        ) : (
          <span className={styles.artist}>Load a track on a deck to get suggestions.</span>
        )}
      </header>
      {error ? <p role="alert" className={styles.empty}>{error}</p> : null}
      {reference && !error && found?.id === reference.id && ranked.length === 0 ? (
        <p className={styles.empty}>Nothing in the library mixes well out of this track.</p>
      ) : null}
      <ol className={styles.list} ref={list}>
        {ranked.map(({ row, match, reasons }, index) => (
          <li
            key={row.id}
            className={styles.item}
            data-cursor={index === at || undefined}
            onClick={() => setCursor(index)}
            onDoubleClick={() => onLoad(target, row)}
            draggable={onDragTracks !== undefined && !nativeTrackDragging()}
            onPointerDown={(event) => {
              if (onDragTracks && nativeTrackDragging() && event.button === 0) dragNative(row, { x: event.clientX, y: event.clientY });
            }}
            onDragStart={(event) => {
              onDragTracks?.({ ids: [row.id], row });
              event.dataTransfer.effectAllowed = "copy";
              event.dataTransfer.setData("text/plain", row.id);
              setRowDragImage(event.currentTarget, event.dataTransfer, { x: event.clientX, y: event.clientY });
            }}
            onDragEnd={() => onDragTracks?.(null)}
            title={t("Double-click or drag onto a deck to load; double-click loads onto deck {deck}", { deck: target.toUpperCase() })}
          >
            <Sleeve row={row} className={styles.sleeve} />
            <div className={styles.text}>
              <span className={styles.title}>{row.title}</span>
              <span className={styles.artist}>{row.artist}</span>
              <span className={styles.reasons}>{reasons.join(" · ")}</span>
            </div>
            <div className={styles.meta}>
              <span className={styles.match} aria-label={t("{match} % match", { match })}>{match}%</span>
              <span className={styles.key}>{formatKey(row.key, keyDisplay)}</span>
              <span className={styles.bpm}>{row.bpmX100 > 0 ? formatBpm(row.bpmX100) : "—"}</span>
            </div>
          </li>
        ))}
      </ol>
      <footer className={styles.note}>
        Ranked on this computer from key, BPM, genre, rating and your history.
      </footer>
    </aside>
  );
}
