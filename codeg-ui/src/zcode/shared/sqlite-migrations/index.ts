import type { SqliteMigrationsContract } from "./contract";
import { migrate } from "./runner";
import { restore } from "./restore";

export type {
  MigrationBackup,
  MigrationOptions,
  MigrationResult,
  SqliteMigration,
  SqliteMigrationsContract,
} from "./contract";
export {
  MigrationChecksumError,
  STORAGE_MIGRATION_COMMAND,
  storageMigrationReportSchema,
} from "./contract";
export const sqliteMigrations: SqliteMigrationsContract = { migrate, restore };

export {
  applySqliteSchemaMigration,
  ensureSqliteSchemaMigrationTable,
  SqliteSchemaMigrationChecksumError,
  SESSION_DATABASE_IDENTITY_MIGRATION,
  runSessionDatabaseIdentityMigration,
} from "./legacy";
export type { SqliteSchemaMigration } from "./legacy";
