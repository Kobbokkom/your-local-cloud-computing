import { useEffect, useState, type FormEvent } from 'react'
import {
  Activity,
  Boxes,
  Check,
  CircleAlert,
  CircleHelp,
  Clock3,
  Cpu,
  LayoutDashboard,
  LockKeyhole,
  LogOut,
  Radio,
  RefreshCw,
  Server,
  ShieldCheck,
  Terminal,
  Workflow,
} from 'lucide-react'
import { Badge, Button } from './components/ui'
import {
  fetchSnapshot,
  formatMemory,
  formatTime,
  type EnvironmentStatus,
  type Job,
  type Snapshot,
  type WorkerInfo,
} from './lib/api'

type View = 'overview' | 'workers' | 'jobs' | 'environments'

const tokenKey = 'ylcc-dashboard-token'
const activeJobStates = new Set(['assigned', 'running'])

function App() {
  const [token, setToken] = useState(() => sessionStorage.getItem(tokenKey) ?? '')
  const [tokenInput, setTokenInput] = useState('')
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null)
  const [error, setError] = useState('')
  const [updatedAt, setUpdatedAt] = useState<number | null>(null)
  const [view, setView] = useState<View>('overview')
  const [refreshing, setRefreshing] = useState(false)
  const [refreshKey, setRefreshKey] = useState(0)

  useEffect(() => {
    if (!token) return
    let stopped = false
    let timer: number | undefined

    const poll = async () => {
      setRefreshing(true)
      try {
        const next = await fetchSnapshot(token)
        if (!stopped) {
          setSnapshot(next)
          setError('')
          setUpdatedAt(Date.now())
        }
      } catch (cause) {
        if (!stopped) {
          const message = cause instanceof Error ? cause.message : 'Could not reach the Hub.'
          if (message === 'AUTH_REQUIRED') {
            sessionStorage.removeItem(tokenKey)
            setToken('')
            setTokenInput('')
            setSnapshot(null)
            setError('This token was rejected. Enter the Hub token to reconnect.')
          } else {
            setError(message)
          }
        }
      } finally {
        if (!stopped) {
          setRefreshing(false)
          timer = window.setTimeout(poll, 5000)
        }
      }
    }

    void poll()
    return () => {
      stopped = true
      if (timer !== undefined) window.clearTimeout(timer)
    }
  }, [token, refreshKey])

  const workers = snapshot?.workers ?? []
  const jobs = snapshot?.jobs ?? []
  const onlineWorkers = workers.filter((worker) => worker.online)
  const activeJobs = jobs.filter((job) => activeJobStates.has(job.state))
  const queuedJobs = jobs.filter((job) => job.state === 'queued')
  const jobsByWorker = new Map<string, number>()
  for (const job of activeJobs) {
    if (job.worker_id) jobsByWorker.set(job.worker_id, (jobsByWorker.get(job.worker_id) ?? 0) + 1)
  }
  const schedulableWorkers = onlineWorkers.filter((worker) => !worker.paused)
  const totalCapacity = schedulableWorkers.reduce((sum, worker) => sum + worker.max_jobs, 0)
  const availableSlots = schedulableWorkers.reduce(
    (sum, worker) => sum + Math.max(worker.max_jobs - (jobsByWorker.get(worker.id) ?? 0), 0),
    0,
  )

  const connect = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    const next = tokenInput.trim()
    if (!next) return
    sessionStorage.setItem(tokenKey, next)
    setToken(next)
    setTokenInput('')
    setSnapshot(null)
  }

  const disconnect = () => {
    sessionStorage.removeItem(tokenKey)
    setToken('')
    setSnapshot(null)
    setError('')
  }

  if (!token) {
    return (
      <ConnectScreen
        error={error}
        token={tokenInput}
        onTokenChange={setTokenInput}
        onSubmit={connect}
      />
    )
  }

  const insecureRemote =
    window.location.protocol !== 'https:' &&
    !['localhost', '127.0.0.1', '[::1]'].includes(window.location.hostname)

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <a className="brand" href="#overview" onClick={() => setView('overview')}>
          <span className="brand-mark"><Workflow size={18} strokeWidth={2.3} /></span>
          <span className="brand-name">ylcc<span>compute pool</span></span>
        </a>
        <div className="nav-label">Workspace</div>
        <nav className="nav-list" aria-label="Dashboard views">
          <NavButton active={view === 'overview'} icon={<LayoutDashboard />} label="Overview" onClick={() => setView('overview')} />
          <NavButton active={view === 'workers'} icon={<Server />} label="Workers" count={workers.length} onClick={() => setView('workers')} />
          <NavButton active={view === 'jobs'} icon={<Activity />} label="Jobs" count={jobs.length} onClick={() => setView('jobs')} />
          <NavButton active={view === 'environments'} icon={<Boxes />} label="Environments" count={snapshot?.environments.length ?? 0} onClick={() => setView('environments')} />
        </nav>
        <div className="sidebar-bottom">
          <div className="hub-card">
            <span className="hub-card-icon"><Radio size={15} /></span>
            <div className="hub-card-copy">
              <span>Hub endpoint</span>
              <strong title={window.location.host}>{window.location.host}</strong>
            </div>
            <span className="online-dot" aria-label="Connected" />
          </div>
          <button className="account-button" onClick={disconnect}>
            <span className="account-avatar">Y</span>
            <span className="account-copy"><strong>Pool session</strong><small>Connected securely</small></span>
            <LogOut size={15} aria-hidden="true" />
          </button>
        </div>
      </aside>

      <main className="main-panel">
        <header className="topbar">
          <div className="breadcrumb"><span>YLCC</span><span className="breadcrumb-slash">/</span><strong>{viewLabel(view)}</strong></div>
          <div className="topbar-actions">
            <span className="refresh-status">
              <span className={error ? 'status-dot status-dot-error' : 'status-dot'} />
              {error ? 'Hub unreachable' : 'Live connection'}
            </span>
            <Button variant="outline" size="sm" onClick={() => setRefreshKey((key) => key + 1)} disabled={refreshing} title="Refresh dashboard">
              <RefreshCw size={14} className={refreshing ? 'spin' : ''} />
              <span>Refresh</span>
            </Button>
            <button className="icon-button help-button" title="Dashboard updates every 5 seconds" aria-label="Dashboard updates every 5 seconds">
              <CircleHelp size={17} />
            </button>
          </div>
        </header>

        <div className="page-content">
          {insecureRemote && (
            <div className="security-notice" role="status">
              <LockKeyhole size={16} />
              <span>This page is using HTTP. Use a trusted private network or HTTPS before entering a pool token.</span>
            </div>
          )}
          {error && (
            <div className="error-notice" role="alert">
              <CircleAlert size={16} />
              <span>{error}</span>
            </div>
          )}
          {!snapshot ? (
            <LoadingState />
          ) : (
            <>
              <div className="page-heading">
                <div>
                  <h1>{viewLabel(view)}</h1>
                  <p>{viewDescription(view)}</p>
                </div>
                <div className="last-updated"><Clock3 size={14} /> Updated {updatedAt ? formatTime(updatedAt / 1000) : '—'}</div>
              </div>
              {view === 'overview' && (
                <Overview
                  workers={workers}
                  jobs={jobs}
                  statuses={snapshot.environmentStatuses}
                  onlineCount={onlineWorkers.length}
                  activeCount={activeJobs.length}
                  queuedCount={queuedJobs.length}
                  availableSlots={availableSlots}
                  totalCapacity={totalCapacity}
                  jobsByWorker={jobsByWorker}
                  onNavigate={setView}
                />
              )}
              {view === 'workers' && <WorkersView workers={workers} jobsByWorker={jobsByWorker} />}
              {view === 'jobs' && <JobsView jobs={jobs} />}
              {view === 'environments' && (
                <EnvironmentsView environments={snapshot.environments} statuses={snapshot.environmentStatuses} />
              )}
            </>
          )}
          <footer className="page-footer">
            <span><ShieldCheck size={14} /> Read-only dashboard</span>
            <span>Auto-refreshes every 5 seconds</span>
          </footer>
        </div>
      </main>
    </div>
  )
}

function ConnectScreen({
  error,
  token,
  onTokenChange,
  onSubmit,
}: {
  error: string
  token: string
  onTokenChange: (value: string) => void
  onSubmit: (event: FormEvent<HTMLFormElement>) => void
}) {
  const insecureRemote =
    window.location.protocol !== 'https:' &&
    !['localhost', '127.0.0.1', '[::1]'].includes(window.location.hostname)

  return (
    <main className="connect-screen">
      <section className="connect-panel">
        <div className="connect-brand"><span className="brand-mark"><Workflow size={19} /></span><span>YLCC</span></div>
        <div className="connect-icon"><LockKeyhole size={21} /></div>
        <h1>Connect to your Hub</h1>
        <p className="connect-description">Enter the pool token configured for this Hub. Find its local file path with `ylcc dashboard`; it is never displayed by ylcc. Your dashboard reads status from this address.</p>
        <div className="endpoint-field"><span className="online-dot" />{window.location.origin}</div>
        <form onSubmit={onSubmit} className="connect-form">
          <label htmlFor="pool-token">Pool token</label>
          <input
            id="pool-token"
            type="password"
            autoComplete="off"
            autoFocus
            value={token}
            onChange={(event) => onTokenChange(event.target.value)}
            placeholder="Paste token"
            required
          />
          {error && <p className="form-error" role="alert">{error}</p>}
          {insecureRemote && <p className="form-warning"><LockKeyhole size={14} /> HTTP is not encrypted. Connect over a trusted private network or HTTPS.</p>}
          <Button className="connect-submit" type="submit" disabled={!token.trim()}>
            Connect to Hub <Check size={16} />
          </Button>
        </form>
        <div className="session-note"><ShieldCheck size={15} /> Token stays in this browser tab and is cleared when you disconnect. Copy it from your local token file; never paste it into chat or logs.</div>
      </section>
      <span className="connect-version">YLCC · self-hosted compute</span>
    </main>
  )
}

function NavButton({
  active,
  icon,
  label,
  count,
  onClick,
}: {
  active: boolean
  icon: React.ReactNode
  label: string
  count?: number
  onClick: () => void
}) {
  return (
    <button className={`nav-button${active ? ' nav-button-active' : ''}`} onClick={onClick} aria-current={active ? 'page' : undefined}>
      {icon}<span>{label}</span>{count !== undefined && <small>{count}</small>}
    </button>
  )
}

function Overview({
  workers,
  jobs,
  statuses,
  onlineCount,
  activeCount,
  queuedCount,
  availableSlots,
  totalCapacity,
  jobsByWorker,
  onNavigate,
}: {
  workers: WorkerInfo[]
  jobs: Job[]
  statuses: EnvironmentStatus[]
  onlineCount: number
  activeCount: number
  queuedCount: number
  availableSlots: number
  totalCapacity: number
  jobsByWorker: Map<string, number>
  onNavigate: (view: View) => void
}) {
  const readyStatuses = statuses.filter((status) => status.state === 'ready').length
  const recentJobs = [...jobs].sort((a, b) => b.updated_at - a.updated_at).slice(0, 5)

  return (
    <>
      <section className="metrics-strip" aria-label="Pool summary">
        <Metric icon={<Server />} label="Online workers" value={`${onlineCount} / ${workers.length}`} note={workers.length ? `${workers.length - onlineCount} offline` : 'No workers registered'} tone="blue" />
        <Metric icon={<Activity />} label="Active jobs" value={String(activeCount)} note={queuedCount ? `${queuedCount} waiting in queue` : 'Queue is clear'} tone="orange" />
        <Metric icon={<Cpu />} label="Available slots" value={String(availableSlots)} note={`${totalCapacity} on unpaused workers`} tone="green" />
        <Metric icon={<Boxes />} label="Ready environments" value={String(readyStatuses)} note={`${statuses.length} worker installs reported`} tone="slate" />
      </section>

      <section className="content-section">
        <SectionHeading title="Worker fleet" detail={`${onlineCount} online`} action="All workers" onAction={() => onNavigate('workers')} />
        {workers.length ? (
          <WorkersTable workers={workers.slice(0, 6)} jobsByWorker={jobsByWorker} />
        ) : (
          <EmptyState icon={<Server />} title="No workers connected" detail="Start a worker with this Hub address and pool token to see it here." />
        )}
      </section>

      <section className="content-section jobs-section">
        <SectionHeading title="Recent jobs" detail={`${jobs.length} total`} action="All jobs" onAction={() => onNavigate('jobs')} />
        {recentJobs.length ? <JobsTable jobs={recentJobs} /> : (
          <EmptyState icon={<Terminal />} title="No jobs yet" detail="Jobs submitted to this pool will appear here." />
        )}
      </section>
    </>
  )
}

function WorkersView({ workers, jobsByWorker }: { workers: WorkerInfo[]; jobsByWorker: Map<string, number> }) {
  return (
    <section className="content-section view-section">
      <SectionHeading title="Registered workers" detail={`${workers.filter((worker) => worker.online).length} online now`} />
      {workers.length ? <WorkersTable workers={workers} jobsByWorker={jobsByWorker} /> : (
        <EmptyState icon={<Server />} title="No workers registered" detail="Workers appear after they connect to this Hub." />
      )}
    </section>
  )
}

function JobsView({ jobs }: { jobs: Job[] }) {
  const [filter, setFilter] = useState('')
  const ordered = [...jobs]
    .filter((job) => {
      const haystack = [job.id, job.state, job.worker_id ?? '', job.spec.environment ?? '', ...job.spec.command].join(' ').toLowerCase()
      return haystack.includes(filter.toLowerCase())
    })
    .sort((a, b) => b.created_at - a.created_at)
  return (
    <section className="content-section view-section">
      <SectionHeading title="Job history" detail={`${ordered.length} of ${jobs.length} jobs`} />
      <div className="filter-row">
        <input
          className="token-input"
          type="search"
          aria-label="Filter jobs"
          placeholder="Filter by job, state, worker, command"
          value={filter}
          onChange={(event) => setFilter(event.target.value)}
        />
      </div>
      {ordered.length ? <JobsTable jobs={ordered} /> : (
        <EmptyState icon={<Terminal />} title="No matching jobs" detail="Submitted jobs and their latest states will appear here." />
      )}
    </section>
  )
}

function EnvironmentsView({
  environments,
  statuses,
}: {
  environments: Snapshot['environments']
  statuses: EnvironmentStatus[]
}) {
  return (
    <section className="content-section view-section">
      <SectionHeading title="Environment catalogue" detail={`${environments.length} definitions`} />
      {environments.length ? (
        <div className="table-scroll"><table className="data-table">
          <thead><tr><th>Environment</th><th>Revision</th><th>Image</th><th>Worker installations</th></tr></thead>
          <tbody>{environments.map((environment) => {
            const installed = statuses.filter((status) => status.environment_id === environment.id && status.revision === environment.revision)
            return <tr key={`${environment.id}-${environment.revision}`}>
              <td><strong>{environment.id}</strong></td>
              <td><code>{environment.revision}</code></td>
              <td className="truncate-cell">{environment.image ?? 'Native'}</td>
              <td><div className="install-list">{installed.length ? installed.map((status) => (
                <span key={`${status.worker_id}-${status.environment_id}`} className="install-item">
                  <StatusBadge state={status.state} /> <span>{status.worker_id}</span>
                </span>
              )) : <span className="muted-text">No worker status</span>}</div></td>
            </tr>
          })}</tbody>
        </table></div>
      ) : <EmptyState icon={<Boxes />} title="No environments configured" detail="Environment definitions will appear here when added to the Hub." />}
    </section>
  )
}

function WorkersTable({ workers, jobsByWorker }: { workers: WorkerInfo[]; jobsByWorker: Map<string, number> }) {
  return (
    <div className="table-scroll"><table className="data-table workers-table">
      <thead><tr><th>Worker</th><th>State</th><th>Platform</th><th>Resources</th><th>Jobs</th><th>Last seen</th></tr></thead>
      <tbody>{workers.map((worker) => {
        const active = jobsByWorker.get(worker.id) ?? 0
        return <tr key={worker.id}>
          <td><div className="worker-name"><span className={`worker-avatar worker-avatar-${worker.capabilities.os}`}>{osGlyph(worker.capabilities.os)}</span><strong>{worker.id}</strong></div></td>
          <td><WorkerState worker={worker} /></td>
          <td><span className="platform-name">{worker.capabilities.os}</span><span className="platform-arch">{worker.capabilities.arch}</span></td>
          <td><div className="resource-line"><Cpu size={13} /> {worker.capabilities.cpus} CPU <span className="resource-divider">·</span> {formatMemory(worker.capabilities.memory_bytes)}</div></td>
          <td><span className="capacity-value">{active}<span> / {worker.max_jobs}</span></span></td>
          <td className="time-cell">{formatTime(worker.last_seen)}</td>
        </tr>
      })}</tbody>
    </table></div>
  )
}

function JobsTable({ jobs }: { jobs: Job[] }) {
  return (
    <div className="table-scroll"><table className="data-table jobs-table">
      <thead><tr><th>Job</th><th>State</th><th>Command</th><th>Worker</th><th>Environment</th><th>Updated</th></tr></thead>
      <tbody>{jobs.map((job) => (
        <tr key={job.id}>
          <td><code className="job-id">{job.id.slice(0, 8)}</code></td>
          <td><StatusBadge state={job.state} /></td>
          <td><code className="command-cell" title={job.spec.command.join(' ')}>{job.spec.command.join(' ') || '—'}</code></td>
          <td>{job.worker_id ?? <span className="muted-text">Unassigned</span>}</td>
          <td>{job.spec.environment ?? <span className="muted-text">Native</span>}</td>
          <td className="time-cell">{formatTime(job.updated_at)}</td>
        </tr>
      ))}</tbody>
    </table></div>
  )
}

function Metric({
  icon,
  label,
  value,
  note,
  tone,
}: {
  icon: React.ReactNode
  label: string
  value: string
  note: string
  tone: string
}) {
  return (
    <div className="metric-item">
      <div className={`metric-icon metric-icon-${tone}`}>{icon}</div>
      <div className="metric-copy"><span>{label}</span><strong>{value}</strong><small>{note}</small></div>
    </div>
  )
}

function SectionHeading({
  title,
  detail,
  action,
  onAction,
}: {
  title: string
  detail: string
  action?: string
  onAction?: () => void
}) {
  return (
    <div className="section-heading">
      <div><h2>{title}</h2><span>{detail}</span></div>
      {action && <button className="text-action" onClick={onAction}>{action}<span aria-hidden="true">↗</span></button>}
    </div>
  )
}

function WorkerState({ worker }: { worker: WorkerInfo }) {
  if (!worker.online) return <Badge variant="muted"><span className="badge-dot" />Offline</Badge>
  if (worker.paused) return <Badge variant="warning"><span className="badge-dot" />Paused</Badge>
  return <Badge variant="success"><span className="badge-dot" />Online</Badge>
}

function StatusBadge({ state }: { state: string }) {
  const variant = ['succeeded', 'ready', 'complete'].includes(state) ? 'success'
    : ['running', 'assigned', 'preparing'].includes(state) ? 'default'
      : ['failed', 'unknown'].includes(state) ? 'danger'
        : ['queued', 'pending'].includes(state) ? 'warning' : 'muted'
  return <Badge variant={variant}><span className="badge-dot" />{state}</Badge>
}

function EmptyState({ icon, title, detail }: { icon: React.ReactNode; title: string; detail: string }) {
  return <div className="empty-state"><span className="empty-icon">{icon}</span><div><strong>{title}</strong><p>{detail}</p></div></div>
}

function LoadingState() {
  return <div className="loading-state"><span className="loading-indicator" /><strong>Connecting to Hub</strong><p>Reading workers, jobs, and environments</p></div>
}

function viewLabel(view: View): string {
  return { overview: 'Overview', workers: 'Workers', jobs: 'Jobs', environments: 'Environments' }[view]
}

function viewDescription(view: View): string {
  return {
    overview: 'A live view of your self-hosted compute pool.',
    workers: 'Connected machines and their available capacity.',
    jobs: 'Recent execution state across the pool.',
    environments: 'Configured environments and worker readiness.',
  }[view]
}

function osGlyph(os: string): string {
  return os === 'windows' ? 'W' : os === 'macos' ? 'M' : 'L'
}

export default App