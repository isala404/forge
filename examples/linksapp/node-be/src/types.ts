export interface UserRecord {
  id: string;
  email: string;
  password_hash: string;
}

export interface PublicUser {
  id: string;
  email: string;
}

export interface Credentials {
  email: string;
  password: string;
}

// Stored at link:slug:{slug}
export interface LinkRecord {
  slug: string;
  url: string;
  ownerId: string;
  createdAt: string;
  expiresAt: string | null;
}

// Stored in the link:owner:{userId} array
export interface OwnedLink {
  slug: string;
  url: string;
  createdAt: string;
  expiresAt: string | null;
}

// The public Link shape returned by the API
export interface Link {
  slug: string;
  url: string;
  createdAt: string;
  expiresAt: string | null;
  clicks: number;
}

export interface LinkCreate {
  url: string;
  slug?: string;
  ttlSeconds?: number;
}

export class HttpError extends Error {
  status: number;

  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}
