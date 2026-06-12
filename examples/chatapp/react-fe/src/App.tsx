import { useCallback, useState } from 'react'
import { ChatCircleDots } from '@phosphor-icons/react'
import { AuthScreen } from './components/auth/AuthScreen'
import { Sidebar } from './components/sidebar/Sidebar'
import { NewChatModal } from './components/sidebar/NewChatModal'
import { Conversation } from './components/conversation/Conversation'
import { Settings } from './components/settings/Settings'
import { EmptyState } from './components/ui/States'
import { ToastProvider } from './components/ui/Toast'
import { useAuthActions, useSession } from './hooks/useSession'
import { useHeartbeat } from './hooks/useHeartbeat'
import { usePresence } from './hooks/usePresence'
import type { User } from './lib/derive'

type View = 'chat' | 'settings'

export function App() {
  const { token, user, loadingMe } = useSession()
  useHeartbeat(Boolean(user))

  if (!token) return <AuthScreen />

  if (loadingMe && !user) {
    return (
      <div className="boot">
        <span className="boot-spinner" aria-hidden />
        <span>Connecting</span>
      </div>
    )
  }

  if (!user) return <AuthScreen />

  return (
    <ToastProvider>
      <Shell self={user} />
    </ToastProvider>
  )
}

function Shell({ self }: { self: User }) {
  const selfId = self.id
  const { logout, logoutAll } = useAuthActions()
  const [activeChatId, setActiveChatId] = useState<string | null>(null)
  const [view, setView] = useState<View>('chat')
  const [newChatOpen, setNewChatOpen] = useState(false)
  const [presenceIds, setPresenceIds] = useState<string[]>([])

  usePresence(presenceIds)

  const onChatsLoaded = useCallback((ids: string[]) => {
    setPresenceIds((prev) => {
      if (prev.length === ids.length && prev.every((id, i) => id === ids[i])) {
        return prev
      }
      return ids
    })
  }, [])

  // Drive a data-pane attribute so mobile can show one pane at a time.
  const pane = view === 'settings' ? 'settings' : activeChatId ? 'detail' : 'list'

  return (
    <div className="app" data-pane={pane}>
      <Sidebar
        self={self}
        activeChatId={activeChatId}
        onSelect={(id) => {
          setActiveChatId(id)
          setView('chat')
        }}
        onNewChat={() => setNewChatOpen(true)}
        onSettings={() => setView('settings')}
        onLogout={logout}
        onChatsLoaded={onChatsLoaded}
      />

      <main className="app-main">
        {view === 'settings' ? (
          <Settings
            onBack={() => setView('chat')}
            onLogoutAll={logoutAll}
          />
        ) : activeChatId ? (
          <Conversation
            chatId={activeChatId}
            selfId={selfId}
            onBack={() => setActiveChatId(null)}
          />
        ) : (
          <div className="convo-placeholder">
            <EmptyState
              icon={<ChatCircleDots size={40} weight="duotone" />}
              title="Pick a conversation"
              body="Choose a chat from the list, or start a new one to begin messaging."
            />
          </div>
        )}
      </main>

      {newChatOpen && (
        <NewChatModal
          onClose={() => setNewChatOpen(false)}
          onCreated={(id) => {
            setNewChatOpen(false)
            setActiveChatId(id)
            setView('chat')
          }}
        />
      )}
    </div>
  )
}
