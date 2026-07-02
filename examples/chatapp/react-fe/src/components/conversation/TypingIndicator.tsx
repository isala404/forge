import type { User } from '../../lib/derive'

export function TypingIndicator({ typers }: { typers: User[] }) {
  if (typers.length === 0) return null

  const label =
    typers.length === 1
      ? `${typers[0].displayName} is typing`
      : typers.length === 2
        ? `${typers[0].displayName} and ${typers[1].displayName} are typing`
        : `${typers.length} people are typing`

  return (
    <div className="typing-indicator" aria-live="polite">
      <span className="typing-dots" aria-hidden>
        <i />
        <i />
        <i />
      </span>
      <span className="typing-label">{label}</span>
    </div>
  )
}
