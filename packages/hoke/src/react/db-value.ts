import { useDebugValue, useEffect, useMemo, useState } from "react";
import type { Collection } from "../db/document-contract.js";

export type DBValueState<T> =
  | { status: "loading"; value: undefined; error: undefined }
  | { status: "success"; value: T | undefined; error: undefined }
  | { status: "error"; value: undefined; error: unknown };

const loading = { status: "loading", value: undefined, error: undefined } as const;

interface LoadedValue<T> {
  selection: { collection: Collection<T>; key: string };
  state: DBValueState<T>;
}

export function useDBValue<T>(collection: Collection<T>, key: string): DBValueState<T> {
  const selection = useMemo(() => ({ collection, key }), [collection, key]);
  const [loaded, setLoaded] = useState<LoadedValue<T> | undefined>();

  useEffect(() => {
    const { collection, key } = selection;
    let active = true;
    let revision = 0;
    const publish = (state: DBValueState<T>) => {
      if (active) setLoaded({ selection, state });
    };
    const load = () => {
      const request = ++revision;
      publish(loading);

      void Promise.resolve()
        .then(() => {
          if (active && request === revision) return collection.get(key);
          return undefined;
        })
        .then(
          (value) => {
            if (active && request === revision) {
              publish({ status: "success", value, error: undefined });
            }
          },
          (error: unknown) => {
            if (active && request === revision) {
              publish({ status: "error", value: undefined, error });
            }
          },
        );
    };

    let unsubscribe: (() => void) | undefined;
    try {
      unsubscribe = collection.subscribe(key, load);
      load();
    } catch (error) {
      revision++;
      publish({ status: "error", value: undefined, error });
    }
    return () => {
      active = false;
      revision++;
      unsubscribe?.();
    };
  }, [selection]);

  const state = loaded?.selection === selection ? loaded.state : loading;
  useDebugValue(state);
  return state;
}
