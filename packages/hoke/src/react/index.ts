import {
  useCallback,
  useDebugValue,
  useEffect,
  useMemo,
  useRef,
  useSyncExternalStore,
} from "react";
import type { Store } from "../store.js";

/** Subscribe to the complete immutable store snapshot. */
export function useValue<T>(store: Store<T>): T {
  return useStore(store, identity);
}

function identity<T>(value: T): T {
  return value;
}

/** Subscribe to a selected value; equal selections retain their previous reference. */
export function useStore<T, S>(
  store: Store<T>,
  selector: (value: T) => S,
  isEqual: (previous: S, next: S) => boolean = Object.is,
): S {
  const committed = useRef<{ value: S } | null>(null);
  const subscribe = useCallback((listener: () => void) => store.subscribe(listener), [store]);

  const [getSelection, getServerSelection] = useMemo(() => {
    let initialized = false;
    let snapshot: T;
    let selection: S;

    // The cache belongs to this render's selector, not to the shared ref: a
    // suspended/concurrent render must not overwrite another render's cache.
    const select = (nextSnapshot: T): S => {
      if (initialized && Object.is(snapshot, nextSnapshot)) return selection;
      const nextSelection = selector(nextSnapshot);
      if (!initialized) {
        const previous = committed.current;
        selection =
          previous !== null && isEqual(previous.value, nextSelection)
            ? previous.value
            : nextSelection;
        initialized = true;
      } else if (!isEqual(selection, nextSelection)) {
        selection = nextSelection;
      }
      snapshot = nextSnapshot;
      return selection;
    };

    return [() => select(store.getSnapshot()), () => select(store.getServerSnapshot())] as const;
  }, [store, selector, isEqual]);

  const value = useSyncExternalStore(subscribe, getSelection, getServerSelection);
  useEffect(() => {
    committed.current = { value };
  }, [value]);
  useDebugValue(value);
  return value;
}

export { type DBValueState, useDBValue } from "./db-value.js";
