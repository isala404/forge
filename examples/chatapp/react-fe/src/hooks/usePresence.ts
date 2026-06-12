import { useSubscription } from 'urql'
import { PresenceChangedSubscription } from '../graphql/operations'

// Subscribe to presence changes for the given user ids. The yielded User is keyed
// by id, so the normalized cache updates `online` everywhere that user appears
// (sidebar dots, chat header, member lists) without any manual reducer.
export function usePresence(userIds: string[]) {
  const sorted = [...new Set(userIds)].sort()
  useSubscription({
    query: PresenceChangedSubscription,
    variables: { userIds: sorted },
    pause: sorted.length === 0,
  })
}
