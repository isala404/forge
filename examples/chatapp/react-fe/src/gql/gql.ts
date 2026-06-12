/* eslint-disable */
import * as types from './graphql';
import type { TypedDocumentNode as DocumentNode } from '@graphql-typed-document-node/core';

/**
 * Map of all GraphQL operations in the project.
 *
 * This map has several performance disadvantages:
 * 1. It is not tree-shakeable, so it will include all operations in the project.
 * 2. It is not minifiable, so the string of a GraphQL query will be multiple times inside the bundle.
 * 3. It does not support dead code elimination, so it will add unused operations.
 *
 * Therefore it is highly recommended to use the babel or swc plugin for production.
 * Learn more about it here: https://the-guild.dev/graphql/codegen/plugins/presets/preset-client#reducing-bundle-size
 */
type Documents = {
    "\n  fragment UserFields on User {\n    id\n    username\n    displayName\n    online\n  }\n": typeof types.UserFieldsFragmentDoc,
    "\n  fragment MediaFields on Media {\n    key\n    downloadUrl\n    contentType\n  }\n": typeof types.MediaFieldsFragmentDoc,
    "\n  fragment ReceiptFields on Receipt {\n    messageId\n    deliveredAt\n    readAt\n    user {\n      ...UserFields\n    }\n  }\n": typeof types.ReceiptFieldsFragmentDoc,
    "\n  fragment MessageFields on Message {\n    id\n    body\n    createdAt\n    chatId\n    sender {\n      ...UserFields\n    }\n    media {\n      ...MediaFields\n    }\n    receipts {\n      ...ReceiptFields\n    }\n  }\n": typeof types.MessageFieldsFragmentDoc,
    "\n  fragment ChatSummary on Chat {\n    id\n    kind\n    title\n    unread\n    disappearingSeconds\n    members {\n      ...UserFields\n    }\n    lastMessage {\n      ...MessageFields\n    }\n  }\n": typeof types.ChatSummaryFragmentDoc,
    "\n  query Me {\n    me {\n      ...UserFields\n    }\n  }\n": typeof types.MeDocument,
    "\n  query Chats {\n    chats {\n      ...ChatSummary\n    }\n  }\n": typeof types.ChatsDocument,
    "\n  query Chat($id: ID!) {\n    chat(id: $id) {\n      ...ChatSummary\n    }\n  }\n": typeof types.ChatDocument,
    "\n  query Messages($chatId: ID!, $before: DateTime, $limit: Int!) {\n    messages(chatId: $chatId, before: $before, limit: $limit) {\n      ...MessageFields\n    }\n  }\n": typeof types.MessagesDocument,
    "\n  query Presence($userIds: [ID!]!) {\n    presence(userIds: $userIds) {\n      ...UserFields\n    }\n  }\n": typeof types.PresenceDocument,
    "\n  query ReactionsEnabled {\n    reactionsEnabled\n  }\n": typeof types.ReactionsEnabledDocument,
    "\n  query OpsStats {\n    opsStats {\n      onlineCount\n      dlqCount\n    }\n  }\n": typeof types.OpsStatsDocument,
    "\n  mutation Signup($username: String!, $displayName: String!, $password: String!) {\n    signup(username: $username, displayName: $displayName, password: $password) {\n      token\n      user {\n        ...UserFields\n      }\n    }\n  }\n": typeof types.SignupDocument,
    "\n  mutation Login($username: String!, $password: String!) {\n    login(username: $username, password: $password) {\n      token\n      user {\n        ...UserFields\n      }\n    }\n  }\n": typeof types.LoginDocument,
    "\n  mutation Logout {\n    logout\n  }\n": typeof types.LogoutDocument,
    "\n  mutation LogoutAll {\n    logoutAll\n  }\n": typeof types.LogoutAllDocument,
    "\n  mutation CreateChat($kind: ChatKind!, $title: String, $memberUsernames: [String!]!) {\n    createChat(kind: $kind, title: $title, memberUsernames: $memberUsernames) {\n      ...ChatSummary\n    }\n  }\n": typeof types.CreateChatDocument,
    "\n  mutation AddMember($chatId: ID!, $username: String!) {\n    addMember(chatId: $chatId, username: $username) {\n      ...ChatSummary\n    }\n  }\n": typeof types.AddMemberDocument,
    "\n  mutation RequestUpload($chatId: ID!) {\n    requestUpload(chatId: $chatId) {\n      key\n      uploadUrl\n      maxBytes\n    }\n  }\n": typeof types.RequestUploadDocument,
    "\n  mutation SendMessage($chatId: ID!, $body: String!, $mediaKey: String, $idempotencyKey: String) {\n    sendMessage(chatId: $chatId, body: $body, mediaKey: $mediaKey, idempotencyKey: $idempotencyKey) {\n      ...MessageFields\n    }\n  }\n": typeof types.SendMessageDocument,
    "\n  mutation SetDisappearing($chatId: ID!, $enabled: Boolean!) {\n    setDisappearing(chatId: $chatId, enabled: $enabled) {\n      ...ChatSummary\n    }\n  }\n": typeof types.SetDisappearingDocument,
    "\n  mutation SetTyping($chatId: ID!, $typing: Boolean!) {\n    setTyping(chatId: $chatId, typing: $typing)\n  }\n": typeof types.SetTypingDocument,
    "\n  mutation MarkRead($chatId: ID!, $messageId: ID!) {\n    markRead(chatId: $chatId, messageId: $messageId)\n  }\n": typeof types.MarkReadDocument,
    "\n  mutation Heartbeat {\n    heartbeat\n  }\n": typeof types.HeartbeatDocument,
    "\n  mutation CreateApiKey($label: String!) {\n    createApiKey(label: $label) {\n      id\n      secret\n    }\n  }\n": typeof types.CreateApiKeyDocument,
    "\n  mutation SetReactionsRollout($percent: Int!) {\n    setReactionsRollout(percent: $percent)\n  }\n": typeof types.SetReactionsRolloutDocument,
    "\n  mutation TriggerFailingJob {\n    triggerFailingJob\n  }\n": typeof types.TriggerFailingJobDocument,
    "\n  subscription MessageAdded($chatId: ID!) {\n    messageAdded(chatId: $chatId) {\n      ...MessageFields\n    }\n  }\n": typeof types.MessageAddedDocument,
    "\n  subscription Typing($chatId: ID!) {\n    typing(chatId: $chatId) {\n      typing\n      user {\n        ...UserFields\n      }\n    }\n  }\n": typeof types.TypingDocument,
    "\n  subscription ReceiptChanged($chatId: ID!) {\n    receiptChanged(chatId: $chatId) {\n      ...ReceiptFields\n    }\n  }\n": typeof types.ReceiptChangedDocument,
    "\n  subscription PresenceChanged($userIds: [ID!]!) {\n    presenceChanged(userIds: $userIds) {\n      ...UserFields\n    }\n  }\n": typeof types.PresenceChangedDocument,
};
const documents: Documents = {
    "\n  fragment UserFields on User {\n    id\n    username\n    displayName\n    online\n  }\n": types.UserFieldsFragmentDoc,
    "\n  fragment MediaFields on Media {\n    key\n    downloadUrl\n    contentType\n  }\n": types.MediaFieldsFragmentDoc,
    "\n  fragment ReceiptFields on Receipt {\n    messageId\n    deliveredAt\n    readAt\n    user {\n      ...UserFields\n    }\n  }\n": types.ReceiptFieldsFragmentDoc,
    "\n  fragment MessageFields on Message {\n    id\n    body\n    createdAt\n    chatId\n    sender {\n      ...UserFields\n    }\n    media {\n      ...MediaFields\n    }\n    receipts {\n      ...ReceiptFields\n    }\n  }\n": types.MessageFieldsFragmentDoc,
    "\n  fragment ChatSummary on Chat {\n    id\n    kind\n    title\n    unread\n    disappearingSeconds\n    members {\n      ...UserFields\n    }\n    lastMessage {\n      ...MessageFields\n    }\n  }\n": types.ChatSummaryFragmentDoc,
    "\n  query Me {\n    me {\n      ...UserFields\n    }\n  }\n": types.MeDocument,
    "\n  query Chats {\n    chats {\n      ...ChatSummary\n    }\n  }\n": types.ChatsDocument,
    "\n  query Chat($id: ID!) {\n    chat(id: $id) {\n      ...ChatSummary\n    }\n  }\n": types.ChatDocument,
    "\n  query Messages($chatId: ID!, $before: DateTime, $limit: Int!) {\n    messages(chatId: $chatId, before: $before, limit: $limit) {\n      ...MessageFields\n    }\n  }\n": types.MessagesDocument,
    "\n  query Presence($userIds: [ID!]!) {\n    presence(userIds: $userIds) {\n      ...UserFields\n    }\n  }\n": types.PresenceDocument,
    "\n  query ReactionsEnabled {\n    reactionsEnabled\n  }\n": types.ReactionsEnabledDocument,
    "\n  query OpsStats {\n    opsStats {\n      onlineCount\n      dlqCount\n    }\n  }\n": types.OpsStatsDocument,
    "\n  mutation Signup($username: String!, $displayName: String!, $password: String!) {\n    signup(username: $username, displayName: $displayName, password: $password) {\n      token\n      user {\n        ...UserFields\n      }\n    }\n  }\n": types.SignupDocument,
    "\n  mutation Login($username: String!, $password: String!) {\n    login(username: $username, password: $password) {\n      token\n      user {\n        ...UserFields\n      }\n    }\n  }\n": types.LoginDocument,
    "\n  mutation Logout {\n    logout\n  }\n": types.LogoutDocument,
    "\n  mutation LogoutAll {\n    logoutAll\n  }\n": types.LogoutAllDocument,
    "\n  mutation CreateChat($kind: ChatKind!, $title: String, $memberUsernames: [String!]!) {\n    createChat(kind: $kind, title: $title, memberUsernames: $memberUsernames) {\n      ...ChatSummary\n    }\n  }\n": types.CreateChatDocument,
    "\n  mutation AddMember($chatId: ID!, $username: String!) {\n    addMember(chatId: $chatId, username: $username) {\n      ...ChatSummary\n    }\n  }\n": types.AddMemberDocument,
    "\n  mutation RequestUpload($chatId: ID!) {\n    requestUpload(chatId: $chatId) {\n      key\n      uploadUrl\n      maxBytes\n    }\n  }\n": types.RequestUploadDocument,
    "\n  mutation SendMessage($chatId: ID!, $body: String!, $mediaKey: String, $idempotencyKey: String) {\n    sendMessage(chatId: $chatId, body: $body, mediaKey: $mediaKey, idempotencyKey: $idempotencyKey) {\n      ...MessageFields\n    }\n  }\n": types.SendMessageDocument,
    "\n  mutation SetDisappearing($chatId: ID!, $enabled: Boolean!) {\n    setDisappearing(chatId: $chatId, enabled: $enabled) {\n      ...ChatSummary\n    }\n  }\n": types.SetDisappearingDocument,
    "\n  mutation SetTyping($chatId: ID!, $typing: Boolean!) {\n    setTyping(chatId: $chatId, typing: $typing)\n  }\n": types.SetTypingDocument,
    "\n  mutation MarkRead($chatId: ID!, $messageId: ID!) {\n    markRead(chatId: $chatId, messageId: $messageId)\n  }\n": types.MarkReadDocument,
    "\n  mutation Heartbeat {\n    heartbeat\n  }\n": types.HeartbeatDocument,
    "\n  mutation CreateApiKey($label: String!) {\n    createApiKey(label: $label) {\n      id\n      secret\n    }\n  }\n": types.CreateApiKeyDocument,
    "\n  mutation SetReactionsRollout($percent: Int!) {\n    setReactionsRollout(percent: $percent)\n  }\n": types.SetReactionsRolloutDocument,
    "\n  mutation TriggerFailingJob {\n    triggerFailingJob\n  }\n": types.TriggerFailingJobDocument,
    "\n  subscription MessageAdded($chatId: ID!) {\n    messageAdded(chatId: $chatId) {\n      ...MessageFields\n    }\n  }\n": types.MessageAddedDocument,
    "\n  subscription Typing($chatId: ID!) {\n    typing(chatId: $chatId) {\n      typing\n      user {\n        ...UserFields\n      }\n    }\n  }\n": types.TypingDocument,
    "\n  subscription ReceiptChanged($chatId: ID!) {\n    receiptChanged(chatId: $chatId) {\n      ...ReceiptFields\n    }\n  }\n": types.ReceiptChangedDocument,
    "\n  subscription PresenceChanged($userIds: [ID!]!) {\n    presenceChanged(userIds: $userIds) {\n      ...UserFields\n    }\n  }\n": types.PresenceChangedDocument,
};

/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 *
 *
 * @example
 * ```ts
 * const query = graphql(`query GetUser($id: ID!) { user(id: $id) { name } }`);
 * ```
 *
 * The query argument is unknown!
 * Please regenerate the types.
 */
export function graphql(source: string): unknown;

/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  fragment UserFields on User {\n    id\n    username\n    displayName\n    online\n  }\n"): (typeof documents)["\n  fragment UserFields on User {\n    id\n    username\n    displayName\n    online\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  fragment MediaFields on Media {\n    key\n    downloadUrl\n    contentType\n  }\n"): (typeof documents)["\n  fragment MediaFields on Media {\n    key\n    downloadUrl\n    contentType\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  fragment ReceiptFields on Receipt {\n    messageId\n    deliveredAt\n    readAt\n    user {\n      ...UserFields\n    }\n  }\n"): (typeof documents)["\n  fragment ReceiptFields on Receipt {\n    messageId\n    deliveredAt\n    readAt\n    user {\n      ...UserFields\n    }\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  fragment MessageFields on Message {\n    id\n    body\n    createdAt\n    chatId\n    sender {\n      ...UserFields\n    }\n    media {\n      ...MediaFields\n    }\n    receipts {\n      ...ReceiptFields\n    }\n  }\n"): (typeof documents)["\n  fragment MessageFields on Message {\n    id\n    body\n    createdAt\n    chatId\n    sender {\n      ...UserFields\n    }\n    media {\n      ...MediaFields\n    }\n    receipts {\n      ...ReceiptFields\n    }\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  fragment ChatSummary on Chat {\n    id\n    kind\n    title\n    unread\n    disappearingSeconds\n    members {\n      ...UserFields\n    }\n    lastMessage {\n      ...MessageFields\n    }\n  }\n"): (typeof documents)["\n  fragment ChatSummary on Chat {\n    id\n    kind\n    title\n    unread\n    disappearingSeconds\n    members {\n      ...UserFields\n    }\n    lastMessage {\n      ...MessageFields\n    }\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  query Me {\n    me {\n      ...UserFields\n    }\n  }\n"): (typeof documents)["\n  query Me {\n    me {\n      ...UserFields\n    }\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  query Chats {\n    chats {\n      ...ChatSummary\n    }\n  }\n"): (typeof documents)["\n  query Chats {\n    chats {\n      ...ChatSummary\n    }\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  query Chat($id: ID!) {\n    chat(id: $id) {\n      ...ChatSummary\n    }\n  }\n"): (typeof documents)["\n  query Chat($id: ID!) {\n    chat(id: $id) {\n      ...ChatSummary\n    }\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  query Messages($chatId: ID!, $before: DateTime, $limit: Int!) {\n    messages(chatId: $chatId, before: $before, limit: $limit) {\n      ...MessageFields\n    }\n  }\n"): (typeof documents)["\n  query Messages($chatId: ID!, $before: DateTime, $limit: Int!) {\n    messages(chatId: $chatId, before: $before, limit: $limit) {\n      ...MessageFields\n    }\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  query Presence($userIds: [ID!]!) {\n    presence(userIds: $userIds) {\n      ...UserFields\n    }\n  }\n"): (typeof documents)["\n  query Presence($userIds: [ID!]!) {\n    presence(userIds: $userIds) {\n      ...UserFields\n    }\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  query ReactionsEnabled {\n    reactionsEnabled\n  }\n"): (typeof documents)["\n  query ReactionsEnabled {\n    reactionsEnabled\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  query OpsStats {\n    opsStats {\n      onlineCount\n      dlqCount\n    }\n  }\n"): (typeof documents)["\n  query OpsStats {\n    opsStats {\n      onlineCount\n      dlqCount\n    }\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  mutation Signup($username: String!, $displayName: String!, $password: String!) {\n    signup(username: $username, displayName: $displayName, password: $password) {\n      token\n      user {\n        ...UserFields\n      }\n    }\n  }\n"): (typeof documents)["\n  mutation Signup($username: String!, $displayName: String!, $password: String!) {\n    signup(username: $username, displayName: $displayName, password: $password) {\n      token\n      user {\n        ...UserFields\n      }\n    }\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  mutation Login($username: String!, $password: String!) {\n    login(username: $username, password: $password) {\n      token\n      user {\n        ...UserFields\n      }\n    }\n  }\n"): (typeof documents)["\n  mutation Login($username: String!, $password: String!) {\n    login(username: $username, password: $password) {\n      token\n      user {\n        ...UserFields\n      }\n    }\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  mutation Logout {\n    logout\n  }\n"): (typeof documents)["\n  mutation Logout {\n    logout\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  mutation LogoutAll {\n    logoutAll\n  }\n"): (typeof documents)["\n  mutation LogoutAll {\n    logoutAll\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  mutation CreateChat($kind: ChatKind!, $title: String, $memberUsernames: [String!]!) {\n    createChat(kind: $kind, title: $title, memberUsernames: $memberUsernames) {\n      ...ChatSummary\n    }\n  }\n"): (typeof documents)["\n  mutation CreateChat($kind: ChatKind!, $title: String, $memberUsernames: [String!]!) {\n    createChat(kind: $kind, title: $title, memberUsernames: $memberUsernames) {\n      ...ChatSummary\n    }\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  mutation AddMember($chatId: ID!, $username: String!) {\n    addMember(chatId: $chatId, username: $username) {\n      ...ChatSummary\n    }\n  }\n"): (typeof documents)["\n  mutation AddMember($chatId: ID!, $username: String!) {\n    addMember(chatId: $chatId, username: $username) {\n      ...ChatSummary\n    }\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  mutation RequestUpload($chatId: ID!) {\n    requestUpload(chatId: $chatId) {\n      key\n      uploadUrl\n      maxBytes\n    }\n  }\n"): (typeof documents)["\n  mutation RequestUpload($chatId: ID!) {\n    requestUpload(chatId: $chatId) {\n      key\n      uploadUrl\n      maxBytes\n    }\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  mutation SendMessage($chatId: ID!, $body: String!, $mediaKey: String, $idempotencyKey: String) {\n    sendMessage(chatId: $chatId, body: $body, mediaKey: $mediaKey, idempotencyKey: $idempotencyKey) {\n      ...MessageFields\n    }\n  }\n"): (typeof documents)["\n  mutation SendMessage($chatId: ID!, $body: String!, $mediaKey: String, $idempotencyKey: String) {\n    sendMessage(chatId: $chatId, body: $body, mediaKey: $mediaKey, idempotencyKey: $idempotencyKey) {\n      ...MessageFields\n    }\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  mutation SetDisappearing($chatId: ID!, $enabled: Boolean!) {\n    setDisappearing(chatId: $chatId, enabled: $enabled) {\n      ...ChatSummary\n    }\n  }\n"): (typeof documents)["\n  mutation SetDisappearing($chatId: ID!, $enabled: Boolean!) {\n    setDisappearing(chatId: $chatId, enabled: $enabled) {\n      ...ChatSummary\n    }\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  mutation SetTyping($chatId: ID!, $typing: Boolean!) {\n    setTyping(chatId: $chatId, typing: $typing)\n  }\n"): (typeof documents)["\n  mutation SetTyping($chatId: ID!, $typing: Boolean!) {\n    setTyping(chatId: $chatId, typing: $typing)\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  mutation MarkRead($chatId: ID!, $messageId: ID!) {\n    markRead(chatId: $chatId, messageId: $messageId)\n  }\n"): (typeof documents)["\n  mutation MarkRead($chatId: ID!, $messageId: ID!) {\n    markRead(chatId: $chatId, messageId: $messageId)\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  mutation Heartbeat {\n    heartbeat\n  }\n"): (typeof documents)["\n  mutation Heartbeat {\n    heartbeat\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  mutation CreateApiKey($label: String!) {\n    createApiKey(label: $label) {\n      id\n      secret\n    }\n  }\n"): (typeof documents)["\n  mutation CreateApiKey($label: String!) {\n    createApiKey(label: $label) {\n      id\n      secret\n    }\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  mutation SetReactionsRollout($percent: Int!) {\n    setReactionsRollout(percent: $percent)\n  }\n"): (typeof documents)["\n  mutation SetReactionsRollout($percent: Int!) {\n    setReactionsRollout(percent: $percent)\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  mutation TriggerFailingJob {\n    triggerFailingJob\n  }\n"): (typeof documents)["\n  mutation TriggerFailingJob {\n    triggerFailingJob\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  subscription MessageAdded($chatId: ID!) {\n    messageAdded(chatId: $chatId) {\n      ...MessageFields\n    }\n  }\n"): (typeof documents)["\n  subscription MessageAdded($chatId: ID!) {\n    messageAdded(chatId: $chatId) {\n      ...MessageFields\n    }\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  subscription Typing($chatId: ID!) {\n    typing(chatId: $chatId) {\n      typing\n      user {\n        ...UserFields\n      }\n    }\n  }\n"): (typeof documents)["\n  subscription Typing($chatId: ID!) {\n    typing(chatId: $chatId) {\n      typing\n      user {\n        ...UserFields\n      }\n    }\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  subscription ReceiptChanged($chatId: ID!) {\n    receiptChanged(chatId: $chatId) {\n      ...ReceiptFields\n    }\n  }\n"): (typeof documents)["\n  subscription ReceiptChanged($chatId: ID!) {\n    receiptChanged(chatId: $chatId) {\n      ...ReceiptFields\n    }\n  }\n"];
/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "\n  subscription PresenceChanged($userIds: [ID!]!) {\n    presenceChanged(userIds: $userIds) {\n      ...UserFields\n    }\n  }\n"): (typeof documents)["\n  subscription PresenceChanged($userIds: [ID!]!) {\n    presenceChanged(userIds: $userIds) {\n      ...UserFields\n    }\n  }\n"];

export function graphql(source: string) {
  return (documents as any)[source] ?? {};
}

export type DocumentType<TDocumentNode extends DocumentNode<any, any>> = TDocumentNode extends DocumentNode<  infer TType,  any>  ? TType  : never;