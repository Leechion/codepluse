# 供应商设置整合进「智能体」页

## Context

用户要求：模型供应商与 API 供应商的内容整合，并**并入「智能体」设置页**（不再有独立的「供应商」导航项）。两套后端数据互不通用（模型供应商=model_provider_* CRUD，已补齐；API 供应商=codepluse_provider_* 执行端切换+CC Switch），所以整合方式为：智能体页底部新增「供应商」折叠区，纵向收纳两块内容，各自功能完整保留。

智能体详情面板本身已消费模型供应商数据（AcpAgentSettings L6398-6403 按 agent_type 过滤用于模型下拉），因此该数据源在智能体页是活跃的。

## 改动

1. **[agents/page.tsx](file:///e:/code/codepluse/codepluse-ui/src/app/settings/agents/page.tsx)**：`<AcpAgentSettings />` 之下新增「供应商」折叠区（`<details>`/Collapsible），内含：
   - **模型供应商**：模型供应商 CRUD（`list/create/update/delete_model_provider`，按智能体类型过滤 + 添加/编辑/删除）——新建 `components/settings/model-provider-manager.tsx` 承载（复用我已实现的 Go 后端）。
   - **执行端供应商**：原 [ApiProvidersSection](file:///e:/code/codepluse/codepluse-ui/src/components/settings/api-providers-section.tsx) 原样保留（执行端切换 + CC Switch）。
2. **[settings-shell.tsx](file:///e:/code/codepluse/codepluse-ui/src/components/settings/settings-shell.tsx)**：删除「供应商」导航项（labelKey providers）；保留 i18n key（页签/标题还在用则保留，否则一并清理）。
3. **路由**：`/settings/model-providers` 与 `/settings/api-providers` 改为重定向到 `/settings/agents`（旧深链接与快捷菜单入口不断链）。
4. **删除** providers-settings.tsx（双页签容器不再需要）。

## 验证

1. `/settings/agents`：智能体列表/详情功能回归正常；页面底部「供应商」折叠区可展开。
2. 折叠区内：模型供应商 CRUD 正常（此前已验证后端回路）；执行端供应商切换/CC Switch 正常。
3. 设置导航无「供应商」项；访问旧路由跳转到智能体页。
4. console 无报错；`pnpm --dir codepluse-ui build` 通过。
