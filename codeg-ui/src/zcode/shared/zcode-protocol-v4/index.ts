// ZCode Protocol v4 —— 数据模型草稿。
//
// 本包纪律：只放 schema 类型 + 纯函数
// （coalesce/conflation/apply），运行时（通道层缓冲、订阅注册表、调度）一律不进本包。
// 与旧 packages/shared/src/zcode-protocol 并存。
export * from "./core";
export * from "../background-bash-output";
export * from "./rows";
export * from "./toolDisplay";
export * from "./create-workflow-display";
export * from "./workflow-observation-display";
export * from "./snapshot";
export * from "./workflow-runs";
export * from "./workflow-runs-reducer";
export * from "./workflow-runs-holes";
// workflowRuns 的键级增量（diff / apply / 规范键序）；op 本身在 delta.js。
export * from "./workflow-runs-delta";
// 界留下的痕迹：被拒实例计数、条目预算、步数读法；表满时的腾位；以及给旧消费者的裁剪。
export * from "./workflow-runs-caps";
export * from "./workflow-runs-eviction";
export * from "./workflow-runs-tables";
export * from "./workflow-runs-legacy";
export * from "./workflow-artifact";
// ⚠ 与上一行只差一个 s，且两个 artifact 不同义：单数 = 引擎内部的「脚本顶层返回值」的
// 序列化；复数 = 脚本 `artifact.*` 发布给用户看的产出。见 workflow-artifacts.ts 的文件头。
export * from "./workflow-artifacts";
// 工作区 transcript（files.* / git.* / world.run 的回放）。
export * from "./workflow-workspace";
export * from "./attachment-ref";
export * from "./attachment-faults";
export * from "./delta";
export * from "./coalesce";
export * from "./profiles";
export * from "./apply";
export * from "./transport";
export * from "./input-prediction/contract";
export * from "./session-data";
export * from "./wire";
export * from "./wire-codec";
export * from "./wire-reassembly";
export * from "./wire-assembler";
export * from "./wire-fault";
export * from "./sessions-index";
export * from "./sessions-index-workflow-activity";
export * from "./workspace-config";
export * from "./command";
export * from "./workflow-run-settings-command";
export * from "./shared-context-ref";
export * from "./shared-context-import";
export * from "./input-intent";
export * from "./submission";
export * from "./fork";
export * from "./telemetry";
export * from "./controller";
export * from "./workspace-hook-review";
export * from "./cuaPermission";

export {
  executionOutputPreviewSchema,
  type ExecutionOutputPreview,
} from "../execution-output-preview";

export { bashOutputDisplaySchema } from "../bash-output-display";
export { modelSelectionSchema, type ModelSelection } from "../model-selection";

export * from "../local-ttft/contract";
