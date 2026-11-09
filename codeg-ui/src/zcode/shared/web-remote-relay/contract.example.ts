import { parseWebRemoteControlRelayV2IncomingMessage } from "./contract";
export const matchedDevice = parseWebRemoteControlRelayV2IncomingMessage({
  type: "auth_ack",
  device_sid: "device-example",
  pair_status: "matched",
});
