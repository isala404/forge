import { useState, type FormEvent } from 'react'
import { useMutation } from 'urql'
import { Modal } from '../ui/Modal'
import { Button } from '../ui/Button'
import { AddMemberMutation } from '../../graphql/operations'
import { errorMessage } from '../../lib/errors'
import { useToast } from '../ui/toast-context'

export function AddMemberModal({
  chatId,
  onClose,
}: {
  chatId: string
  onClose: () => void
}) {
  const [, addMember] = useMutation(AddMemberMutation)
  const toast = useToast()
  const [username, setUsername] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function onSubmit(e: FormEvent) {
    e.preventDefault()
    const name = username.trim().replace(/^@/, '')
    if (!name) return
    setBusy(true)
    setError(null)
    const res = await addMember({ chatId, username: name })
    setBusy(false)
    if (res.error) {
      setError(errorMessage(res.error))
      return
    }
    toast(`Added @${name}`, 'success')
    onClose()
  }

  return (
    <Modal
      title="Add member"
      onClose={onClose}
      footer={
        <>
          <Button variant="subtle" type="button" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" form="add-member-form" loading={busy} disabled={!username.trim()}>
            Add
          </Button>
        </>
      }
    >
      <form id="add-member-form" onSubmit={onSubmit} className="stack">
        <label className="field">
          <span className="field-label">Username</span>
          <input
            className="input"
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            placeholder="grace"
            autoFocus
          />
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
