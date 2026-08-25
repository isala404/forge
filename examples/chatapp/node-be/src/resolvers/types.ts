import type {
  ChatResolvers,
  MessageResolvers,
  ReceiptResolvers,
  UserResolvers,
} from "../generated/graphql.ts";
import { mapForge } from "../errors.ts";
import { loadUser } from "./helpers.ts";

export const User: UserResolvers = {
  id: (u) => u.id,
  username: (u) => u.username,
  displayName: (u) => u.display_name,
  online: (u, _a, ctx) => ctx.loaders.online.load(u.id),
};

export const Chat: ChatResolvers = {
  id: (c) => c.id,
  kind: (c) => (c.kind === "group" ? "GROUP" : "DIRECT"),
  title: (c) => c.title ?? null,
  members: (c, _a, ctx) => ctx.loaders.membersByChatId.load(c.id),
  lastMessage: (c, _a, ctx) => ctx.loaders.lastMessageByChatId.load(c.id),
  unread: (c, _a, ctx) => ctx.loaders.unread.load(c.id),
  disappearingSeconds: (c) => c.disappearing_seconds ?? null,
};

export const Message: MessageResolvers = {
  id: (m) => m.id,
  body: (m) => m.body,
  createdAt: (m) => m.created_at,
  chatId: (m) => m.chat_id,
  sender: (m, _a, ctx) => loadUser(ctx, m.sender_id),
  media: async (m, _a, ctx) => {
    if (!m.media_key) return null;
    let downloadUrl: string;
    try {
      downloadUrl = (await ctx.app.forge.blobPresignDownload(m.media_key, 3600)).url;
    } catch (e) {
      throw mapForge(e);
    }
    return { key: m.media_key, downloadUrl, contentType: m.content_type ?? null };
  },
  receipts: (m, _a, ctx) => ctx.loaders.receiptsByMessageId.load(m.id),
};

export const Receipt: ReceiptResolvers = {
  messageId: (r) => r.message_id,
  user: (r, _a, ctx) => loadUser(ctx, r.user_id),
  deliveredAt: (r) => r.delivered_at ?? null,
  readAt: (r) => r.read_at ?? null,
};
