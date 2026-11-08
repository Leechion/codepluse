import type { ProjectCatalogPort } from "./contract";
import { readProjectCatalog } from ".";
export async function example(port: ProjectCatalogPort) {
  return (await readProjectCatalog(port)).projects.map((project) => project.name);
}
