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

export interface Todo {
  id: string;
  title: string;
  completed: boolean;
  createdAt: string;
  updatedAt: string;
}

export interface TodoCreate {
  title: string;
}

export interface TodoPatch {
  title?: string;
  completed?: boolean;
}

export class HttpError extends Error {
  status: number;

  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}
