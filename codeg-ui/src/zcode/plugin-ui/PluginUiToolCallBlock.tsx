import type { ToolCallBlockRenderContext } from "../ToolCallBlocks/fileSummaryTypes";
import { McpToolCallBlock } from "../ToolCallBlocks/renderers/mcp";
import { usePluginUiToolCallModel } from "./adapters/usePluginUiToolCallModel";
import { PluginUiToolCallView } from "./components/PluginUiToolCallBlock";
import { readPluginUiPresentation } from "./domain/readPluginUiPresentation";

/** 宿主工具行渲染插槽，普通 MCP 卡片作为模块外的展示依赖注入。 */
export function PluginUiToolCallBlock(context: ToolCallBlockRenderContext) {
  const model = usePluginUiToolCallModel(context);
  return <PluginUiToolCallView {...model} fallback={<McpToolCallBlock {...context} />} />;
}
export function shouldRenderPluginUi(
  context: Pick<ToolCallBlockRenderContext, "toolCallNode" | "pluginUi">,
): boolean {
  return Boolean(
    context.pluginUi?.scope.sessionId &&
    readPluginUiPresentation(context.toolCallNode.toolCall.raw),
  );
}
