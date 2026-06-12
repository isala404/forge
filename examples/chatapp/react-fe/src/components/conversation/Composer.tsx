import { useRef, useState, type FormEvent, type KeyboardEvent } from 'react'
import { useMutation } from 'urql'
import { Paperclip, PaperPlaneRight, X } from '@phosphor-icons/react'
import { SendMessageMutation } from '../../graphql/operations'
import { useUpload } from '../../hooks/useUpload'
import { useTypingPublisher } from '../../hooks/useTyping'
import { useToast } from '../ui/toast-context'
import { errorMessage } from '../../lib/errors'

type Pending = { file: File; key: string; preview: string | null }

export function Composer({ chatId }: { chatId: string }) {
  const [, sendMessage] = useMutation(SendMessageMutation)
  const { upload, uploading } = useUpload(chatId)
  const { onInput, stop } = useTypingPublisher(chatId)
  const toast = useToast()

  const [body, setBody] = useState('')
  const [pending, setPending] = useState<Pending | null>(null)
  const [sending, setSending] = useState(false)
  const fileRef = useRef<HTMLInputElement>(null)
  const textRef = useRef<HTMLTextAreaElement>(null)
  // Stable per-attempt key so a resend after a lost response dedupes server-side.
  // Generated when a send starts, kept on error (retry reuses it), cleared on
  // success and whenever the text changes (a new/edited message is a new attempt).
  const idempotencyKey = useRef<string | null>(null)

  const canSend = (body.trim().length > 0 || pending !== null) && !sending

  async function onPickFile(file: File) {
    const res = await upload(file)
    if ('error' in res) {
      toast(res.error, 'error')
      return
    }
    const preview = file.type.startsWith('image/') ? URL.createObjectURL(file) : null
    setPending({ file, key: res.key, preview })
  }

  function clearPending() {
    if (pending?.preview) URL.revokeObjectURL(pending.preview)
    setPending(null)
  }

  async function submit(e?: FormEvent) {
    e?.preventDefault()
    if (!canSend) return
    stop()
    setSending(true)
    if (idempotencyKey.current === null) idempotencyKey.current = crypto.randomUUID()
    const res = await sendMessage({
      chatId,
      body: body.trim() || (pending ? pending.file.name : ''),
      mediaKey: pending?.key ?? null,
      idempotencyKey: idempotencyKey.current,
    })
    setSending(false)
    if (res.error) {
      toast(errorMessage(res.error), 'error')
      return
    }
    idempotencyKey.current = null
    setBody('')
    clearPending()
    textRef.current?.focus()
  }

  function onKeyDown(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      void submit()
    }
  }

  return (
    <form className="composer" onSubmit={submit}>
      {pending && (
        <div className="composer-attachment">
          {pending.preview ? (
            <img src={pending.preview} alt="" className="composer-attachment-thumb" />
          ) : (
            <span className="composer-attachment-file">{pending.file.name}</span>
          )}
          <button
            type="button"
            className="icon-btn"
            onClick={clearPending}
            aria-label="Remove attachment"
          >
            <X size={16} />
          </button>
        </div>
      )}

      <div className="composer-row">
        <input
          ref={fileRef}
          type="file"
          hidden
          onChange={(e) => {
            const file = e.target.files?.[0]
            if (file) void onPickFile(file)
            e.target.value = ''
          }}
        />
        <button
          type="button"
          className="icon-btn composer-attach"
          onClick={() => fileRef.current?.click()}
          aria-label="Attach a file"
          disabled={uploading || pending !== null}
          data-loading={uploading || undefined}
        >
          <Paperclip size={20} />
        </button>

        <textarea
          ref={textRef}
          className="composer-input"
          placeholder="Type a message"
          rows={1}
          value={body}
          onChange={(e) => {
            setBody(e.target.value)
            idempotencyKey.current = null
            onInput()
          }}
          onKeyDown={onKeyDown}
          aria-label="Message"
        />

        <button
          type="submit"
          className="composer-send"
          disabled={!canSend}
          aria-label="Send message"
          data-loading={sending || undefined}
        >
          <PaperPlaneRight size={20} weight="fill" />
        </button>
      </div>
    </form>
  )
}
