import type { StartupStorageContract, AgentStorageMigrationInput } from "./contract";
export async function exampleStartupStorage(
  port: StartupStorageContract,
  input: AgentStorageMigrationInput,
) {
  const migration = await port.migrateAgent(input);
  return migration.backupSkipped;
}
