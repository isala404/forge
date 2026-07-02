import pg from "pg";

function databaseUrl(adminUrl: string, database: string): string {
  const url = new URL(adminUrl);
  url.pathname = `/${database}`;
  return url.toString();
}

const ADMIN_URL =
  process.env.CHATAPP_TEST_ADMIN_URL ??
  process.env.TEST_DATABASE_URL ??
  "postgres://postgres:forge@127.0.0.1:5432/postgres";

export const TEST_DB = process.env.CHATAPP_TEST_DB ?? "chatapp_node_test";
export const TEST_DB_URL =
  process.env.CHATAPP_TEST_DATABASE_URL ?? databaseUrl(ADMIN_URL, TEST_DB);

// Drop + recreate a dedicated test database so every run starts clean (and Forge's
// migrations re-apply against a fresh schema).
export async function setup(): Promise<void> {
  const admin = new pg.Client({ connectionString: ADMIN_URL });
  await admin.connect();
  await admin.query(
    `SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = $1 AND pid <> pg_backend_pid()`,
    [TEST_DB],
  );
  await admin.query(`DROP DATABASE IF EXISTS ${TEST_DB}`);
  await admin.query(`CREATE DATABASE ${TEST_DB}`);
  await admin.end();
}
