import { GraphQLError } from "graphql";

export type ForgeCode =
  | "UNAUTHENTICATED"
  | "FORBIDDEN"
  | "INVALID"
  | "LIMIT"
  | "NOT_FOUND"
  | "PRECONDITION"
  | "UNAVAILABLE"
  | "CONFIG"
  | "BACKEND";

const FORGE_CODES: ReadonlySet<string> = new Set<ForgeCode>([
  "UNAUTHENTICATED",
  "INVALID",
  "LIMIT",
  "NOT_FOUND",
  "PRECONDITION",
  "UNAVAILABLE",
  "CONFIG",
  "BACKEND",
]);

export function err(code: ForgeCode, message: string): GraphQLError {
  return new GraphQLError(message, { extensions: { code } });
}

function errMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

// The forgelib binding prefixes every error with "<CODE>: message"; recover the
// code from that prefix. An unrecognized prefix maps to BACKEND.
export function forgeErrorCode(e: unknown): ForgeCode {
  const msg = errMessage(e);
  const sep = msg.indexOf(": ");
  if (sep > 0) {
    const head = msg.slice(0, sep);
    if (FORGE_CODES.has(head)) return head as ForgeCode;
  }
  return "BACKEND";
}

export function mapForge(e: unknown): GraphQLError {
  return err(forgeErrorCode(e), errMessage(e));
}

export function mapDb(e: unknown): GraphQLError {
  return err("BACKEND", errMessage(e));
}
