import { Users } from '@phosphor-icons/react'
import { Avatar } from '../ui/Avatar'
import { PresenceDot } from '../ui/PresenceDot'
import {
  chatDisplayName,
  directPeerOnline,
  isGroup,
  lastMessage,
  previewText,
  unmaskUser,
  type Chat,
  type Message,
} from '../../lib/derive'
import { formatListTime } from '../../lib/format'

type Props = {
  chat: Chat
  selfId: string
  active: boolean
  onSelect: () => void
}

function senderPrefix(last: Message, selfId: string): string {
  const u = unmaskUser(last.sender)
  return `${u.id === selfId ? 'You' : u.displayName}: `
}

export function ChatRow({ chat, selfId, active, onSelect }: Props) {
  const name = chatDisplayName(chat, selfId)
  const group = isGroup(chat)
  const last = lastMessage(chat)
  const online = directPeerOnline(chat, selfId)
  const prefix = last && group ? senderPrefix(last, selfId) : ''

  return (
    <button
      className="chat-row"
      data-active={active || undefined}
      onClick={onSelect}
      aria-current={active ? 'true' : undefined}
    >
      <span className="chat-row-avatar">
        <Avatar name={name} seed={chat.id} group={group} />
        {!group && online !== null && (
          <span className="chat-row-presence">
            <PresenceDot
              online={online}
              label={`${name} ${online ? 'online' : 'offline'}`}
            />
          </span>
        )}
      </span>

      <span className="chat-row-body">
        <span className="chat-row-top">
          <span className="chat-row-name">
            {group && <Users size={14} weight="bold" className="chat-row-tag" />}
            {name}
          </span>
          {last && (
            <span className="chat-row-time">{formatListTime(last.createdAt)}</span>
          )}
        </span>
        <span className="chat-row-bottom">
          <span className="chat-row-preview">
            {prefix}
            {previewText(last)}
          </span>
          {chat.unread > 0 && (
            <span className="unread-badge" aria-label={`${chat.unread} unread`}>
              {chat.unread > 99 ? '99+' : chat.unread}
            </span>
          )}
        </span>
      </span>
    </button>
  )
}
