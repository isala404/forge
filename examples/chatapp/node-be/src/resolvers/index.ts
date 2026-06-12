import type { Resolvers } from "../generated/graphql.ts";
import { DateTime } from "./scalars.ts";
import { User, Chat, Message, Receipt } from "./types.ts";
import { Query } from "./query.ts";
import { Mutation } from "./mutation.ts";
import { Subscription } from "./subscription.ts";

export const resolvers: Resolvers = { DateTime, User, Chat, Message, Receipt, Query, Mutation, Subscription };
