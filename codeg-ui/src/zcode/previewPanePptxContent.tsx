import { lazy, Suspense } from "react";
import type { PptxPreviewViewerLabels } from "./components/ui/pptx-preview-viewer";
import type { PptxElementReferenceSource } from "./lib/pptxElementReference";
import type { PptxReferencePreviewNavigation } from "./lib/codeViewer";

const PptxPreviewViewer = lazy(() =>
  import("./components/ui/pptx-preview-viewer").then((module) => ({
    default: module.PptxPreviewViewer,
  })),
);

export function PptxPreviewContent({
  data,
  fileName,
  labels,
  onOpenBrowserUrl,
  referenceSource,
  referenceNavigation,
  referenceNavigationReady,
}: {
  data: ArrayBuffer;
  labels: PptxPreviewViewerLabels;
  fileName?: string;
  referenceSource?: PptxElementReferenceSource;
  referenceNavigation?: PptxReferencePreviewNavigation;
  referenceNavigationReady?: boolean;
  onOpenBrowserUrl?: (url: string) => void;
}) {
  return (
    <Suspense
      fallback={<div className="p-3 text-ui-base text-foreground-subtle">{labels.loading}</div>}
    >
      <PptxPreviewViewer
        data={data}
        fileName={fileName}
        labels={labels}
        onOpenBrowserUrl={onOpenBrowserUrl}
        {...(referenceSource ? { referenceSource } : {})}
        {...(referenceNavigation ? { referenceNavigation } : {})}
        referenceNavigationReady={referenceNavigationReady}
        className="h-full"
      />
    </Suspense>
  );
}
