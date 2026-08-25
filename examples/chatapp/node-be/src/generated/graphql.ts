import type { GraphQLResolveInfo, GraphQLScalarType, GraphQLScalarTypeConfig } from 'graphql';
import type { UserRow, ChatRow, MessageRow, ReceiptRow } from '../db.ts';
import type { GqlContext } from '../context.ts';
export type Maybe<T> = T | null;
export type InputMaybe<T> = Maybe<T>;
export type Exact<T extends { [key: string]: unknown }> = { [K in keyof T]: T[K] };
export type MakeOptional<T, K extends keyof T> = Omit<T, K> & { [SubKey in K]?: Maybe<T[SubKey]> };
export type MakeMaybe<T, K extends keyof T> = Omit<T, K> & { [SubKey in K]: Maybe<T[SubKey]> };
export type MakeEmpty<T extends { [key: string]: unknown }, K extends keyof T> = { [_ in K]?: never };
export type Incremental<T> = T | { [P in keyof T]?: P extends ' $fragmentName' | '__typename' ? T[P] : never };
export type Omit<T, K extends keyof T> = Pick<T, Exclude<keyof T, K>>;
export type RequireFields<T, K extends keyof T> = Omit<T, K> & { [P in K]-?: NonNullable<T[P]> };
/** All built-in and custom scalars, mapped to their actual values */
export type Scalars = {
  ID: { input: string; output: string; }
  String: { input: string; output: string; }
  Boolean: { input: boolean; output: boolean; }
  Int: { input: number; output: number; }
  Float: { input: number; output: number; }
  /** RFC3339 / ISO-8601 UTC timestamp, serialized as a string. */
  DateTime: { input: Date; output: Date; }
};

/** A freshly minted API key (forge auth). The secret is shown exactly once. */
export type ApiKeyPayload = {
  __typename?: 'ApiKeyPayload';
  id: Scalars['String']['output'];
  secret: Scalars['String']['output'];
};

export type Chat = {
  __typename?: 'Chat';
  /** Disappearing-message lifetime in seconds, or null when off (forge schedule). */
  disappearingSeconds?: Maybe<Scalars['Int']['output']>;
  id: Scalars['ID']['output'];
  kind: ChatKind;
  lastMessage?: Maybe<Message>;
  members: Array<User>;
  /** Group title; null for direct chats (the UI derives a name from the other member). */
  title?: Maybe<Scalars['String']['output']>;
  /** Unread count for the requesting user, tracked in kv. */
  unread: Scalars['Int']['output'];
};

export type ChatKind =
  | 'DIRECT'
  | 'GROUP';

/** An attachment stored in blob storage, exposed via a short-lived presigned download URL. */
export type Media = {
  __typename?: 'Media';
  contentType?: Maybe<Scalars['String']['output']>;
  downloadUrl: Scalars['String']['output'];
  key: Scalars['String']['output'];
};

export type Message = {
  __typename?: 'Message';
  body: Scalars['String']['output'];
  chatId: Scalars['ID']['output'];
  createdAt: Scalars['DateTime']['output'];
  id: Scalars['ID']['output'];
  media?: Maybe<Media>;
  receipts: Array<Receipt>;
  sender: User;
};

export type Mutation = {
  __typename?: 'Mutation';
  addMember: Chat;
  /** Mint a personal API key (forge auth). The secret is returned exactly once. */
  createApiKey: ApiKeyPayload;
  createChat: Chat;
  heartbeat: Scalars['Boolean']['output'];
  login: SessionPayload;
  logout: Scalars['Boolean']['output'];
  logoutAll: Scalars['Boolean']['output'];
  markRead: Scalars['Boolean']['output'];
  /** Hand the client a presigned PUT URL to upload an attachment directly to blob storage. */
  requestUpload: UploadTicket;
  sendMessage: Message;
  /** Turn disappearing messages on/off for a chat (forge schedule). */
  setDisappearing: Chat;
  /** Set the `reactions_v1` feature-flag rollout percentage (forge config). */
  setReactionsRollout: Scalars['Boolean']['output'];
  setTyping: Scalars['Boolean']['output'];
  signup: SessionPayload;
  /** Enqueue a job destined to dead-letter (forge queue DLQ demo). */
  triggerFailingJob: Scalars['Boolean']['output'];
};


export type MutationAddMemberArgs = {
  chatId: Scalars['ID']['input'];
  username: Scalars['String']['input'];
};


export type MutationCreateApiKeyArgs = {
  label: Scalars['String']['input'];
};


export type MutationCreateChatArgs = {
  kind: ChatKind;
  memberUsernames: Array<Scalars['String']['input']>;
  title?: InputMaybe<Scalars['String']['input']>;
};


export type MutationLoginArgs = {
  password: Scalars['String']['input'];
  username: Scalars['String']['input'];
};


export type MutationMarkReadArgs = {
  chatId: Scalars['ID']['input'];
  messageId: Scalars['ID']['input'];
};


export type MutationRequestUploadArgs = {
  chatId: Scalars['ID']['input'];
};


export type MutationSendMessageArgs = {
  body: Scalars['String']['input'];
  chatId: Scalars['ID']['input'];
  idempotencyKey?: InputMaybe<Scalars['String']['input']>;
  mediaKey?: InputMaybe<Scalars['String']['input']>;
};


export type MutationSetDisappearingArgs = {
  chatId: Scalars['ID']['input'];
  enabled: Scalars['Boolean']['input'];
};


export type MutationSetReactionsRolloutArgs = {
  percent: Scalars['Int']['input'];
};


export type MutationSetTypingArgs = {
  chatId: Scalars['ID']['input'];
  typing: Scalars['Boolean']['input'];
};


export type MutationSignupArgs = {
  displayName: Scalars['String']['input'];
  password: Scalars['String']['input'];
  username: Scalars['String']['input'];
};

/** Developer-tools gauges for the settings page. */
export type OpsStats = {
  __typename?: 'OpsStats';
  /** Jobs sitting in the `fail.dlq` dead-letter queue. */
  dlqCount: Scalars['Int']['output'];
  /** Users currently online, counted via a kv scan of the `online:` prefix. */
  onlineCount: Scalars['Int']['output'];
};

export type Query = {
  __typename?: 'Query';
  chat?: Maybe<Chat>;
  chats: Array<Chat>;
  /** The authenticated user, or null when unauthenticated. */
  me?: Maybe<User>;
  messages: Array<Message>;
  /** Developer-tools gauges (kv scan + DLQ depth) for the settings page. */
  opsStats: OpsStats;
  presence: Array<User>;
  /** Whether the `reactions_v1` feature flag is enabled for the current user (forge config). */
  reactionsEnabled: Scalars['Boolean']['output'];
};


export type QueryChatArgs = {
  id: Scalars['ID']['input'];
};


export type QueryMessagesArgs = {
  before?: InputMaybe<Scalars['DateTime']['input']>;
  chatId: Scalars['ID']['input'];
  limit?: Scalars['Int']['input'];
};


export type QueryPresenceArgs = {
  userIds: Array<Scalars['ID']['input']>;
};

export type Receipt = {
  __typename?: 'Receipt';
  deliveredAt?: Maybe<Scalars['DateTime']['output']>;
  messageId: Scalars['ID']['output'];
  readAt?: Maybe<Scalars['DateTime']['output']>;
  user: User;
};

/** Returned by signup/login. The token authenticates HTTP (Authorization: Bearer) and WS. */
export type SessionPayload = {
  __typename?: 'SessionPayload';
  token: Scalars['String']['output'];
  user: User;
};

export type Subscription = {
  __typename?: 'Subscription';
  /** New messages in a chat (live). */
  messageAdded: Message;
  presenceChanged: User;
  receiptChanged: Receipt;
  typing: TypingEvent;
};


export type SubscriptionMessageAddedArgs = {
  chatId: Scalars['ID']['input'];
};


export type SubscriptionPresenceChangedArgs = {
  userIds: Array<Scalars['ID']['input']>;
};


export type SubscriptionReceiptChangedArgs = {
  chatId: Scalars['ID']['input'];
};


export type SubscriptionTypingArgs = {
  chatId: Scalars['ID']['input'];
};

export type TypingEvent = {
  __typename?: 'TypingEvent';
  typing: Scalars['Boolean']['output'];
  user: User;
};

/** A presigned PUT ticket. The client uploads attachment bytes directly to blob storage. */
export type UploadTicket = {
  __typename?: 'UploadTicket';
  key: Scalars['String']['output'];
  maxBytes: Scalars['Int']['output'];
  uploadUrl: Scalars['String']['output'];
};

export type User = {
  __typename?: 'User';
  displayName: Scalars['String']['output'];
  id: Scalars['ID']['output'];
  /** Live presence, backed by a kv key with a short TTL refreshed by heartbeat. */
  online: Scalars['Boolean']['output'];
  username: Scalars['String']['output'];
};

export type WithIndex<TObject> = TObject & Record<string, any>;
export type ResolversObject<TObject> = WithIndex<TObject>;

export type ResolverTypeWrapper<T> = Promise<T> | T;


export type ResolverWithResolve<TResult, TParent, TContext, TArgs> = {
  resolve: ResolverFn<TResult, TParent, TContext, TArgs>;
};
export type Resolver<TResult, TParent = {}, TContext = {}, TArgs = {}> = ResolverFn<TResult, TParent, TContext, TArgs> | ResolverWithResolve<TResult, TParent, TContext, TArgs>;

export type ResolverFn<TResult, TParent, TContext, TArgs> = (
  parent: TParent,
  args: TArgs,
  context: TContext,
  info: GraphQLResolveInfo
) => Promise<TResult> | TResult;

export type SubscriptionSubscribeFn<TResult, TParent, TContext, TArgs> = (
  parent: TParent,
  args: TArgs,
  context: TContext,
  info: GraphQLResolveInfo
) => AsyncIterable<TResult> | Promise<AsyncIterable<TResult>>;

export type SubscriptionResolveFn<TResult, TParent, TContext, TArgs> = (
  parent: TParent,
  args: TArgs,
  context: TContext,
  info: GraphQLResolveInfo
) => TResult | Promise<TResult>;

export interface SubscriptionSubscriberObject<TResult, TKey extends string, TParent, TContext, TArgs> {
  subscribe: SubscriptionSubscribeFn<{ [key in TKey]: TResult }, TParent, TContext, TArgs>;
  resolve?: SubscriptionResolveFn<TResult, { [key in TKey]: TResult }, TContext, TArgs>;
}

export interface SubscriptionResolverObject<TResult, TParent, TContext, TArgs> {
  subscribe: SubscriptionSubscribeFn<any, TParent, TContext, TArgs>;
  resolve: SubscriptionResolveFn<TResult, any, TContext, TArgs>;
}

export type SubscriptionObject<TResult, TKey extends string, TParent, TContext, TArgs> =
  | SubscriptionSubscriberObject<TResult, TKey, TParent, TContext, TArgs>
  | SubscriptionResolverObject<TResult, TParent, TContext, TArgs>;

export type SubscriptionResolver<TResult, TKey extends string, TParent = {}, TContext = {}, TArgs = {}> =
  | ((...args: any[]) => SubscriptionObject<TResult, TKey, TParent, TContext, TArgs>)
  | SubscriptionObject<TResult, TKey, TParent, TContext, TArgs>;

export type TypeResolveFn<TTypes, TParent = {}, TContext = {}> = (
  parent: TParent,
  context: TContext,
  info: GraphQLResolveInfo
) => Maybe<TTypes> | Promise<Maybe<TTypes>>;

export type IsTypeOfResolverFn<T = {}, TContext = {}> = (obj: T, context: TContext, info: GraphQLResolveInfo) => boolean | Promise<boolean>;

export type NextResolverFn<T> = () => Promise<T>;

export type DirectiveResolverFn<TResult = {}, TParent = {}, TContext = {}, TArgs = {}> = (
  next: NextResolverFn<TResult>,
  parent: TParent,
  args: TArgs,
  context: TContext,
  info: GraphQLResolveInfo
) => TResult | Promise<TResult>;



/** Mapping between all available schema types and the resolvers types */
export type ResolversTypes = ResolversObject<{
  ApiKeyPayload: ResolverTypeWrapper<ApiKeyPayload>;
  Boolean: ResolverTypeWrapper<Scalars['Boolean']['output']>;
  Chat: ResolverTypeWrapper<ChatRow>;
  ChatKind: ChatKind;
  DateTime: ResolverTypeWrapper<Scalars['DateTime']['output']>;
  ID: ResolverTypeWrapper<Scalars['ID']['output']>;
  Int: ResolverTypeWrapper<Scalars['Int']['output']>;
  Media: ResolverTypeWrapper<Media>;
  Message: ResolverTypeWrapper<MessageRow>;
  Mutation: ResolverTypeWrapper<{}>;
  OpsStats: ResolverTypeWrapper<OpsStats>;
  Query: ResolverTypeWrapper<{}>;
  Receipt: ResolverTypeWrapper<ReceiptRow>;
  SessionPayload: ResolverTypeWrapper<Omit<SessionPayload, 'user'> & { user: ResolversTypes['User'] }>;
  String: ResolverTypeWrapper<Scalars['String']['output']>;
  Subscription: ResolverTypeWrapper<{}>;
  TypingEvent: ResolverTypeWrapper<Omit<TypingEvent, 'user'> & { user: ResolversTypes['User'] }>;
  UploadTicket: ResolverTypeWrapper<UploadTicket>;
  User: ResolverTypeWrapper<UserRow>;
}>;

/** Mapping between all available schema types and the resolvers parents */
export type ResolversParentTypes = ResolversObject<{
  ApiKeyPayload: ApiKeyPayload;
  Boolean: Scalars['Boolean']['output'];
  Chat: ChatRow;
  DateTime: Scalars['DateTime']['output'];
  ID: Scalars['ID']['output'];
  Int: Scalars['Int']['output'];
  Media: Media;
  Message: MessageRow;
  Mutation: {};
  OpsStats: OpsStats;
  Query: {};
  Receipt: ReceiptRow;
  SessionPayload: Omit<SessionPayload, 'user'> & { user: ResolversParentTypes['User'] };
  String: Scalars['String']['output'];
  Subscription: {};
  TypingEvent: Omit<TypingEvent, 'user'> & { user: ResolversParentTypes['User'] };
  UploadTicket: UploadTicket;
  User: UserRow;
}>;

export type ApiKeyPayloadResolvers<ContextType = GqlContext, ParentType extends ResolversParentTypes['ApiKeyPayload'] = ResolversParentTypes['ApiKeyPayload']> = ResolversObject<{
  id?: Resolver<ResolversTypes['String'], ParentType, ContextType>;
  secret?: Resolver<ResolversTypes['String'], ParentType, ContextType>;
  __isTypeOf?: IsTypeOfResolverFn<ParentType, ContextType>;
}>;

export type ChatResolvers<ContextType = GqlContext, ParentType extends ResolversParentTypes['Chat'] = ResolversParentTypes['Chat']> = ResolversObject<{
  disappearingSeconds?: Resolver<Maybe<ResolversTypes['Int']>, ParentType, ContextType>;
  id?: Resolver<ResolversTypes['ID'], ParentType, ContextType>;
  kind?: Resolver<ResolversTypes['ChatKind'], ParentType, ContextType>;
  lastMessage?: Resolver<Maybe<ResolversTypes['Message']>, ParentType, ContextType>;
  members?: Resolver<Array<ResolversTypes['User']>, ParentType, ContextType>;
  title?: Resolver<Maybe<ResolversTypes['String']>, ParentType, ContextType>;
  unread?: Resolver<ResolversTypes['Int'], ParentType, ContextType>;
  __isTypeOf?: IsTypeOfResolverFn<ParentType, ContextType>;
}>;

export interface DateTimeScalarConfig extends GraphQLScalarTypeConfig<ResolversTypes['DateTime'], any> {
  name: 'DateTime';
}

export type MediaResolvers<ContextType = GqlContext, ParentType extends ResolversParentTypes['Media'] = ResolversParentTypes['Media']> = ResolversObject<{
  contentType?: Resolver<Maybe<ResolversTypes['String']>, ParentType, ContextType>;
  downloadUrl?: Resolver<ResolversTypes['String'], ParentType, ContextType>;
  key?: Resolver<ResolversTypes['String'], ParentType, ContextType>;
  __isTypeOf?: IsTypeOfResolverFn<ParentType, ContextType>;
}>;

export type MessageResolvers<ContextType = GqlContext, ParentType extends ResolversParentTypes['Message'] = ResolversParentTypes['Message']> = ResolversObject<{
  body?: Resolver<ResolversTypes['String'], ParentType, ContextType>;
  chatId?: Resolver<ResolversTypes['ID'], ParentType, ContextType>;
  createdAt?: Resolver<ResolversTypes['DateTime'], ParentType, ContextType>;
  id?: Resolver<ResolversTypes['ID'], ParentType, ContextType>;
  media?: Resolver<Maybe<ResolversTypes['Media']>, ParentType, ContextType>;
  receipts?: Resolver<Array<ResolversTypes['Receipt']>, ParentType, ContextType>;
  sender?: Resolver<ResolversTypes['User'], ParentType, ContextType>;
  __isTypeOf?: IsTypeOfResolverFn<ParentType, ContextType>;
}>;

export type MutationResolvers<ContextType = GqlContext, ParentType extends ResolversParentTypes['Mutation'] = ResolversParentTypes['Mutation']> = ResolversObject<{
  addMember?: Resolver<ResolversTypes['Chat'], ParentType, ContextType, RequireFields<MutationAddMemberArgs, 'chatId' | 'username'>>;
  createApiKey?: Resolver<ResolversTypes['ApiKeyPayload'], ParentType, ContextType, RequireFields<MutationCreateApiKeyArgs, 'label'>>;
  createChat?: Resolver<ResolversTypes['Chat'], ParentType, ContextType, RequireFields<MutationCreateChatArgs, 'kind' | 'memberUsernames'>>;
  heartbeat?: Resolver<ResolversTypes['Boolean'], ParentType, ContextType>;
  login?: Resolver<ResolversTypes['SessionPayload'], ParentType, ContextType, RequireFields<MutationLoginArgs, 'password' | 'username'>>;
  logout?: Resolver<ResolversTypes['Boolean'], ParentType, ContextType>;
  logoutAll?: Resolver<ResolversTypes['Boolean'], ParentType, ContextType>;
  markRead?: Resolver<ResolversTypes['Boolean'], ParentType, ContextType, RequireFields<MutationMarkReadArgs, 'chatId' | 'messageId'>>;
  requestUpload?: Resolver<ResolversTypes['UploadTicket'], ParentType, ContextType, RequireFields<MutationRequestUploadArgs, 'chatId'>>;
  sendMessage?: Resolver<ResolversTypes['Message'], ParentType, ContextType, RequireFields<MutationSendMessageArgs, 'body' | 'chatId'>>;
  setDisappearing?: Resolver<ResolversTypes['Chat'], ParentType, ContextType, RequireFields<MutationSetDisappearingArgs, 'chatId' | 'enabled'>>;
  setReactionsRollout?: Resolver<ResolversTypes['Boolean'], ParentType, ContextType, RequireFields<MutationSetReactionsRolloutArgs, 'percent'>>;
  setTyping?: Resolver<ResolversTypes['Boolean'], ParentType, ContextType, RequireFields<MutationSetTypingArgs, 'chatId' | 'typing'>>;
  signup?: Resolver<ResolversTypes['SessionPayload'], ParentType, ContextType, RequireFields<MutationSignupArgs, 'displayName' | 'password' | 'username'>>;
  triggerFailingJob?: Resolver<ResolversTypes['Boolean'], ParentType, ContextType>;
}>;

export type OpsStatsResolvers<ContextType = GqlContext, ParentType extends ResolversParentTypes['OpsStats'] = ResolversParentTypes['OpsStats']> = ResolversObject<{
  dlqCount?: Resolver<ResolversTypes['Int'], ParentType, ContextType>;
  onlineCount?: Resolver<ResolversTypes['Int'], ParentType, ContextType>;
  __isTypeOf?: IsTypeOfResolverFn<ParentType, ContextType>;
}>;

export type QueryResolvers<ContextType = GqlContext, ParentType extends ResolversParentTypes['Query'] = ResolversParentTypes['Query']> = ResolversObject<{
  chat?: Resolver<Maybe<ResolversTypes['Chat']>, ParentType, ContextType, RequireFields<QueryChatArgs, 'id'>>;
  chats?: Resolver<Array<ResolversTypes['Chat']>, ParentType, ContextType>;
  me?: Resolver<Maybe<ResolversTypes['User']>, ParentType, ContextType>;
  messages?: Resolver<Array<ResolversTypes['Message']>, ParentType, ContextType, RequireFields<QueryMessagesArgs, 'chatId' | 'limit'>>;
  opsStats?: Resolver<ResolversTypes['OpsStats'], ParentType, ContextType>;
  presence?: Resolver<Array<ResolversTypes['User']>, ParentType, ContextType, RequireFields<QueryPresenceArgs, 'userIds'>>;
  reactionsEnabled?: Resolver<ResolversTypes['Boolean'], ParentType, ContextType>;
}>;

export type ReceiptResolvers<ContextType = GqlContext, ParentType extends ResolversParentTypes['Receipt'] = ResolversParentTypes['Receipt']> = ResolversObject<{
  deliveredAt?: Resolver<Maybe<ResolversTypes['DateTime']>, ParentType, ContextType>;
  messageId?: Resolver<ResolversTypes['ID'], ParentType, ContextType>;
  readAt?: Resolver<Maybe<ResolversTypes['DateTime']>, ParentType, ContextType>;
  user?: Resolver<ResolversTypes['User'], ParentType, ContextType>;
  __isTypeOf?: IsTypeOfResolverFn<ParentType, ContextType>;
}>;

export type SessionPayloadResolvers<ContextType = GqlContext, ParentType extends ResolversParentTypes['SessionPayload'] = ResolversParentTypes['SessionPayload']> = ResolversObject<{
  token?: Resolver<ResolversTypes['String'], ParentType, ContextType>;
  user?: Resolver<ResolversTypes['User'], ParentType, ContextType>;
  __isTypeOf?: IsTypeOfResolverFn<ParentType, ContextType>;
}>;

export type SubscriptionResolvers<ContextType = GqlContext, ParentType extends ResolversParentTypes['Subscription'] = ResolversParentTypes['Subscription']> = ResolversObject<{
  messageAdded?: SubscriptionResolver<ResolversTypes['Message'], "messageAdded", ParentType, ContextType, RequireFields<SubscriptionMessageAddedArgs, 'chatId'>>;
  presenceChanged?: SubscriptionResolver<ResolversTypes['User'], "presenceChanged", ParentType, ContextType, RequireFields<SubscriptionPresenceChangedArgs, 'userIds'>>;
  receiptChanged?: SubscriptionResolver<ResolversTypes['Receipt'], "receiptChanged", ParentType, ContextType, RequireFields<SubscriptionReceiptChangedArgs, 'chatId'>>;
  typing?: SubscriptionResolver<ResolversTypes['TypingEvent'], "typing", ParentType, ContextType, RequireFields<SubscriptionTypingArgs, 'chatId'>>;
}>;

export type TypingEventResolvers<ContextType = GqlContext, ParentType extends ResolversParentTypes['TypingEvent'] = ResolversParentTypes['TypingEvent']> = ResolversObject<{
  typing?: Resolver<ResolversTypes['Boolean'], ParentType, ContextType>;
  user?: Resolver<ResolversTypes['User'], ParentType, ContextType>;
  __isTypeOf?: IsTypeOfResolverFn<ParentType, ContextType>;
}>;

export type UploadTicketResolvers<ContextType = GqlContext, ParentType extends ResolversParentTypes['UploadTicket'] = ResolversParentTypes['UploadTicket']> = ResolversObject<{
  key?: Resolver<ResolversTypes['String'], ParentType, ContextType>;
  maxBytes?: Resolver<ResolversTypes['Int'], ParentType, ContextType>;
  uploadUrl?: Resolver<ResolversTypes['String'], ParentType, ContextType>;
  __isTypeOf?: IsTypeOfResolverFn<ParentType, ContextType>;
}>;

export type UserResolvers<ContextType = GqlContext, ParentType extends ResolversParentTypes['User'] = ResolversParentTypes['User']> = ResolversObject<{
  displayName?: Resolver<ResolversTypes['String'], ParentType, ContextType>;
  id?: Resolver<ResolversTypes['ID'], ParentType, ContextType>;
  online?: Resolver<ResolversTypes['Boolean'], ParentType, ContextType>;
  username?: Resolver<ResolversTypes['String'], ParentType, ContextType>;
  __isTypeOf?: IsTypeOfResolverFn<ParentType, ContextType>;
}>;

export type Resolvers<ContextType = GqlContext> = ResolversObject<{
  ApiKeyPayload?: ApiKeyPayloadResolvers<ContextType>;
  Chat?: ChatResolvers<ContextType>;
  DateTime?: GraphQLScalarType;
  Media?: MediaResolvers<ContextType>;
  Message?: MessageResolvers<ContextType>;
  Mutation?: MutationResolvers<ContextType>;
  OpsStats?: OpsStatsResolvers<ContextType>;
  Query?: QueryResolvers<ContextType>;
  Receipt?: ReceiptResolvers<ContextType>;
  SessionPayload?: SessionPayloadResolvers<ContextType>;
  Subscription?: SubscriptionResolvers<ContextType>;
  TypingEvent?: TypingEventResolvers<ContextType>;
  UploadTicket?: UploadTicketResolvers<ContextType>;
  User?: UserResolvers<ContextType>;
}>;

