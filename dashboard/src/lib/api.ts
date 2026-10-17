export type Capabilities = {
  os: string
  arch: string
  abi: string
  tools: Record<string, string>
  gpu: string[]
  runtimes: string[]
  cpus: number
  memory_bytes: number
  containerized: boolean
}

export type WorkerInfo = {
  id: string
  capabilities: Capabilities
  max_jobs: number
  paused: boolean
  online: boolean
  last_seen: number
}

export type Job = {
  id: string
  state: string
  spec: { command: string[]; environment?: string | null }
  worker_id?: string | null
  created_at: number
  updated_at: number
}

export type Environment = {
  id: string
  revision: string
  image?: string | null
}

export type EnvironmentStatus = {
  worker_id: string
  environment_id: string
  revision: string
  state: string
  error?: string | null
}

export type Snapshot = {
  workers: WorkerInfo[]
  jobs: Job[]
  environments: Environment[]
  environmentStatuses: EnvironmentStatus[]
}

async function get<T>(path: string, token: string): Promise<T> {
  const response = await fetch(path, {
    headers: { Authorization: `Bearer ${token}` },
    cache: 'no-store',
  })
  if (!response.ok) {
    if (response.status === 401) throw new Error('AUTH_REQUIRED')
    const message = await response.text()
    throw new Error(message || `Hub returned ${response.status}`)
  }
  return response.json() as Promise<T>
}

export async function fetchSnapshot(token: string): Promise<Snapshot> {
  const [workers, jobs, environments, environmentStatuses] = await Promise.all([
    get<WorkerInfo[]>('/v1/workers', token),
    get<Job[]>('/v1/jobs', token),
    get<Environment[]>('/v1/environments', token),
    get<EnvironmentStatus[]>('/v1/environments/status', token),
  ])
  return { workers, jobs, environments, environmentStatuses }
}

export function formatTime(timestamp: number): string {
  const milliseconds = timestamp < 1_000_000_000_000 ? timestamp * 1000 : timestamp
  return new Intl.DateTimeFormat('en', {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  }).format(new Date(milliseconds))
}

export async function fetchJobLogs(token: string, jobId: string): Promise<unknown[]> {
  const response = await fetch(`/v1/jobs/${encodeURIComponent(jobId)}/logs`, {
    headers: { Authorization: `Bearer ${token}` },
    cache: 'no-store',
  })
  if (!response.ok) {
    if (response.status === 401) throw new Error('AUTH_REQUIRED')
    const message = await response.text()
    throw new Error(message || `Hub returned ${response.status}`)
  }
  return response.json() as Promise<unknown[]>
}

export function formatMemory(bytes: number): string {
  if (!bytes) return '—'
  const gibibytes = bytes / 1024 ** 3
  return `${gibibytes < 10 ? gibibytes.toFixed(1) : Math.round(gibibytes)} GiB`
}