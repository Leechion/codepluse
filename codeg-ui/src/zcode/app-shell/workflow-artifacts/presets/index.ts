/**
 * 预置产物渲染器的公开面。
 *
 * 四个渲染器 + 两个纯函数层：
 * - `parseArtifactPresetSpec(kind, spec)` 把 wire 上的 `unknown` spec 判成「可渲染 / 不可渲染」；
 * - `applyArtifactItems(kind, spec, items)` 把 `report(item, id)` 的条目流折成视图模型；
 * - `<ArtifactChart|Table|Metrics|Board>` 渲染它，`compact` 是 run 侧板卡片里的小尺寸形态。
 *
 * **这个 barrel 的静态依赖里没有 recharts**：`ArtifactChart` 是一层 `lazy()` 封装
 * （见 `ArtifactChart.tsx`），所以 import 本模块不会把图表库拖进首屏。
 */

export { type ArtifactItem } from "./apply";
export {
  parseArtifactPresetSpec,
  type ArtifactPresetKind,
  type BoardSpec,
  type ChartSpec,
  type MetricsSpec,
  type TableSpec,
} from "./spec";

export { type PresetLabels } from "./parts";
export { ArtifactChart } from "./ArtifactChart";
export { ArtifactTable } from "./ArtifactTable";
export { ArtifactMetrics } from "./ArtifactMetrics";
export { ArtifactBoard } from "./ArtifactBoard";
