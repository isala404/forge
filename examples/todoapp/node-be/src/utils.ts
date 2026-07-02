import { type Credentials, HttpError, type PublicUser, type UserRecord } from "./types.ts";

export const AUDIT_QUEUE = "todo-audit";
export const SESSION_IDLE_SECS = 30 * 60;
export const SESSION_ABSOLUTE_SECS = 7 * 24 * 60 * 60;

export function envOr(key: string, fallback: string): string {
  return process.env[key] || fallback;
}

export function validateCredentials(input: Credentials): { email: string; password: string } {
  const email = String(input.email || "").trim().toLowerCase();
  const password = String(input.password || "").trim();
  if (!email.includes("@") || email.length > 254) throw new HttpError(400, "enter a valid email");
  if (password.length < 8) throw new HttpError(400, "password must be at least 8 characters");
  return { email, password };
}

export function validateTitle(raw: string): string {
  const title = String(raw || "").trim();
  if (title.length === 0 || title.length > 160) {
    throw new HttpError(400, "title must be 1 to 160 characters");
  }
  return title;
}

export function bearerToken(rawHeader: string | undefined): string {
  const raw = rawHeader || "";
  const token = raw.replace(/^bearer\s+/i, "").trim();
  if (!token) throw new HttpError(401, "authentication required");
  return token;
}

export function publicUser(user: UserRecord): PublicUser {
  return { id: user.id, email: user.email };
}

export function userEmailKey(email: string): string {
  return `todo:user:email:${email}`;
}

export function userIdKey(id: string): string {
  return `todo:user:id:${id}`;
}

export function todosKey(userId: string): string {
  return `todo:todos:${userId}`;
}
