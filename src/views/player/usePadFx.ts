import { useEffect, useRef, useState } from "react";

import { getBackend } from "@/ipc/client";
import type { DeckId, PadMode } from "@/ipc/types";
import { heardPadFx, padFxFor, pressPad, releasePad, type HeldPad } from "@/lib/padFx";
import { useEventCallback } from "@/store/useEventCallback";

/**
 * PAD FX on one deck's controller pads, held to be heard. Without a BPM the
 * pads do nothing, as BEAT JUMP's do. Unloading the deck, another track or a
 * controller that goes away lets go of whatever is held.
 */
export function usePadFx(deck: DeckId, trackId: string | null, playingBpmX100: number) {
  const held = useRef<HeldPad[]>([]);
  const beatMs = useRef(0);
  const [heard, setHeard] = useState<HeldPad | null>(null);
  const apply = useEventCallback((next: HeldPad[]) => {
    const before = heardPadFx(held.current);
    held.current = next;
    setHeard(next.at(-1) ?? null);
    const fx = heardPadFx(next);
    if (fx === before) return;
    void getBackend().then((backend) => backend.setPadFx(deck, fx, beatMs.current)).catch(() => {});
  });
  const press = useEventCallback((mode: PadMode, pad: number) => {
    const fx = padFxFor(mode, pad);
    if (!fx || playingBpmX100 <= 0) return;
    beatMs.current = 6_000_000 / playingBpmX100;
    apply(pressPad(held.current, pad, fx));
  });
  const release = useEventCallback((pad: number) => apply(releasePad(held.current, pad)));
  const releaseAll = useEventCallback(() => apply([]));
  useEffect(() => releaseAll, [trackId, releaseAll]);
  useEffect(() => {
    let live = true;
    let stop: (() => void) | undefined;
    void getBackend().then((backend) => {
      if (live) stop = backend.onControllerStatus((status) => { if (!status.name) releaseAll(); });
    }).catch(() => {});
    return () => {
      live = false;
      stop?.();
    };
  }, [releaseAll]);
  return { press, release, heard };
}
