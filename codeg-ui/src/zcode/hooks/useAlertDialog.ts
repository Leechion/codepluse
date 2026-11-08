import { useAlertDialogStore } from "../store/alertDialogStore";

export function useAlertDialog() {
  return useAlertDialogStore((state) => state.requestAlert);
}
