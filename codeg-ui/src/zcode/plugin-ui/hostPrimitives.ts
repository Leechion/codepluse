/** Shared sandbox transport/presentation and existing session admission binding. */
export { createPluginUiMessagePortTransport } from "./adapters/messagePortTransport";
export { createPluginUiPagePlane } from "./adapters/pluginUiPagePlane";
export { readPluginUiStyleVariables } from "./adapters/readPluginUiStyleVariables";
export {
  sandboxPages,
  onSandboxPageRemoved,
  type SandboxPageOwner,
} from "./adapters/sandboxPageOwners";
export {
  getPluginUiSessionActions,
  registerPluginUiSessionActions,
} from "./adapters/pluginUiSessionActions";
