import type { BundledLanguage, BundledTheme } from "shiki";
import type { Ref, UIEventHandler } from "react";
import { cn } from "./components/lib/utils";
import { MermaidBlock } from "./components/ai-elements/mermaid-block";
import { CodeViewer } from "./components/ui/code-viewer";
import type { CodeCommentLabels } from "./components/ui/code-viewer";
import type { CodePreviewSettings } from "./store";
import type { CodeCommentPreview, CodeCommentRange } from "./lib/codeCommentContext";
import { isMermaidLanguage } from "./lib/mermaidLanguage";
import type { Theme } from "./useTheme";

interface CodeContentProps {
  code: string;
  language: BundledLanguage;
  codePreviewSettings: CodePreviewSettings;
  codeTheme: BundledTheme;
  /** 应用主题（store 耦合剥离）：透传给 Mermaid 预览，缺省按 "system" 兜底。 */
  theme?: Theme;
  wrapLongLines: boolean;
  firstLineNumber?: number;
  comments?: readonly CodeCommentPreview[];
  topComment?: CodeCommentPreview | null;
  topCommentShowRange?: boolean;
  topCommentNotice?: string;
  focusedRange?: CodeCommentRange | null;
  focusRequestId?: string;
  enableLineSelection?: boolean;
  enableGutterUtility?: boolean;
  labels?: Partial<CodeCommentLabels>;
  onSubmitCodeComment?: (params: {
    range: CodeCommentRange;
    selectedText: string;
    comment: string;
  }) => void;
  onDeleteCodeComment?: (commentId: string) => void;
  onScroll?: UIEventHandler<HTMLDivElement>;
  scrollContainerRef?: Ref<HTMLDivElement>;
  className?: string;
}

export function CodeContent({
  code,
  language,
  codePreviewSettings,
  codeTheme,
  theme,
  wrapLongLines,
  firstLineNumber,
  comments,
  topComment,
  topCommentShowRange,
  topCommentNotice,
  focusedRange,
  focusRequestId,
  enableLineSelection = false,
  enableGutterUtility = false,
  labels,
  onSubmitCodeComment,
  onDeleteCodeComment,
  onScroll,
  scrollContainerRef,
  className,
}: CodeContentProps) {
  if (isMermaidLanguage(language)) {
    return (
      <div
        ref={scrollContainerRef}
        className={cn("h-full w-full overflow-auto bg-background p-4", className)}
        onScroll={onScroll}
      >
        <MermaidBlock
          code={code}
          theme={theme}
          className="min-h-full rounded-xl border border-border"
        />
      </div>
    );
  }

  return (
    <CodeViewer
      code={code}
      language={language}
      showLineNumbers={codePreviewSettings.showLineNumbers}
      theme={codeTheme}
      wrapLongLines={wrapLongLines}
      fontSizePx={codePreviewSettings.fontSizePx}
      firstLineNumber={firstLineNumber}
      comments={comments}
      topComment={topComment}
      topCommentShowRange={topCommentShowRange}
      topCommentNotice={topCommentNotice}
      focusedRange={focusedRange}
      focusRequestId={focusRequestId}
      enableLineSelection={enableLineSelection}
      enableGutterUtility={enableGutterUtility}
      labels={labels}
      onSubmitCodeComment={onSubmitCodeComment}
      onDeleteCodeComment={onDeleteCodeComment}
      onScroll={onScroll}
      scrollContainerRef={scrollContainerRef}
      className={className}
    />
  );
}
