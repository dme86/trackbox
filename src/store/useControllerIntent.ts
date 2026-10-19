import { useEffect } from "react";

import { getBackend } from "@/ipc/client";
import type { ControllerIntent } from "@/ipc/types";
import { useEventCallback } from "./useEventCallback";

/** Calls `listener` with every intent a connected DJ controller sends. */
export function useControllerIntent(listener: (intent: ControllerIntent) => void): void {
  const handle = useEventCallback(listener);
  useEffect(() => {
    let live = true;
    let stop: (() => void) | undefined;
    void getBackend()
      .then((backend) => {
        if (live) stop = backend.onControllerIntent(handle);
      })
      .catch(() => {});
    return () => {
      live = false;
      stop?.();
    };
  }, [handle]);
}
