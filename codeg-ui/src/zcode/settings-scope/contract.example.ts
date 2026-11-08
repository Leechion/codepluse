/**
 * settings-scope 标准调用示例：配置目录目录表 + 管理快照 → 菜单分组；选中 key → 读写目标。
 */
import type { ConfigWorkspaceTarget, SettingsScopeDeviceState } from "./contract";
import { buildSettingsScopeGroups, resolveSettingsScope } from ".";

export function exampleSettingsScope(params: {
  targets: readonly ConfigWorkspaceTarget[];
  devices: readonly SettingsScopeDeviceState[];
  selectedScopeKey: string;
}) {
  const context = { ...params, remoteUserAvailable: true };
  return {
    groups: buildSettingsScopeGroups(context),
    resolution: resolveSettingsScope(params.selectedScopeKey, context),
  };
}
