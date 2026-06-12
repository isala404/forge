import { useState, type FormEvent } from 'react'
import { useMutation } from 'urql'
import { Plus, X } from '@phosphor-icons/react'
import { Modal } from '../ui/Modal'
import { Button } from '../ui/Button'
import { CreateChatMutation } from '../../graphql/operations'
import { unmaskChat } from '../../lib/derive'
import { errorMessage } from '../../lib/errors'

type Kind = 'DIRECT' | 'GROUP'

type Props = {
  onClose: () => void
  onCreated: (chatId: string) => void
}

export function NewChatModal({ onClose, onCreated }: Props) {
  const [, createChat] = useMutation(CreateChatMutation)
  const [kind, setKind] = useState<Kind>('DIRECT')
  const [title, setTitle] = useState('')
  const [draft, setDraft] = useState('')
  const [members, setMembers] = useState<string[]>([])
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  function addMember() {
    const name = draft.trim().replace(/^@/, '')
    if (!name) return
    if (members.includes(name)) {
      setDraft('')
      return
    }
    setMembers((m) => [...m, name])
    setDraft('')
  }

  const directReady = kind === 'DIRECT' && members.length === 1
  const groupReady =
    kind === 'GROUP' && title.trim().length > 0 && members.length >= 1
  const canSubmit = directReady || groupReady

  async function onSubmit(e: FormEvent) {
    e.preventDefault()
    if (!canSubmit) return
    setError(null)
    setBusy(true)
    const res = await createChat({
      kind,
      title: kind === 'GROUP' ? title.trim() : null,
      memberUsernames: members,
    })
    setBusy(false)
    if (res.error) {
      setError(errorMessage(res.error))
      return
    }
    if (res.data?.createChat) {
      onCreated(unmaskChat(res.data.createChat).id)
    }
  }

  return (
    <Modal
      title="New conversation"
      onClose={onClose}
      footer={
        <>
          <Button variant="subtle" onClick={onClose} type="button">
            Cancel
          </Button>
          <Button
            type="submit"
            form="new-chat-form"
            loading={busy}
            disabled={!canSubmit}
          >
            Create
          </Button>
        </>
      }
    >
      <form id="new-chat-form" onSubmit={onSubmit} className="stack">
        <div className="segmented" role="tablist" aria-label="Chat kind">
          <button
            type="button"
            role="tab"
            aria-selected={kind === 'DIRECT'}
            data-active={kind === 'DIRECT' || undefined}
            onClick={() => setKind('DIRECT')}
          >
            Direct
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={kind === 'GROUP'}
            data-active={kind === 'GROUP' || undefined}
            onClick={() => setKind('GROUP')}
          >
            Group
          </button>
        </div>

        {kind === 'GROUP' && (
          <label className="field">
            <span className="field-label">Group name</span>
            <input
              className="input"
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder="Weekend plans"
            />
          </label>
        )}

        <label className="field">
          <span className="field-label">
            {kind === 'DIRECT' ? 'Username' : 'Members'}
          </span>
          <div className="chip-input">
            <input
              className="input"
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' || e.key === ',') {
                  e.preventDefault()
                  addMember()
                }
              }}
              placeholder={kind === 'DIRECT' ? 'ada' : 'Add a username, press Enter'}
              disabled={kind === 'DIRECT' && members.length >= 1}
            />
            <button
              type="button"
              className="icon-btn"
              onClick={addMember}
              aria-label="Add member"
              disabled={kind === 'DIRECT' && members.length >= 1}
            >
              <Plus size={18} />
            </button>
          </div>
          {members.length > 0 && (
            <div className="chips">
              {members.map((m) => (
                <span className="chip" key={m}>
                  @{m}
                  <button
                    type="button"
                    onClick={() => setMembers((cur) => cur.filter((x) => x !== m))}
                    aria-label={`Remove ${m}`}
                  >
                    <X size={13} weight="bold" />
                  </button>
                </span>
              ))}
            </div>
          )}
          <span className="field-hint">
            {kind === 'DIRECT'
              ? 'You will be added automatically. Pick exactly one other person.'
              : 'You are added automatically. Add at least one more member.'}
          </span>
        </label>

        {error && (
          <p className="form-banner" role="alert">
            {error}
          </p>
        )}
      </form>
    </Modal>
  )
}
