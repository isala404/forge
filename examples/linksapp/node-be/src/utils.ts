import { HttpError, type Credentials, type PublicUser, type UserRecord } from "./types.ts";

export const CLICKS_QUEUE = "clicks";
export const EXPIRE_QUEUE = "link-expire";
export const SESSION_IDLE_SECS = 30 * 60;
export const SESSION_ABSOLUTE_SECS = 7 * 24 * 60 * 60;
export const DEFAULT_MAX_LINKS = 100;

export const SLUG_RE = /^[A-Za-z0-9_-]{3,32}$/;
export const RESERVED_SLUGS = new Set(["api", "healthz", "favicon.ico"]);

export function clickTopic(slug: string): string {
  return `clicks:${slug}`;
}

export function envOr(key: string, fallback: string): string {
  return process.env[key] || fallback;
}

// KV key helpers
export function userEmailKey(email: string): string {
  return `link:user:email:${email}`;
}

export function userIdKey(id: string): string {
  return `link:user:id:${id}`;
}

export function slugKey(slug: string): string {
  return `link:slug:${slug}`;
}

export function ownerKey(userId: string): string {
  return `link:owner:${userId}`;
}

export function clicksKey(slug: string): string {
  return `clicks:${slug}`;
}

export function qrKey(slug: string): string {
  return `qr:${slug}`;
}

export function publicUser(user: UserRecord): PublicUser {
  return { id: user.id, email: user.email };
}

export function validateCredentials(input: Credentials): { email: string; password: string } {
  const email = String(input.email ?? "").trim().toLowerCase();
  const password = String(input.password ?? "").trim();
  if (!email.includes("@") || email.length > 254) throw new HttpError(400, "enter a valid email");
  if (password.length < 8) throw new HttpError(400, "password must be at least 8 characters");
  return { email, password };
}

export function validateUrl(raw: unknown): string {
  const url = String(raw ?? "").trim();
  if (
    (!url.startsWith("http://") && !url.startsWith("https://")) ||
    url.length > 2048
  ) {
    throw new HttpError(400, "enter a valid http(s) url");
  }
  return url;
}

export function validateSlug(raw: string): string {
  const slug = raw.trim();
  if (!SLUG_RE.test(slug) || RESERVED_SLUGS.has(slug)) {
    throw new HttpError(400, "invalid slug");
  }
  return slug;
}

export function bearerToken(rawHeader: string | undefined): string {
  const raw = rawHeader || "";
  const token = raw.replace(/^bearer\s+/i, "").trim();
  if (!token) throw new HttpError(401, "authentication required");
  return token;
}

const SLUG_CHARS = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

export function randomSlug(): string {
  let slug = "";
  for (let i = 0; i < 7; i++) {
    slug += SLUG_CHARS[Math.floor(Math.random() * SLUG_CHARS.length)];
  }
  return slug;
}
