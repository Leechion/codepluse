// 新源码原来散落在 legacy 根目录，CI 无法确认归属；公开入口统一收口到受管模块。
export * from "./contract";
export * from "./domain/retry";
export * from "./domain/heartbeatTiming";
export * from "./domain/webRemoteControlActions";
// 手机日志通道（mobile-log / mobile-log-pull）报文 schema 与上限。
export * from "./mobileLog";
export { WebRemoteControlV2Heartbeat } from "./adapters/heartbeat";

export {
  collectMobilePushEvents,
  type MobilePushObservation,
} from "./domain/notification";
