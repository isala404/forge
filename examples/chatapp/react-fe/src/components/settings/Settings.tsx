import { useState } from 'react'
import { useMutation, useQuery } from 'urql'
import {
  ArrowLeft,
  Copy,
  Key,
  Pulse,
  SignOut,
  Stack,
  WarningOctagon,
} from '@phosphor-icons/react'
import {
  CreateApiKeyMutation,
  OpsStatsQuery,
  ReactionsEnabledQuery,
  SetReactionsRolloutMutation,
  TriggerFailingJobMutation,
} from '../../graphql/operations'
import { Button } from '../ui/Button'
import { Modal } from '../ui/Modal'
import { Skeleton } from '../ui/Skeleton'
import { useToast } from '../ui/toast-context'
import { errorMessage } from '../../lib/errors'

type Props = {
  onBack: () => void
  onLogoutAll: () => void
}

export function Settings({ onBack, onLogoutAll }: Props) {
  const toast = useToast()
  const [{ data, fetching, error }, refetchOps] = useQuery({
    query: OpsStatsQuery,
    requestPolicy: 'cache-and-network',
  })
  const [{ data: reactions }, refetchReactions] = useQuery({
    query: ReactionsEnabledQuery,
    requestPolicy: 'cache-and-network',
  })
  const reactionsEnabled = reactions?.reactionsEnabled ?? false
  const [, createApiKey] = useMutation(CreateApiKeyMutation)
  const [, setRollout] = useMutation(SetReactionsRolloutMutation)
  const [, triggerFail] = useMutation(TriggerFailingJobMutation)

  const [percent, setPercent] = useState(0)
  const [label, setLabel] = useState('')
  const [mintedSecret, setMintedSecret] = useState<string | null>(null)
  const [minting, setMinting] = useState(false)

  async function mint() {
    if (!label.trim()) return
    setMinting(true)
    const res = await createApiKey({ label: label.trim() })
    setMinting(false)
    if (res.error) {
      toast(errorMessage(res.error), 'error')
      return
    }
    if (res.data?.createApiKey) {
      setMintedSecret(res.data.createApiKey.secret)
      setLabel('')
    }
  }

  async function saveRollout() {
    const res = await setRollout({ percent })
    if (res.error) toast(errorMessage(res.error), 'error')
    else {
      toast(`Reactions rollout set to ${percent}%`, 'success')
      refetchReactions({ requestPolicy: 'network-only' })
    }
  }

  async function fireFailingJob() {
    const res = await triggerFail({})
    if (res.error) toast(errorMessage(res.error), 'error')
    else {
      toast('Failing job enqueued. It will land in the DLQ.', 'info')
      window.setTimeout(() => refetchOps({ requestPolicy: 'network-only' }), 1500)
    }
  }

  return (
    <section className="settings">
      <header className="settings-head">
        <button className="icon-btn" onClick={onBack} aria-label="Back">
          <ArrowLeft size={20} />
        </button>
        <h1>Settings</h1>
      </header>

      <div className="settings-scroll">
        <section className="panel">
          <div className="panel-head">
            <Pulse size={20} weight="duotone" />
            <div>
              <h2>Operations</h2>
              <p>Live gauges from the backend.</p>
            </div>
            <Button
              variant="subtle"
              onClick={() => refetchOps({ requestPolicy: 'network-only' })}
            >
              Refresh
            </Button>
          </div>

          <div className="gauge-grid">
            <div className="gauge">
              <span className="gauge-label">Online now</span>
              {fetching && !data ? (
                <Skeleton width={56} height={30} />
              ) : error ? (
                <span className="gauge-value gauge-error">--</span>
              ) : (
                <span className="gauge-value">{data?.opsStats.onlineCount ?? 0}</span>
              )}
            </div>
            <div className="gauge">
              <span className="gauge-label">DLQ depth</span>
              {fetching && !data ? (
                <Skeleton width={56} height={30} />
              ) : error ? (
                <span className="gauge-value gauge-error">--</span>
              ) : (
                <span className="gauge-value" data-warn={(data?.opsStats.dlqCount ?? 0) > 0 || undefined}>
                  {data?.opsStats.dlqCount ?? 0}
                </span>
              )}
            </div>
          </div>

          {error && <p className="panel-error">{errorMessage(error)}</p>}

          <Button
            variant="subtle"
            icon={<WarningOctagon size={17} />}
            onClick={fireFailingJob}
          >
            Trigger failing job
          </Button>
        </section>

        <section className="panel">
          <div className="panel-head">
            <Stack size={20} weight="duotone" />
            <div>
              <h2>Reactions rollout</h2>
              <p>Feature-flag percentage for reactions_v2.</p>
            </div>
            <span className="flag-pill" data-on={reactionsEnabled || undefined}>
              {reactionsEnabled ? 'On for you' : 'Off for you'}
            </span>
          </div>
          <div className="slider-row">
            <input
              type="range"
              min={0}
              max={100}
              value={percent}
              onChange={(e) => setPercent(Number(e.target.value))}
              aria-label="Reactions rollout percent"
            />
            <span className="slider-value">{percent}%</span>
            <Button variant="subtle" onClick={saveRollout}>
              Save
            </Button>
          </div>
        </section>

        <section className="panel">
          <div className="panel-head">
            <Key size={20} weight="duotone" />
            <div>
              <h2>API keys</h2>
              <p>Mint a personal key. The secret is shown once.</p>
            </div>
          </div>
          <div className="inline-form">
            <input
              className="input"
              value={label}
              onChange={(e) => setLabel(e.target.value)}
              placeholder="Key label, e.g. cli"
              aria-label="API key label"
            />
            <Button onClick={mint} loading={minting} disabled={!label.trim()}>
              Mint key
            </Button>
          </div>
        </section>

        <section className="panel panel-danger">
          <div className="panel-head">
            <SignOut size={20} weight="duotone" />
            <div>
              <h2>Sessions</h2>
              <p>Sign out everywhere, revoking every active session.</p>
            </div>
          </div>
          <Button variant="danger" onClick={onLogoutAll}>
            Sign out of all sessions
          </Button>
        </section>
      </div>

      {mintedSecret && (
        <Modal
          title="API key created"
          onClose={() => setMintedSecret(null)}
          footer={
            <Button onClick={() => setMintedSecret(null)}>Done</Button>
          }
        >
          <p className="modal-note">
            Copy this secret now. It will not be shown again.
          </p>
          <div className="secret-box">
            <code>{mintedSecret}</code>
            <button
              className="icon-btn"
              onClick={() => {
                void navigator.clipboard.writeText(mintedSecret)
                toast('Copied to clipboard', 'success')
              }}
              aria-label="Copy secret"
            >
              <Copy size={18} />
            </button>
          </div>
        </Modal>
      )}
    </section>
  )
}
