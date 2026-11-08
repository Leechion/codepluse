import { useMemo, useSyncExternalStore } from "react";
import { useBaseWorkspaceServices } from "./useWorkspaceServices";
import { getSourceServerDisplayNameRegistry } from "../store/sourceServerDisplayNameRegistry";

const emptyNames: ReadonlyMap<string, string> = new Map();
const subscribeToNothing = () => () => undefined;
const getEmptyNames = () => emptyNames;

export function useSourceServerDisplayNames(enabled = true): ReadonlyMap<string, string> {
  const management = useBaseWorkspaceServices().sourceServerManagementService;
  const registry = useMemo(
    () => (enabled && management ? getSourceServerDisplayNameRegistry(management) : null),
    [enabled, management],
  );
  return useSyncExternalStore(
    registry?.subscribe ?? subscribeToNothing,
    registry?.getSnapshot ?? getEmptyNames,
    registry?.getSnapshot ?? getEmptyNames,
  );
}
