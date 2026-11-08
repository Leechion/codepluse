export type {
  SettingsScopeContext,
  SettingsScopeDevice,
  SettingsScopeDeviceState,
  SettingsScopeGroup,
  SettingsScopeOption,
  SettingsScopeResolution,
  SettingsScopeState,
  SettingsUserScope,
} from "./contract";
export {
  LOCAL_USER_SCOPE_KEY,
  buildSettingsScopeGroups,
  getPluginWorkspaceKey,
  isPluginScopeWorkspaceConnected,
  isUserScopeKey,
  parseRemoteUserScopeKey,
  remoteUserScopeKey,
  resolveDefaultSettingsScopeKey,
  resolveSettingsScope,
} from "./domain/scope";
export { useSettingsScope } from "./app/useSettingsScope";
export {
  useSettingsScopeForm,
  type SettingsScopeForm,
} from "./app/useSettingsScopeForm";
export {
  SettingsScopeDeviceProvider,
  useSettingsScopeDevice,
} from "./ui/SettingsScopeDevice";
export {
  SettingsScopeBlockedNotice,
  resolveSettingsScopeBlock,
  type SettingsScopeBlock,
} from "./ui/SettingsScopeBlockedNotice";
