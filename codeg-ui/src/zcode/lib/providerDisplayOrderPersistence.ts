import type { IProviderSettingsService, ProviderSettingsView } from "@zcode/services";
import type { ProviderOrderView } from "./modelProviderOrdering";

export async function persistProviderDisplayOrder(params: {
  state: ProviderOrderView;
  providerSettingsService: Pick<IProviderSettingsService, "reorderPersonalProviders">;
}): Promise<ProviderSettingsView> {
  return params.providerSettingsService.reorderPersonalProviders(params.state.providerIds);
}
