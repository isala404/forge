import { Fragment, useEffect, useLayoutEffect, useRef } from 'react'
import { useMutation, useQuery } from 'urql'
import { ChatCircleDots } from '@phosphor-icons/react'
import { ChatQuery, MarkReadMutation } from '../../graphql/operations'
import { useConversation } from '../../hooks/useConversation'
import { useTypingWatch } from '../../hooks/useTyping'
import { unmaskChat, unmaskUser, type Chat } from '../../lib/derive'
import { ConversationHeader } from './ConversationHeader'
import { MessageBubble } from './MessageBubble'
import { Composer } from './Composer'
import { TypingIndicator } from './TypingIndicator'
import { MessageSkeleton } from '../ui/Skeleton'
import { EmptyState, ErrorState } from '../ui/States'
import { dayKey, formatDayLabel } from '../../lib/format'
import { errorMessage } from '../../lib/errors'

type Props = {
  chatId: string
  selfId: string
  onBack: () => void
}

export function Conversation({ chatId, selfId, onBack }: Props) {
  const [{ data: chatData, fetching: chatFetching, error: chatError }, refetchChat] =
    useQuery({ query: ChatQuery, variables: { id: chatId } })
  const chat: Chat | null = chatData?.chat ? unmaskChat(chatData.chat) : null

  const { items, fetching, error, refetch } = useConversation(chatId)
  const typers = useTypingWatch(chatId)
  const [, markRead] = useMutation(MarkReadMutation)

  const scrollRef = useRef<HTMLDivElement>(null)
  const newest = items[0]?.message ?? null

  // Mark the newest incoming message read whenever it changes.
  const lastReadId = useRef<string | null>(null)
  useEffect(() => {
    if (!newest) return
    const sender = unmaskUser(newest.sender)
    if (sender.id === selfId) return
    if (lastReadId.current === newest.id) return
    lastReadId.current = newest.id
    void markRead({ chatId, messageId: newest.id })
  }, [newest, chatId, selfId, markRead])

  // Keep the column pinned to the bottom (newest) as messages arrive.
  useLayoutEffect(() => {
    const el = scrollRef.current
    if (el) el.scrollTop = el.scrollHeight
  }, [items.length, typers.length])

  if (chatError) {
    return (
      <section className="convo">
        <ErrorState
          message={errorMessage(chatError)}
          onRetry={() => refetchChat({ requestPolicy: 'network-only' })}
        />
      </section>
    )
  }

  // Render oldest-first (items is newest-first), grouped by day.
  const ordered = [...items].reverse()

  return (
    <section className="convo">
      {chat ? (
        <ConversationHeader chat={chat} selfId={selfId} onBack={onBack} />
      ) : chatFetching ? (
        <div className="convo-head convo-head-skeleton" />
      ) : null}

      <div className="convo-scroll" ref={scrollRef}>
        {fetching && items.length === 0 ? (
          <div className="convo-loading">
            <MessageSkeleton />
            <MessageSkeleton mine />
            <MessageSkeleton />
            <MessageSkeleton mine />
          </div>
        ) : error && items.length === 0 ? (
          <ErrorState
            message={errorMessage(error)}
            onRetry={() => refetch({ requestPolicy: 'network-only' })}
          />
        ) : ordered.length === 0 ? (
          <EmptyState
            icon={<ChatCircleDots size={30} weight="duotone" />}
            title="No messages yet"
            body="Say hello to start the conversation."
          />
        ) : (
          <div className="convo-messages">
            {ordered.map((item, i) => {
              const prev = ordered[i - 1]?.message
              const showDay =
                !prev || dayKey(prev.createdAt) !== dayKey(item.message.createdAt)
              const sameSenderAsPrev =
                prev && unmaskUser(prev.sender).id === unmaskUser(item.message.sender).id
              return (
                <Fragment key={item.message.id}>
                  {showDay && (
                    <div className="day-divider">
                      <span>{formatDayLabel(item.message.createdAt)}</span>
                    </div>
                  )}
                  <MessageBubble
                    message={item.message}
                    receipts={item.receipts}
                    selfId={selfId}
                    showSender={
                      Boolean(chat && chat.kind === 'GROUP') &&
                      (!sameSenderAsPrev || showDay)
                    }
                  />
                </Fragment>
              )
            })}
          </div>
        )}

        <TypingIndicator typers={typers} />
      </div>

      <Composer chatId={chatId} />
    </section>
  )
}
