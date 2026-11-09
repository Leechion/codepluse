import { useCallback } from "react";
import { toast } from "../../components/ui/toast";
import { useZCodeIntl } from "../../i18n/IntlProvider";
import type { CodeViewerSource } from "../../lib/codeViewer";
import type { PptxElementReference } from "../../lib/pptxElementReference";
import { createPptxElementReferencePreviewSource } from "../../lib/pptxElementReferencePreview";

export function useOpenPptxElementReference(options: {
  workspacePath: string;
  workspaceIdentity?: string;
  remoteSessionId?: string;
  onOpenCodeViewer?: (source: CodeViewerSource) => void;
}) {
  const { workspacePath, workspaceIdentity, remoteSessionId, onOpenCodeViewer } = options;
  const { intl } = useZCodeIntl();

  return useCallback(
    (reference: PptxElementReference) => {
      if (!onOpenCodeViewer) {
        return;
      }
      const source = createPptxElementReferencePreviewSource(reference, {
        workspacePath,
        workspaceIdentity,
        remoteSessionId,
      });
      if (!source) {
        toast(
          intl.formatMessage({
            id: "chat.pptxElements.previewScopeUnavailable",
          }),
        );
        return;
      }
      onOpenCodeViewer(source);
    },
    [intl, onOpenCodeViewer, remoteSessionId, workspaceIdentity, workspacePath],
  );
}
