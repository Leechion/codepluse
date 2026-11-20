// 设置浮层的模块级桥。
//
// `openSettingsWindow` 是 lib 层函数（12 个入口共用），拿不到 React context；而浮层
// 的状态属于 workspace 窗口里的 provider。这里放一个可注册的处理器：provider 挂载时
// 登记，卸载时注销；`openSettingsWindow` 先问它能不能就地打开，能就不再去开第二个
// 窗口（Go 桌面壳下开窗口 = 重新 exec 自己，会真的多出一个 WebView2 窗口）。
export type SettingsOverlayHandler = (
  section?: string | null,
  agentType?: string | null
) => void

let handler: SettingsOverlayHandler | null = null

/** 登记浮层处理器，返回注销函数（供 provider 的 cleanup 使用）。 */
export function registerSettingsOverlayHandler(
  next: SettingsOverlayHandler
): () => void {
  const previous = handler
  handler = next
  return () => {
    handler = previous
  }
}

/** 当前是否有能就地打开设置的浮层接管。 */
export function settingsOverlayIsAvailable(): boolean {
  return handler !== null
}

/**
 * 请求在当前窗口内打开设置浮层。返回 false 表示没有浮层接管（纯浏览器直接访问
 * `/settings/...`、宠物窗口等），调用方应退回原来的开窗口逻辑。
 */
export function requestSettingsOverlay(
  section?: string | null,
  agentType?: string | null
): boolean {
  if (handler === null) return false
  handler(section ?? null, agentType ?? null)
  return true
}
