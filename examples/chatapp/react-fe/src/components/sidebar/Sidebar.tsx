import { useEffect, useMemo, useState } from 'react'
import { useQuery } from 'urql'
import {
  GearSix,
  MagnifyingGlass,
  NotePencil,
  SignOut,
} from '@phosphor-icons/react'
import { ChatsQuery } from '../../graphql/operations'
import {
  chatDisplayName,
  unmaskChat,
  unmaskUser,
  type Chat,
} from '../../lib/derive'
import { ChatRow } from './ChatRow'
import { ChatRowSkeleton } from '../ui/Skeleton'
import { EmptyState, ErrorState } from '../ui/States'
import { Avatar } from '../ui/Avatar'
import { errorMessage } from '../../lib/errors'
import { onWsReconnect } from '../../lib/urql'
import type { User } from '../../lib/derive'

type Props = {
  self: User
  activeChatId: string | null
  onSelect: (id: string) => void
  onNewChat: () => void
  onSettings: () => void
  onLogout: () => void
  onChatsLoaded: (memberIds: string[]) => void
}

export function Sidebar({
  self,
  activeChatId,
  onSelect,
  onNewChat,
  onSettings,
  onLogout,
  onChatsLoaded,
}: Props) {
  const [{ data, fetching, error }, refetch] = useQuery({ query: ChatsQuery })
  const [filter, setFilter] = useState('')

  const chats: Chat[] = useMemo(
    () => (data?.chats ?? []).map(unmaskChat),
    [data],
  )

  // The sidebar has no subscription, so unread counts and last-message previews
  // never update live. On every socket (re)connect, refetch the list from the
  // network so it catches up on everything missed while offline.
  useEffect(
    () => onWsReconnect(() => refetch({ requestPolicy: 'network-only' })),
    [refetch],
  )

  // Lift every member id so the parent can drive one presence subscription.
  useEffect(() => {
    if (!data) return
    const ids = new Set<string>()
    for (const c of chats) for (const m of c.members) ids.add(unmaskUser(m).id)
    onChatsLoaded([...ids])
  }, [chats, data, onChatsLoaded])

  const visible = filter.trim()
    ? chats.filter((c) =>
        chatDisplayName(c, self.id)
          .toLowerCase()
          .includes(filter.trim().toLowerCase()),
      )
    : chats

  return (
    <aside className="sidebar">
      <header className="sidebar-head">
        <div className="sidebar-me">
          <Avatar name={self.displayName} seed={self.id} size={38} />
          <div className="sidebar-me-text">
            <strong>{self.displayName}</strong>
            <span>@{self.username}</span>
          </div>
        </div>
        <div className="sidebar-actions">
          <button className="icon-btn" onClick={onNewChat} aria-label="New chat">
            <NotePencil size={20} />
          </button>
          <button className="icon-btn" onClick={onSettings} aria-label="Settings">
            <GearSix size={20} />
          </button>
          <button className="icon-btn" onClick={onLogout} aria-label="Sign out">
            <SignOut size={20} />
          </button>
        </div>
      </header>

      <div className="sidebar-search">
        <MagnifyingGlass size={17} weight="bold" />
        <input
          className="sidebar-search-input"
          placeholder="Search chats"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          aria-label="Search chats"
        />
      </div>

      <div className="sidebar-list" role="list">
        {fetching && chats.length === 0 ? (
          Array.from({ length: 6 }).map((_, i) => <ChatRowSkeleton key={i} />)
        ) : error ? (
          <ErrorState message={errorMessage(error)} onRetry={() => refetch({ requestPolicy: 'network-only' })} />
        ) : chats.length === 0 ? (
          <EmptyState
            icon={<NotePencil size={30} weight="duotone" />}
            title="No conversations yet"
            body="Start a direct message or spin up a group to get going."
          />
        ) : visible.length === 0 ? (
          <EmptyState
            icon={<MagnifyingGlass size={28} weight="duotone" />}
            title="No matches"
            body={`Nothing matches "${filter.trim()}".`}
          />
        ) : (
          visible.map((chat) => (
            <ChatRow
              key={chat.id}
              chat={chat}
              selfId={self.id}
              active={chat.id === activeChatId}
              onSelect={() => onSelect(chat.id)}
            />
          ))
        )}
      </div>
    </aside>
  )
}
