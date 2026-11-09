import type { ConfigCatalogPort } from "./contract";
import { readConfigCatalog, projectConfigTargets } from ".";
export async function exampleWorkspaceConfigCatalog(port: ConfigCatalogPort) {
  return projectConfigTargets(await readConfigCatalog(port), []);
}
