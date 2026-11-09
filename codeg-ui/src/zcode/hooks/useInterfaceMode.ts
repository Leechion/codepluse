import { useZCodeStoreWithDefault } from "../store/StoreProvider";

export function useIsOfficeMode(): boolean {
  return useZCodeStoreWithDefault((state) => state.interfaceMode === "office", false);
}
