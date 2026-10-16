import { useCallback, useEffect, useRef, useState } from 'react'
import { xdr } from '@stellar/stellar-sdk'
import { CONTRACT_ID, RPC_URL, getIndexerUrl } from '../lib/config'
import { openIndexerStream } from '../lib/indexer'

export interface ContractStats {
  agreements: number
  milestonesLocked: number
}

export type StatsStatus =
  | 'loading'
  | 'ok'
  | 'stale'
  | 'error'

export interface UseContractStatsResult {
  stats: ContractStats | null
  status: StatsStatus
  lastUpdated: string | null
}

function encodeTopicFilter(symbol: string): string {
  const scVal = xdr.ScVal.scvSymbol(symbol)
  return scVal.toXDR('base64')
}

const PLACEHOLDER_PATTERNS = [/^your[_-]/i, /^changeme$/i, /^placeholder$/i, /^xxx+$/i, /^example$/i, /^unset/i]

function isPlaceholderValue(value: string): boolean {
  return PLACEHOLDER_PATTERNS.some((pattern) => pattern.test(value.trim()))
}

function warnIfPlaceholder(name: string, value: string): void {
  if (isPlaceholderValue(value)) {
    console.warn(`[useContractStats] ${name} looks like a placeholder value: "${value}"`)
  }
}

interface RpcEventFilter {
  type: 'contract'
  contractIds: string[]
  topics: string[][]
}

interface RpcGetEventsRequest {
  jsonrpc: '2.0'
  id: number
  method: 'getEvents'
  params: {
    startLedger: number
    filters: RpcEventFilter[]
    pagination?: { limit: number }
  }
}

interface RpcGetEventsResponse {
  jsonrpc: string
  id: number
  result?: {
    events: unknown[]
    latestLedger: number
  }
  error?: {
    code: number
    message: string
  }
}

async function fetchEventCount(topicSymbol: string, signal: AbortSignal): Promise<number> {
  const topicXdr = encodeTopicFilter(topicSymbol)

  const body: RpcGetEventsRequest = {
    jsonrpc: '2.0',
    id: 1,
    method: 'getEvents',
    params: {
      startLedger: 1,
      filters: [
        {
          type: 'contract',
          contractIds: [CONTRACT_ID],
          topics: [[topicXdr]],
        },
      ],
      pagination: { limit: 200 },
    },
  }

  const response = await fetch(RPC_URL, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
    signal,
  })

  if (!response.ok) {
    throw new Error(`RPC HTTP ${response.status}: ${response.statusText}`)
  }

  const json: RpcGetEventsResponse = await response.json()

  if (json.error) {
    throw new Error(`RPC error ${json.error.code}: ${json.error.message}`)
  }

  const count = json.result?.events.length ?? 0

  if (count === 0) {
    console.warn(
      `[useContractStats] No "${topicSymbol}" events returned by RPC. ` +
        'This may indicate the node has pruned history or the topic filter is wrong.',
    )
  }

  return count
}

const POLL_INTERVAL_MS = 60_000

function isContractStats(data: unknown): data is ContractStats {
  const obj = data as Partial<ContractStats> | null
  return typeof obj?.agreements === 'number' && typeof obj.milestonesLocked === 'number'
}

/**
 * Landing-page contract statistics.
 *
 * With VITE_INDEXER_URL set, stats are pushed by the indexer relay whenever
 * new events are indexed — counted over the indexer's full history rather
 * than whatever the RPC node still retains. Without it (or if the relay is
 * unreachable), each tab polls RPC every POLL_INTERVAL_MS while visible.
 */
export function useContractStats(): UseContractStatsResult {
  const [stats, setStats] = useState<ContractStats | null>(null)
  const [status, setStatus] = useState<StatsStatus>('loading')
  const [lastUpdated, setLastUpdated] = useState<string | null>(null)

  const statsRef = useRef<ContractStats | null>(null)
  // Bumped on every fetch kickoff so a request superseded by a newer one
  // (e.g. the immediate refetch on tab-visible) can recognize it's stale
  // and skip applying its result, even if it resolves after the newer one.
  const requestIdRef = useRef(0)

  const fetchStats = useCallback(async (signal: AbortSignal) => {
    const requestId = ++requestIdRef.current

    try {
      const [agreements, milestonesLocked] = await Promise.all([
        fetchEventCount('created', signal),
        fetchEventCount('locked', signal),
      ])

      if (signal.aborted || requestId !== requestIdRef.current) return

      const next: ContractStats = { agreements, milestonesLocked }
      statsRef.current = next
      setStats(next)
      setStatus('ok')
      setLastUpdated(new Date().toISOString())
    } catch (err) {
      if (signal.aborted || requestId !== requestIdRef.current) return

      console.error('[useContractStats] Fetch failed:', err)

      setStatus(statsRef.current !== null ? 'stale' : 'error')
    }
  }, [])

  useEffect(() => {
    if (!CONTRACT_ID || !RPC_URL) {
      console.error(
        '[useContractStats] CONTRACT_ID or RPC_URL is not set. ' +
          'Check VITE_CONTRACT_ID and VITE_RPC_URL in your .env file.',
      )
      setStatus('error')
      return
    }
    warnIfPlaceholder('CONTRACT_ID', CONTRACT_ID)
    warnIfPlaceholder('RPC_URL', RPC_URL)

    const controller = new AbortController()
    let interval: ReturnType<typeof setInterval> | null = null
    let closeStream: (() => void) | null = null
    let polling = false

    const startInterval = () => {
      if (interval !== null) return
      interval = setInterval(() => fetchStats(controller.signal), POLL_INTERVAL_MS)
    }

    const stopInterval = () => {
      if (interval === null) return
      clearInterval(interval)
      interval = null
    }

    const handleVisibilityChange = () => {
      if (document.hidden) {
        stopInterval()
      } else {
        fetchStats(controller.signal)
        startInterval()
      }
    }

    const startPolling = () => {
      if (controller.signal.aborted || polling) return
      polling = true
      fetchStats(controller.signal)
      startInterval()
      document.addEventListener('visibilitychange', handleVisibilityChange)
    }

    if (getIndexerUrl()) {
      closeStream = openIndexerStream(
        '/stream/stats',
        {
          stats: (data) => {
            if (controller.signal.aborted || polling || !isContractStats(data)) return
            const next: ContractStats = {
              agreements: data.agreements,
              milestonesLocked: data.milestonesLocked,
            }
            statsRef.current = next
            setStats(next)
            setStatus('ok')
            setLastUpdated(new Date().toISOString())
          },
        },
        startPolling,
      )
    } else {
      startPolling()
    }

    return () => {
      controller.abort()
      closeStream?.()
      stopInterval()
      document.removeEventListener('visibilitychange', handleVisibilityChange)
    }
  }, [fetchStats])

  return { stats, status, lastUpdated }
}
