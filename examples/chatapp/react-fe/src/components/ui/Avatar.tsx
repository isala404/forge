import { useMemo } from 'react'

const PALETTE = [
  '#3b82a8',
  '#0f9d76',
  '#b06f3a',
  '#7c5cc4',
  '#c2557a',
  '#4a8a52',
  '#a8763b',
  '#5b7fb0',
]

function pick(seed: string): string {
  let h = 0
  for (let i = 0; i < seed.length; i++) h = (h * 31 + seed.charCodeAt(i)) | 0
  return PALETTE[Math.abs(h) % PALETTE.length]
}

function initials(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean)
  if (parts.length === 0) return '?'
  if (parts.length === 1) return parts[0].slice(0, 2).toUpperCase()
  return (parts[0][0] + parts[parts.length - 1][0]).toUpperCase()
}

type Props = {
  name: string
  seed?: string
  size?: number
  group?: boolean
}

export function Avatar({ name, seed, size = 44, group = false }: Props) {
  const bg = useMemo(() => pick(seed ?? name), [seed, name])
  return (
    <span
      aria-hidden
      style={{
        width: size,
        height: size,
        background: bg,
        fontSize: size * 0.38,
        borderRadius: group ? 'var(--radius-md)' : 'var(--radius-pill)',
      }}
      className="avatar"
    >
      {initials(name)}
    </span>
  )
}
