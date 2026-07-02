import type { ReactNode } from 'react'
import { Warning, ArrowClockwise } from '@phosphor-icons/react'
import { Button } from './Button'

export function EmptyState({
  icon,
  title,
  body,
  action,
}: {
  icon: ReactNode
  title: string
  body?: string
  action?: ReactNode
}) {
  return (
    <div className="state-block" role="status">
      <div className="state-icon" aria-hidden>
        {icon}
      </div>
      <h2 className="state-title">{title}</h2>
      {body && <p className="state-body">{body}</p>}
      {action}
    </div>
  )
}

export function ErrorState({
  title = 'Something went wrong',
  message,
  onRetry,
}: {
  title?: string
  message?: string
  onRetry?: () => void
}) {
  return (
    <div className="state-block" role="alert">
      <div className="state-icon state-icon-danger" aria-hidden>
        <Warning size={30} weight="duotone" />
      </div>
      <h2 className="state-title">{title}</h2>
      {message && <p className="state-body">{message}</p>}
      {onRetry && (
        <Button variant="subtle" icon={<ArrowClockwise size={16} />} onClick={onRetry}>
          Try again
        </Button>
      )}
    </div>
  )
}
