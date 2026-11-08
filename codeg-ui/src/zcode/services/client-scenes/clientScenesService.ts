import type { ApiClient } from "@zcode/shared";
import { readApiJson } from "../providers/api/apiJson";
import { ZCODE_CLIENT_SCENES_URL } from "../providers/api/apiEndpoints";
import type { ClientScenesResponse, IClientScenesService } from "./clientScenes";

export function createClientScenesService(dependencies: {
  apiClient: ApiClient;
}): IClientScenesService {
  return {
    list: () =>
      readApiJson<ClientScenesResponse>(dependencies.apiClient, ZCODE_CLIENT_SCENES_URL, {
        method: "GET",
      }),
  };
}
