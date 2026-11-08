import type { WebRemoteControlDeviceNavigation } from "./contract";
export function deviceNavigation(chooseOtherDevice: () => void): WebRemoteControlDeviceNavigation {
  return { deviceName: "Example computer", chooseOtherDevice };
}
