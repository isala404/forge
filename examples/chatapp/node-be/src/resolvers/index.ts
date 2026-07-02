import type { Resolvers } from "../generated/graphql.ts";
import { DateTime } from "./scalars.ts";
import { User, Chat, Message, Receipt } from "./types.ts";
import { query as authQuery, mutation as authMutation } from "./auth.ts";
import { query as chatQuery, mutation as chatMutation } from "./chat.ts";
import { query as messageQuery, mutation as messageMutation, subscription as messageSubscription } from "./message.ts";
import { query as presenceQuery, mutation as presenceMutation, subscription as presenceSubscription } from "./presence.ts";
import { mutation as receiptMutation, subscription as receiptSubscription } from "./receipt.ts";
import { query as opsQuery, mutation as opsMutation } from "./ops.ts";

export const resolvers: Resolvers = {
  DateTime,
  User,
  Chat,
  Message,
  Receipt,
  Query: { ...authQuery, ...chatQuery, ...messageQuery, ...presenceQuery, ...opsQuery },
  Mutation: { ...authMutation, ...chatMutation, ...messageMutation, ...presenceMutation, ...receiptMutation, ...opsMutation },
  Subscription: { ...messageSubscription, ...presenceSubscription, ...receiptSubscription },
};
