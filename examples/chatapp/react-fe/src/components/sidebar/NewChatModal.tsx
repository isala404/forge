import { useState, type FormEvent } from 'react'
import { Plus, X } from '@phosphor-icons/react'
import { Modal } from '../ui/Modal'
import { Button } from '../ui/Button'
import { useMutation } from 'urql'
import { CreateChatMutation } from '../../graphql/operations'
import { unmaskChat } from '../../lib/derive'
import { errorMessage } from '../../lib/errors'

type Props = {
  onClose: () => void
  onCreated: (chatId: string) => void
}

// A conversation is just a set of people. One other person is a direct chat; two
// or more is a group, and only then does an (optional) group name make sense. The
// kind is inferred from the member count rather than chosen up front, so there is
// no Direct/Group switch to get wrong. A blank group name is fine: the sidebar
// derives a title from the members when none is set.
export function NewChatModal({ onClose, onCreated }: Props) {
  const [, createChat] = useMutation(CreateChatMutation)
  const [title, setTitle] = useState('')
  const [draft, setDraft] = useState('')
  const [members, setMembers] = useState<string[]>([])
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const isGroup = members.length >= 2

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

  const canSubmit = members.length >= 1

  async function onSubmit(e: FormEvent) {
    e.preventDefault()
    if (!canSubmit) return
    setError(null)
    setBusy(true)
    const res = await createChat({
      kind: isGroup ? 'GROUP' : 'DIRECT',
      title: isGroup && title.trim() ? title.trim() : null,
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
        <label className="field">
          <span className="field-label">Members</span>
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
              placeholder="Add a username, press Enter"
            />
            <button
              type="button"
              className="icon-btn"
              onClick={addMember}
              aria-label="Add member"
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
            You are added automatically. Add one person for a direct chat, or more
            for a group.
          </span>
        </label>

        {isGroup && (
          <label className="field">
            <span className="field-label">Group name (optional)</span>
            <input
              className="input"
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder="Weekend plans"
            />
          </label>
        )}

        {error && (
          <p className="form-banner" role="alert">
            {error}
          </p>
        )}
      </form>
    </Modal>
  )
}
