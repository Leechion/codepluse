import { Suspense, type ReactNode } from "react";
import { ScopedErrorBoundary } from "../../ErrorBoundary";
import { useZCodeIntl } from "../../i18n/IntlProvider";
import { UsageEmptyState } from "./usageStatsUiParts";

export function UsageChartLoadBoundary({
  children,
  loadingDescription,
  resetKeys,
  scope,
}: {
  children: ReactNode;
  loadingDescription: string;
  resetKeys: readonly unknown[];
  scope: string;
}) {
  const { intl } = useZCodeIntl();

  return (
    <ScopedErrorBoundary scope={scope} resetKeys={resetKeys} variant="inline">
      <Suspense
        fallback={
          <UsageEmptyState
            title={intl.formatMessage({ id: "settings.usage.loadingTitle" })}
            description={loadingDescription}
          />
        }
      >
        {children}
      </Suspense>
    </ScopedErrorBoundary>
  );
}
