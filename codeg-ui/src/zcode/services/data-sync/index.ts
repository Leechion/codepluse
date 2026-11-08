// 浏览器安全的公共入口：只导出契约。Renderer 经 @zcode/services 引到这里，不能带入 node:fs 等实现。
export type { IDataSyncCategoryService } from "./contract";
export { IDeviceSyncStateService, IUserConfigSyncService } from "./contract";
