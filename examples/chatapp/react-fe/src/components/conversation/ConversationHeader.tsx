import { useState } from 'react'
import { useMutation } from 'urql'
import { ArrowLeft, Timer, UserPlus } from '@phosphor-icons/react'
import { Avatar } from '../ui/Avatar'
import { PresenceDot } from '../ui/PresenceDot'
import { AddMemberModal } from './AddMemberModal'
import { SetDisappearingMutation } from '../../graphql/operations'
import {
  chatDisplayName,
  chatMembers,
  chatPeers,
  directPeerOnline,
  isGroup,
  type Chat,
} from '../../lib/derive'
import { useToast } from '../ui/toast-context'
import { errorMessage } from '../../lib/errors'

type Props = {
  chat: Chat
  selfId: string
  onBack: () => void
}

export function ConversationHeader({ chat, selfId, onBack }: Props) {
  const [, setDisappearing] = useMutation(SetDisappearingMutation)
  const toast = useToast()
  const [addingMember, setAddingMember] = useState(false)
  const [toggling, setToggling] = useState(false)

  const name = chatDisplayName(chat, selfId)
  const group = isGroup(chat)
  const online = directPeerOnline(chat, selfId)
  const disappearing = chat.disappearingSeconds != null

  const subtitle = group
    ? memberSummary(chat, selfId)
    : online == null
      ? ''
      : online
        ? 'Online'
        : 'Offline'

  async function toggleDisappearing() {
    setToggling(true)
    const res = await setDisappearing({ chatId: chat.id, enabled: !disappearing })
    setToggling(false)
    if (res.error) {
      toast(errorMessage(res.error), 'error')
      return
    }
    toast(
      disappearing ? 'Disappearing messages off' : 'Disappearing messages on',
      'success',
    )
  }

  return (
    <header className="convo-head">
      <button className="icon-btn convo-back" onClick={onBack} aria-label="Back to chats">
        <ArrowLeft size={20} />
      </button>

      <span className="convo-head-avatar">
        <Avatar name={name} seed={chat.id} size={40} group={group} />
        {!group && online !== null && (
          <span className="chat-row-presence">
            <PresenceDot online={online} />
          </span>
        )}
      </span>

      <div className="convo-head-text">
        <strong>{name}</strong>
        {subtitle && <span>{subtitle}</span>}
      </div>

      <div className="convo-head-actions">
        <button
          className="icon-btn"
          data-active={disappearing || undefined}
          onClick={toggleDisappearing}
          disabled={toggling}
          aria-pressed={disappearing}
          aria-label={
            disappearing ? 'Disable disappearing messages' : 'Enable disappearing messages'
          }
          title={
            disappearing ? 'Disappearing messages on' : 'Disappearing messages off'
          }
        >
          <Timer size={20} weight={disappearing ? 'fill' : 'regular'} />
        </button>
        {group && (
          <button
            className="icon-btn"
            onClick={() => setAddingMember(true)}
            aria-label="Add member"
          >
            <UserPlus size={20} />
          </button>
        )}
      </div>

      {addingMember && (
        <AddMemberModal chatId={chat.id} onClose={() => setAddingMember(false)} />
      )}
    </header>
  )
}

function memberSummary(chat: Chat, selfId: string): string {
  const total = chatMembers(chat).length
  const peers = chatPeers(chat, selfId)
  const online = peers.filter((p) => p.online).length
  const base = `${total} member${total === 1 ? '' : 's'}`
  return online > 0 ? `${base}, ${online} online` : base
}
