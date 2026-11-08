import { ServiceChannels, type MarketingTouchSnapshot } from "@zcode/shared";
import { createServiceDescriptor } from "../descriptors";

export interface IMarketingTouchService {
  query(input: { locale: "zh-CN" | "en-US" }): Promise<MarketingTouchSnapshot & { scope: string }>;
  report(input: {
    locale: "zh-CN" | "en-US";
    scope: string;
    campaignId: string;
    actionType: "confirm" | "cancel";
  }): Promise<void>;
}
export const IMarketingTouchService = createServiceDescriptor<IMarketingTouchService>(
  ServiceChannels.MarketingTouch,
);
