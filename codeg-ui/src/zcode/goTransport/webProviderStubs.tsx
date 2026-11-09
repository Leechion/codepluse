// Web/Go 部署的最小 Platform/Services Provider 桩。
//
// vendor v4 组件（ConversationComposer → useComposerAttachments 等）要求
// PlatformProvider + ServiceProvider 上下文；桌面宿主由 zcode/Root 注入真实
// 桥接，而 codeg-ui 的 /workspace 页面没有桌面壳。这里提供 web 语义的安全桩：
// - 附件选择走 HTML <input> 回退（canSelectFilePath=false），native picker 不可达；
// - 附件跨机暂存服务只在远端 workspace 流程触发（web 无 remoteSessionId），
//   桩实现全部 no-op，防止删除上传中附件时命中未定义方法；
// - 其余服务访问回退到「断连代理」——复用 vendor useWorkspaceServices 的
//   disconnected 语义：命令明确拒绝（带 code）、事件统一空订阅，消费方按
//   断连路径优雅降级而不是 TypeError 崩溃。
//
// 导入注意：IServiceAccessor / IPlatformService 用直接文件相对导入而非
// @zcode/services|@zcode/shared 桶（fresh vendor 树的桶是裁剪版，且服务桶
// 尚未重建）；@zcode/rpc 的 Event 以本地结构类型代替。
import type { ReactNode } from "react";
import type { IPlatformService } from "../shared/platform";
import type { IServiceAccessor } from "../services/accessor";
import type {
  IPromptAttachmentTransferService,
  PromptAttachmentTransferProgress,
} from "../services/prompt-attachment-transfer/promptAttachmentTransfer";
import { PlatformProvider } from "../hooks/usePlatform";
import { ServiceProvider } from "../hooks/useServices";
import { getDisconnectedRemoteServices } from "../hooks/useWorkspaceServices";
import { TabStoreProvider } from "../store/TabStoreProvider";
import { TooltipProvider } from "../components/ui/tooltip";
import {
  CodingPlanUpgradeDialogContext,
  type CodingPlanUpgradeDialogContextValue,
} from "../settings/CodingPlanUpgradeDialogProvider";

const noop = () => {};

/** vendor @zcode/rpc 的 Event 结构（订阅函数 → disposable）。 */
type RpcEvent<T> = (listener: (value: T) => void) => { dispose(): void };

/**
 * 套餐升级入口的降级实现：真实 Provider 依赖 StoreProvider + 完整权益查询链，
 * web 独立页不挂载；工具栏按钮点击时 no-op（web 部署无内购面）。
 */
const codingPlanDialogStub: CodingPlanUpgradeDialogContextValue = {
  inventory: { entryPlanList: "", status: "error", retry: noop },
  openCodingPlanUpgrade: () => false,
};

function CodingPlanUpgradeDialogStub({ children }: { children: ReactNode }) {
  return (
    <CodingPlanUpgradeDialogContext.Provider value={codingPlanDialogStub}>
      {children}
    </CodingPlanUpgradeDialogContext.Provider>
  );
}

const webAttachmentTransfer: IPromptAttachmentTransferService = {
  async stage(params) {
    // web 无跨机暂存：沿用 localPath 零拷贝语义（ref 原样返回，staged=false）。
    return {
      operationId: params.operationId,
      ref: params.localPath,
      bytes: params.sizeBytes ?? 0,
      staged: false,
    };
  },
  async adopt() {},
  async cancel() {},
  async cleanup() {},
  onDynamicProgress(): RpcEvent<PromptAttachmentTransferProgress> {
    // 无动态进度可报；空订阅。
    return () => ({ dispose: noop });
  },
};

/** 断连服务代理：vendor 的 disconnected 语义（命令拒绝、事件空订阅）。 */
const disconnected = getDisconnectedRemoteServices();

const webPlatform = {
  canOpenLocalFiles: false,
  canSelectFilePath: false,
  async selectDirectory() {
    return null;
  },
  async selectFile() {
    return null;
  },
  onRemoteConnectionLog() {
    return noop;
  },
  onRemoteSessionClosed() {
    return noop;
  },
  async activateOrSetWorkspace() {
    return { activated: false };
  },
  async connectRemote() {
    return { success: false, error: "web" };
  },
  async stopWebRemoteControl() {},
  async getWebRemoteControlStatus() {
    return { status: "disabled" } as never;
  },
  async disposeRemoteSession() {},
  async isDockerAvailable() {
    return false;
  },
  async listWSLDistros() {
    return [];
  },
  async listDockerContainers() {
    return [];
  },
  async listSSHConfigAliases() {
    return [];
  },
  openExternal(url: string) {
    if (typeof window !== "undefined") {
      window.open(url, "_blank", "noopener,noreferrer");
    }
  },
  async openFeedback() {},
} as unknown as IPlatformService;

const webServices: IServiceAccessor = new Proxy(
  {
    promptAttachmentTransferService: webAttachmentTransfer,
  } as unknown as IServiceAccessor,
  {
    get(target, property, receiver) {
      if (Reflect.has(target, property)) {
        return Reflect.get(target, property, receiver);
      }
      return Reflect.get(disconnected, property);
    },
  },
);

/** vendor 组件所需的上下文壳；/workspace 页面用它包裹对话树。 */
export function WebProviderStubs({ children }: { children: ReactNode }) {
  return (
    <TooltipProvider>
      <ServiceProvider services={webServices} composerClientNamespace="codeg-web">
        <PlatformProvider platform={webPlatform}>
          {/* composer 工具栏读取跨标签状态；独立页面用空 TabStore 即可 */}
          <TabStoreProvider>
            {/* composer 工具栏的套餐升级入口；web 模式降级为 no-op */}
            <CodingPlanUpgradeDialogStub>{children}</CodingPlanUpgradeDialogStub>
          </TabStoreProvider>
        </PlatformProvider>
      </ServiceProvider>
    </TooltipProvider>
  );
}
