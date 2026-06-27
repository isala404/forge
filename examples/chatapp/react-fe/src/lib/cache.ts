import type { CacheExchangeOpts } from '@urql/exchange-graphcache'
import { ChatsQuery, MESSAGE_PAGE_SIZE, MessagesQuery } from '../graphql/operations'

type Ided = { id: string; __typename?: string }

// Normalized cache. User/Chat/Message are entities keyed by id, so a Message that
// arrives over a subscription and the same Message inside a query share one record
// and update in place. Receipt has no id field (identified by messageId+user) so it
// resolves inline within its parent Message rather than as a standalone record.
// Value types (SessionPayload, OpsStats, UploadTicket, ApiKeyPayload, Media,
// TypingEvent) are deliberately not normalized.
export const cacheConfig: CacheExchangeOpts = {
  keys: {
    User: (u) => (u as { id: string }).id,
    Chat: (c) => (c as { id: string }).id,
    Message: (m) => (m as { id: string }).id,
    Media: () => null,
    Receipt: () => null,
    TypingEvent: () => null,
    SessionPayload: () => null,
    ApiKeyPayload: () => null,
    UploadTicket: () => null,
    OpsStats: () => null,
  },
  // graphcache updates entity fields automatically but never adds an entity to a list
  // it wasn't already in. These two write the mutation results into the list queries
  // the UI reads, so a new chat and the sender's own message render from the mutation
  // alone, without waiting on the at-most-once pubsub echo, which a just-created chat
  // may not be subscribed to yet (the lost-echo race, most visible on slower backends).
  updates: {
    Mutation: {
      createChat(result, _args, cache) {
        const chat = result.createChat as Ided | null
        if (!chat) return
        cache.updateQuery({ query: ChatsQuery }, (data) => {
          const d = data as { chats: Ided[] } | null
          if (!d || d.chats.some((c) => c.id === chat.id)) return data
          return { ...d, chats: [chat, ...d.chats] } as typeof data
        })
      },
      sendMessage(result, args, cache) {
        const message = result.sendMessage as Ided | null
        if (!message) return
        const chatId = args.chatId as string
        cache.updateQuery(
          { query: MessagesQuery, variables: { chatId, limit: MESSAGE_PAGE_SIZE } },
          (data) => {
            const d = data as { messages: Ided[] } | null
            if (!d || d.messages.some((m) => m.id === message.id)) return data
            return { ...d, messages: [message, ...d.messages] } as typeof data
          },
        )
      },
    },
  },
}
