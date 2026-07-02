import { FileArrowDown } from '@phosphor-icons/react'
import { StatusTicks } from '../ui/StatusTicks'
import { formatTime } from '../../lib/format'
import {
  deliveryState,
  unmaskMedia,
  unmaskUser,
  type Message,
  type Receipt,
} from '../../lib/derive'

type Props = {
  message: Message
  receipts: Receipt[]
  selfId: string
  showSender: boolean
}

function isImage(contentType: string | null | undefined): boolean {
  return Boolean(contentType && contentType.startsWith('image/'))
}

export function MessageBubble({ message, receipts, selfId, showSender }: Props) {
  const sender = unmaskUser(message.sender)
  const mine = sender.id === selfId
  const media = message.media ? unmaskMedia(message.media) : null

  return (
    <div className="bubble-row" data-mine={mine || undefined}>
      <div className="bubble" data-mine={mine || undefined}>
        {showSender && !mine && (
          <span className="bubble-sender">{sender.displayName}</span>
        )}

        {media && (
          <a
            className="bubble-media"
            href={media.downloadUrl}
            target="_blank"
            rel="noreferrer"
            data-image={isImage(media.contentType) || undefined}
          >
            {isImage(media.contentType) ? (
              <img src={media.downloadUrl} alt={message.body || 'Attachment'} loading="lazy" />
            ) : (
              <span className="bubble-file">
                <FileArrowDown size={22} weight="duotone" />
                <span className="bubble-file-name">
                  {media.contentType ?? 'File'}
                </span>
              </span>
            )}
          </a>
        )}

        {message.body && <span className="bubble-body">{message.body}</span>}

        <span className="bubble-meta">
          <time dateTime={message.createdAt}>{formatTime(message.createdAt)}</time>
          {mine && <StatusTicks state={deliveryState(receipts, selfId)} />}
        </span>
      </div>
    </div>
  )
}
