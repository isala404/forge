import { useCallback, useEffect, useRef, useState } from 'react'
import { useMutation, useSubscription } from 'urql'
import { SetTypingMutation, TypingSubscription } from '../graphql/operations'
import { unmaskUser, type User } from '../lib/derive'

const TYPING_TTL_MS = 6000
const SWEEP_MS = 1000

type Typer = { user: User; expiresAt: number }

// Tracks remote typers for a chat. The backend suppresses the caller's own
// events, so anyone we see here is someone else. Each entry carries an expiry
// timestamp; a periodic sweep drops stale ones, matching the server-side kv TTL.
// Modeling expiry as data (a timestamp) instead of a Map of setTimeout handles
// keeps reset and cleanup trivial: there are no timers to track.
export function useTypingWatch(chatId: string) {
  const [active, setActive] = useState<{ chatId: string; typers: Typer[] }>({
    chatId,
    typers: [],
  })

  // Reset when the chat changes (render-time state adjustment, no effect).
  if (active.chatId !== chatId) {
    setActive({ chatId, typers: [] })
  }

  useSubscription(
    { query: TypingSubscription, variables: { chatId } },
    (_prev, value) => {
      if (!value?.typing) return value
      const user = unmaskUser(value.typing.user)
      const isTyping = value.typing.typing
      setActive((cur) => {
        if (cur.chatId !== chatId) return cur
        const rest = cur.typers.filter((t) => t.user.id !== user.id)
        if (!isTyping) return { ...cur, typers: rest }
        return {
          ...cur,
          typers: [...rest, { user, expiresAt: Date.now() + TYPING_TTL_MS }],
        }
      })
      return value
    },
  )

  useEffect(() => {
    const id = window.setInterval(() => {
      setActive((cur) => {
        const now = Date.now()
        const live = cur.typers.filter((t) => t.expiresAt > now)
        return live.length === cur.typers.length ? cur : { ...cur, typers: live }
      })
    }, SWEEP_MS)
    return () => window.clearInterval(id)
  }, [])

  return active.chatId === chatId ? active.typers.map((t) => t.user) : []
}

// Publishes the caller's typing state, throttled: send `true` at most every few
// seconds while typing, and `false` shortly after the user stops.
export function useTypingPublisher(chatId: string) {
  const [, setTyping] = useMutation(SetTypingMutation)
  const activeRef = useRef(false)
  const stopTimer = useRef<number | undefined>(undefined)

  const stop = useCallback(() => {
    if (!activeRef.current) return
    activeRef.current = false
    void setTyping({ chatId, typing: false })
  }, [chatId, setTyping])

  const onInput = useCallback(() => {
    if (!activeRef.current) {
      activeRef.current = true
      void setTyping({ chatId, typing: true })
    }
    if (stopTimer.current) window.clearTimeout(stopTimer.current)
    stopTimer.current = window.setTimeout(stop, 3000)
  }, [chatId, setTyping, stop])

  useEffect(() => {
    return () => {
      if (stopTimer.current) window.clearTimeout(stopTimer.current)
      if (activeRef.current) void setTyping({ chatId, typing: false })
      activeRef.current = false
    }
  }, [chatId, setTyping])

  return { onInput, stop }
}
