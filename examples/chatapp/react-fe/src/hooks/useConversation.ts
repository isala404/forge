import { useEffect, useMemo, useState } from 'react'
import { useQuery, useSubscription } from 'urql'
import {
  MESSAGE_PAGE_SIZE,
  MessageAddedSubscription,
  MessagesQuery,
  ReceiptChangedSubscription,
} from '../graphql/operations'
import { onWsReconnect } from '../lib/urql'
import {
  unmaskMessage,
  unmaskReceipt,
  unmaskUser,
  type Message,
  type Receipt,
} from '../lib/derive'

const PAGE = MESSAGE_PAGE_SIZE
const EMPTY_RECEIPTS: ReadonlyMap<string, Receipt[]> = new Map()

export type ChatMessage = {
  message: Message
  receipts: Receipt[]
}

// Owns the message window for one chat: the initial query result plus everything
// that arrives live. The API returns newest-first; we preserve that and let the
// view render bottom-up. Receipts are not normalized (no id), so a live
// receiptChanged event is folded into a per-message override map keyed by message
// id, and the effective receipt list for a message is its base receipts with any
// override for the same user replacing the base entry.
export function useConversation(chatId: string) {
  const [{ data, fetching, error }, refetch] = useQuery({
    query: MessagesQuery,
    variables: { chatId, limit: PAGE },
    requestPolicy: 'cache-and-network',
  })

  // Live state is bucketed by chatId so switching chats resets it via a render-
  // time comparison (the supported "adjust state on prop change" pattern) with no
  // effect and no ref.
  const [live, setLive] = useState<{
    chatId: string
    messages: Message[]
    receipts: Map<string, Receipt[]>
  }>({ chatId, messages: [], receipts: new Map() })

  if (live.chatId !== chatId) {
    setLive({ chatId, messages: [], receipts: new Map() })
  }

  // On every socket (re)connect, pull the durable window from Postgres. Anything
  // published over pubsub while we were offline never arrived; the network-only
  // refetch reconciles it against the normalized cache (messages dedup by id).
  useEffect(
    () => onWsReconnect(() => refetch({ requestPolicy: 'network-only' })),
    [refetch],
  )

  useSubscription(
    { query: MessageAddedSubscription, variables: { chatId } },
    (_prev, value) => {
      if (value?.messageAdded) {
        const msg = unmaskMessage(value.messageAdded)
        setLive((cur) => {
          if (cur.chatId !== chatId || cur.messages.some((m) => m.id === msg.id)) {
            return cur
          }
          return { ...cur, messages: [msg, ...cur.messages] }
        })
      }
      return value
    },
  )

  useSubscription(
    { query: ReceiptChangedSubscription, variables: { chatId } },
    (_prev, value) => {
      if (value?.receiptChanged) {
        const receipt = unmaskReceipt(value.receiptChanged)
        setLive((cur) => {
          if (cur.chatId !== chatId) return cur
          const next = new Map(cur.receipts)
          const userId = unmaskUser(receipt.user).id
          const existing = next.get(receipt.messageId) ?? []
          next.set(receipt.messageId, [
            ...existing.filter((r) => unmaskUser(r.user).id !== userId),
            receipt,
          ])
          return { ...cur, receipts: next }
        })
      }
      return value
    },
  )

  const items: ChatMessage[] = useMemo(() => {
    const fresh = live.chatId === chatId
    const liveMessages = fresh ? live.messages : []
    const receiptOverrides = fresh ? live.receipts : EMPTY_RECEIPTS

    const base = (data?.messages ?? []).map(unmaskMessage)
    const byId = new Map<string, Message>()
    for (const m of liveMessages) byId.set(m.id, m)
    for (const m of base) if (!byId.has(m.id)) byId.set(m.id, m)

    return [...byId.values()]
      .sort((a, b) => Date.parse(b.createdAt) - Date.parse(a.createdAt))
      .map((message) => ({
        message,
        receipts: effectiveReceipts(message, receiptOverrides.get(message.id)),
      }))
  }, [data, live, chatId])

  return {
    items,
    fetching: fetching && (data?.messages?.length ?? 0) === 0,
    error,
    refetch,
  }
}

function effectiveReceipts(
  message: Message,
  overrides: Receipt[] | undefined,
): Receipt[] {
  const base = message.receipts.map(unmaskReceipt)
  if (!overrides || overrides.length === 0) return base
  const overriddenUsers = new Set(overrides.map((r) => unmaskUser(r.user).id))
  return [
    ...base.filter((r) => !overriddenUsers.has(unmaskUser(r.user).id)),
    ...overrides,
  ]
}
