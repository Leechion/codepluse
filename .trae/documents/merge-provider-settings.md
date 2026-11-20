# 合并设置里的「模型供应商」与「API 供应商」

## Context

设置窗口导航（[settings-shell.tsx](file:///e:/code/codepluse/codepluse-ui/src/components/settings/settings-shell.tsx) L108-117）有两个供应商入口：

- `/settings/model-providers` → [ModelProviderSettings](file:///e:/code/codepluse/codepluse-ui/src/components/settings/model-provider-settings.tsx)：智能体模型供应商（list_model_providers 等，按 AgentType 管理，增删改对话框）。
- `/settings/api-providers` → [ApiProvidersSection](file:///e:/code/codepluse/codepluse-ui/src/components/settings/api-providers-section.tsx)：执行端供应商（codepluse_provider_*，claude/dsh 的 Anthropic 兼容端点与激活切换）。

两者都是「配置供应商端点」，用户要求合并为一个。**两套数据互不通用，不能丢功能**——合并方案：保留 `/settings/model-providers` 路由为合并页（内部 Tabs 双页签「模型供应商 / API 供应商」），`/settings/api-providers` 路由渲染同一合并页并默认落在 API 页签（旧链接与快捷菜单入口无缝兼容）。导航只留一个「供应商」入口。

## 改动

1. **新增 [providers-settings.tsx](file:///e:/code/codepluse/codepluse-ui/src/components/settings/providers-settings.tsx)**（client 组件）：props `{ initialTab?: "model" | "api" }`，用 shadcn Tabs（[ui/tabs.tsx](file:///e:/code/codepluse/codepluse-ui/src/components/ui/tabs.tsx) 已存在）；页签文案复用 `useTranslations("SettingsShell.nav")` 的 `model_providers` / `api_providers`（10 语言已有翻译，无需新增页签文案）；内容分别渲染 `<ModelProviderSettings />` / `<ApiProvidersSection />`。
2. **[/settings/model-providers/page.tsx](file:///e:/code/codepluse/codepluse-ui/src/app/settings/model-providers/page.tsx)**：改渲染 `<ProvidersSettings initialTab="model" />`。
3. **[/settings/api-providers/page.tsx](file:///e:/code/codepluse/codepluse-ui/src/app/settings/api-providers/page.tsx)**：改渲染 `<ProvidersSettings initialTab="api" />`（quick-actions 的 `openSettingsWindow("api-providers")` 无需改动，自动落到 API 页签）。
4. **[settings-shell.tsx](file:///e:/code/codepluse/codepluse-ui/src/components/settings/settings-shell.tsx)**：删除 `api_providers` 导航项（L113-117）；`model-providers` 项 labelKey 改为 `"providers"`（L110）。
5. **i18n（10 个语言文件）**：`SettingsShell.nav` 下新增 `"providers"`（zh-CN「供应商」、en「Providers」、其余语言给对应译名，不确定的用英文兜底）。

不动 `zcode/SettingsPage.tsx`（另一套未挂载的 ZCode 设置体系，不影响用户所见）。

## 验证

```powershell
pnpm --dir codepluse-ui build
```

浏览器：
1. `/settings/model-providers`：合并页显示，默认「模型供应商」页签，列表/增删改正常。
2. 切到「API 供应商」页签：codepluse 供应商列表与激活切换正常。
3. 直接访问 `/settings/api-providers`：落在 API 页签。
4. 设置导航只有「供应商」一项；底部状态栏快捷菜单 → 设置 仍可用。
5. console 无报错；回归：其他设置页正常。
