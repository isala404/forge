import * as db from "../db.ts";
import type { ChatRow, UserRow } from "../db.ts";
import { err, mapDb } from "../errors.ts";
import type { MutationResolvers, QueryResolvers } from "../generated/graphql.ts";
import { parseId, requireAuth, requireMember } from "./helpers.ts";

export const query: Required<Pick<QueryResolvers, "chats" | "chat">> = {
  chats: async (_r, _a, ctx) => {
    const user = requireAuth(ctx);
    try {
      return await db.chatsForUser(ctx.app.pool, user.id);
    } catch (e) {
      throw mapDb(e);
    }
  },

  chat: async (_r, { id }, ctx) => {
    const user = requireAuth(ctx);
    const chatId = parseId(id);
    await requireMember(ctx.app, chatId, user.id);
    try {
      return await db.chatById(ctx.app.pool, chatId);
    } catch (e) {
      throw mapDb(e);
    }
  },
};

export const mutation: Required<Pick<MutationResolvers, "createChat" | "addMember">> = {
  createChat: async (_r, { kind, title, memberUsernames }, ctx) => {
    const app = ctx.app;
    const user = requireAuth(ctx);
    const ids = [user.id];
    for (const uname of memberUsernames) {
      let u: UserRow | null;
      try {
        u = await db.userByUsername(app.pool, uname);
      } catch (e) {
        throw mapDb(e);
      }
      if (!u) throw err("NOT_FOUND", `no such user: ${uname}`);
      if (!ids.includes(u.id)) ids.push(u.id);
    }
    const kindStr = kind === "DIRECT" ? "direct" : "group";
    if (kind === "DIRECT" && ids.length !== 2) {
      throw err("INVALID", "a direct chat needs exactly one other member");
    }
    let chatId: string;
    try {
      chatId = await db.createChat(app.pool, kindStr, title ?? null, user.id, ids);
    } catch (e) {
      throw mapDb(e);
    }
    let row: ChatRow | null;
    try {
      row = await db.chatById(app.pool, chatId);
    } catch (e) {
      throw mapDb(e);
    }
    if (!row) throw err("BACKEND", "chat vanished after create");
    return row;
  },

  addMember: async (_r, { chatId, username }, ctx) => {
    const app = ctx.app;
    const user = requireAuth(ctx);
    const id = parseId(chatId);
    await requireMember(app, id, user.id);
    let u: UserRow | null;
    try {
      u = await db.userByUsername(app.pool, username);
    } catch (e) {
      throw mapDb(e);
    }
    if (!u) throw err("NOT_FOUND", `no such user: ${username}`);
    try {
      await db.addMember(app.pool, id, u.id);
    } catch (e) {
      throw mapDb(e);
    }
    let row: ChatRow | null;
    try {
      row = await db.chatById(app.pool, id);
    } catch (e) {
      throw mapDb(e);
    }
    if (!row) throw err("NOT_FOUND", "chat not found");
    return row;
  },
};
