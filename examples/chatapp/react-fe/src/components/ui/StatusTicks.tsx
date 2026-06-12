import { Check, Checks } from '@phosphor-icons/react'

export type DeliveryState = 'sent' | 'delivered' | 'read'

const LABEL: Record<DeliveryState, string> = {
  sent: 'Sent',
  delivered: 'Delivered',
  read: 'Read',
}

export function StatusTicks({ state }: { state: DeliveryState }) {
  const Icon = state === 'sent' ? Check : Checks
  return (
    <span
      className="status-ticks"
      data-read={state === 'read' || undefined}
      role="img"
      aria-label={LABEL[state]}
      title={LABEL[state]}
    >
      <Icon size={16} weight="bold" />
    </span>
  )
}
