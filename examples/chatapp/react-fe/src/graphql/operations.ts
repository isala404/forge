import { graphql } from '../gql'

// Window size for the messages query. Shared so the graphcache update for sendMessage
// targets the exact same cached query (chatId + limit) that useConversation reads.
export const MESSAGE_PAGE_SIZE = 50

// Every selection the app makes flows through these named fragments. The screens
// compose them; nothing hand-concatenates query text.

export const UserFields = graphql(`
  fragment UserFields on User {
    id
    username
    displayName
    online
  }
`)

export const MediaFields = graphql(`
  fragment MediaFields on Media {
    key
    downloadUrl
    contentType
  }
`)

export const ReceiptFields = graphql(`
  fragment ReceiptFields on Receipt {
    messageId
    deliveredAt
    readAt
    user {
      ...UserFields
    }
  }
`)

export const MessageFields = graphql(`
  fragment MessageFields on Message {
    id
    body
    createdAt
    chatId
    sender {
      ...UserFields
    }
    media {
      ...MediaFields
    }
    receipts {
      ...ReceiptFields
    }
  }
`)

export const ChatSummary = graphql(`
  fragment ChatSummary on Chat {
    id
    kind
    title
    unread
    disappearingSeconds
    members {
      ...UserFields
    }
    lastMessage {
      ...MessageFields
    }
  }
`)

export const MeQuery = graphql(`
  query Me {
    me {
      ...UserFields
    }
  }
`)

export const ChatsQuery = graphql(`
  query Chats {
    chats {
      ...ChatSummary
    }
  }
`)

export const ChatQuery = graphql(`
  query Chat($id: ID!) {
    chat(id: $id) {
      ...ChatSummary
    }
  }
`)

export const MessagesQuery = graphql(`
  query Messages($chatId: ID!, $before: DateTime, $limit: Int!) {
    messages(chatId: $chatId, before: $before, limit: $limit) {
      ...MessageFields
    }
  }
`)

export const PresenceQuery = graphql(`
  query Presence($userIds: [ID!]!) {
    presence(userIds: $userIds) {
      ...UserFields
    }
  }
`)

export const ReactionsEnabledQuery = graphql(`
  query ReactionsEnabled {
    reactionsEnabled
  }
`)

export const OpsStatsQuery = graphql(`
  query OpsStats {
    opsStats {
      onlineCount
      dlqCount
    }
  }
`)

export const SignupMutation = graphql(`
  mutation Signup($username: String!, $displayName: String!, $password: String!) {
    signup(username: $username, displayName: $displayName, password: $password) {
      token
      user {
        ...UserFields
      }
    }
  }
`)

export const LoginMutation = graphql(`
  mutation Login($username: String!, $password: String!) {
    login(username: $username, password: $password) {
      token
      user {
        ...UserFields
      }
    }
  }
`)

export const LogoutMutation = graphql(`
  mutation Logout {
    logout
  }
`)

export const LogoutAllMutation = graphql(`
  mutation LogoutAll {
    logoutAll
  }
`)

export const CreateChatMutation = graphql(`
  mutation CreateChat($kind: ChatKind!, $title: String, $memberUsernames: [String!]!) {
    createChat(kind: $kind, title: $title, memberUsernames: $memberUsernames) {
      ...ChatSummary
    }
  }
`)

export const AddMemberMutation = graphql(`
  mutation AddMember($chatId: ID!, $username: String!) {
    addMember(chatId: $chatId, username: $username) {
      ...ChatSummary
    }
  }
`)

export const RequestUploadMutation = graphql(`
  mutation RequestUpload($chatId: ID!) {
    requestUpload(chatId: $chatId) {
      key
      uploadUrl
      maxBytes
    }
  }
`)

export const SendMessageMutation = graphql(`
  mutation SendMessage($chatId: ID!, $body: String!, $mediaKey: String, $idempotencyKey: String) {
    sendMessage(chatId: $chatId, body: $body, mediaKey: $mediaKey, idempotencyKey: $idempotencyKey) {
      ...MessageFields
    }
  }
`)

export const SetDisappearingMutation = graphql(`
  mutation SetDisappearing($chatId: ID!, $enabled: Boolean!) {
    setDisappearing(chatId: $chatId, enabled: $enabled) {
      ...ChatSummary
    }
  }
`)

export const SetTypingMutation = graphql(`
  mutation SetTyping($chatId: ID!, $typing: Boolean!) {
    setTyping(chatId: $chatId, typing: $typing)
  }
`)

export const MarkReadMutation = graphql(`
  mutation MarkRead($chatId: ID!, $messageId: ID!) {
    markRead(chatId: $chatId, messageId: $messageId)
  }
`)

export const HeartbeatMutation = graphql(`
  mutation Heartbeat {
    heartbeat
  }
`)

export const CreateApiKeyMutation = graphql(`
  mutation CreateApiKey($label: String!) {
    createApiKey(label: $label) {
      id
      secret
    }
  }
`)

export const SetReactionsRolloutMutation = graphql(`
  mutation SetReactionsRollout($percent: Int!) {
    setReactionsRollout(percent: $percent)
  }
`)

export const TriggerFailingJobMutation = graphql(`
  mutation TriggerFailingJob {
    triggerFailingJob
  }
`)

export const MessageAddedSubscription = graphql(`
  subscription MessageAdded($chatId: ID!) {
    messageAdded(chatId: $chatId) {
      ...MessageFields
    }
  }
`)

export const TypingSubscription = graphql(`
  subscription Typing($chatId: ID!) {
    typing(chatId: $chatId) {
      typing
      user {
        ...UserFields
      }
    }
  }
`)

export const ReceiptChangedSubscription = graphql(`
  subscription ReceiptChanged($chatId: ID!) {
    receiptChanged(chatId: $chatId) {
      ...ReceiptFields
    }
  }
`)

export const PresenceChangedSubscription = graphql(`
  subscription PresenceChanged($userIds: [ID!]!) {
    presenceChanged(userIds: $userIds) {
      ...UserFields
    }
  }
`)
