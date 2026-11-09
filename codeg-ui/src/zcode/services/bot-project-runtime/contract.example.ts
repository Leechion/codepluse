import type { BotProjectRuntimePort, BotRemoteTarget } from "./contract";
import { createBotProjectRuntime } from ".";
export async function example(port: BotProjectRuntimePort, target: BotRemoteTarget) {
  return createBotProjectRuntime(port).isConnected(target);
}
