import { useEffect } from 'react'
import { useMutation } from 'urql'
import { HeartbeatMutation } from '../graphql/operations'
import {
  PRESENCE_HEARTBEAT_JITTER_MS,
  PRESENCE_HEARTBEAT_MS,
} from '../lib/config'

// Keep the user's presence fresh while the tab is open. The backend kv lapses after
// a TTL, so we beat at <= TTL/2 to survive one dropped beat, retry a failed beat
// once before the next interval, and beat immediately whenever the tab becomes
// visible or the network comes back so a backgrounded/offline tab is never shown
// stale-offline longer than necessary.
export function useHeartbeat(active: boolean) {
  const [, heartbeat] = useMutation(HeartbeatMutation)

  useEffect(() => {
    if (!active) return
    let cancelled = false

    const beat = async () => {
      if (cancelled) return
      const res = await heartbeat({})
      // Retry once on a transient failure; a second failure just waits for the
      // next scheduled beat rather than spinning.
      if (!cancelled && res.error) await heartbeat({})
    }

    // Spread beats across tabs so they don't synchronize into a herd.
    const next = () =>
      PRESENCE_HEARTBEAT_MS + Math.random() * PRESENCE_HEARTBEAT_JITTER_MS

    let timer = 0
    const schedule = () => {
      timer = window.setTimeout(() => {
        void beat()
        schedule()
      }, next())
    }

    // Beat now if the tab is foregrounded and online; an offline/hidden mount waits
    // for the visibility/online listeners below to drive the first beat.
    const beatNow = () => {
      if (!document.hidden && navigator.onLine) void beat()
    }

    const onVisibility = () => {
      if (!document.hidden) beatNow()
    }

    beatNow()
    schedule()
    document.addEventListener('visibilitychange', onVisibility)
    window.addEventListener('online', beatNow)
    window.addEventListener('focus', beatNow)

    return () => {
      cancelled = true
      window.clearTimeout(timer)
      document.removeEventListener('visibilitychange', onVisibility)
      window.removeEventListener('online', beatNow)
      window.removeEventListener('focus', beatNow)
    }
  }, [active, heartbeat])
}
