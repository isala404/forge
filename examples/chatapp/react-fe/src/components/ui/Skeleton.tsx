type Props = {
  width?: number | string
  height?: number | string
  radius?: string
  className?: string
}

export function Skeleton({
  width = '100%',
  height = 14,
  radius = 'var(--radius-sm)',
  className = '',
}: Props) {
  return (
    <span
      className={`skeleton ${className}`}
      style={{ width, height, borderRadius: radius }}
      aria-hidden
    />
  )
}

export function ChatRowSkeleton() {
  return (
    <div className="chat-row chat-row-skeleton">
      <Skeleton width={44} height={44} radius="var(--radius-pill)" />
      <div className="chat-row-body">
        <Skeleton width="55%" height={13} />
        <Skeleton width="75%" height={11} />
      </div>
    </div>
  )
}

export function MessageSkeleton({ mine = false }: { mine?: boolean }) {
  return (
    <div className="bubble-row" data-mine={mine || undefined}>
      <Skeleton
        width={mine ? 180 : 220}
        height={38}
        radius="var(--radius-lg)"
      />
    </div>
  )
}
