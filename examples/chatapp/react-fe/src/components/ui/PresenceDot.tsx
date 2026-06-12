type Props = {
  online: boolean
  label?: string
}

export function PresenceDot({ online, label }: Props) {
  return (
    <span
      className="presence-dot"
      data-online={online || undefined}
      role="img"
      aria-label={label ?? (online ? 'Online' : 'Offline')}
      title={online ? 'Online' : 'Offline'}
    />
  )
}
