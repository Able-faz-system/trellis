import { createServer } from 'node:http'
import { toRpcEvent } from './ingest.js'

const AGREEMENT_ID_RE = /^[0-9a-f]{64}$/
const HEARTBEAT_MS = 15_000
const MAX_LIMIT = 1000

/**
 * HTTP query API + Server-Sent Events relay.
 *
 *   GET /health                          ingest status
 *   GET /events?agreementId=<hex>        stored history, newest first
 *   GET /stats                           aggregate counts for the landing page
 *   GET /stream/events?agreementId=<hex> SSE: `snapshot`, then `events` deltas
 *   GET /stream/stats                    SSE: `stats` on connect and on change
 *
 * Every browser subscribes here instead of polling Soroban RPC itself; the
 * Ingester polls RPC once and this server fans results out.
 */
export function createIndexerServer({ store, ingester, corsOrigin, maxClients }) {
  /** @type {Set<{ res: import('node:http').ServerResponse, agreementId: string | null, stats: boolean }>} */
  const clients = new Set()

  ingester.on('events', (fresh) => {
    const byAgreement = new Map()
    for (const e of fresh) {
      if (!e.agreementId) continue
      if (!byAgreement.has(e.agreementId)) byAgreement.set(e.agreementId, [])
      byAgreement.get(e.agreementId).push(toRpcEvent(e))
    }
    const stats = store.stats()
    for (const client of clients) {
      if (client.stats) send(client.res, 'stats', stats)
      const matching = client.agreementId && byAgreement.get(client.agreementId)
      if (matching) send(client.res, 'events', { events: matching })
    }
  })

  const heartbeat = setInterval(() => {
    for (const { res } of clients) res.write(': ping\n\n')
  }, HEARTBEAT_MS)
  heartbeat.unref()

  const server = createServer((req, res) => {
    res.setHeader('Access-Control-Allow-Origin', corsOrigin)
    if (req.method === 'OPTIONS') {
      res.setHeader('Access-Control-Allow-Methods', 'GET, OPTIONS')
      res.writeHead(204).end()
      return
    }
    if (req.method !== 'GET') return json(res, 405, { error: 'method not allowed' })

    const url = new URL(req.url ?? '/', 'http://localhost')

    switch (url.pathname) {
      case '/health':
        return json(res, 200, {
          status: ingester.lastError ? 'degraded' : 'ok',
          lastPollAt: ingester.lastPollAt,
          lastError: ingester.lastError,
          rpcLatestLedger: ingester.rpcLatestLedger,
          latestIndexedLedger: store.stats().latestIndexedLedger,
          lastGap: JSON.parse(store.getMeta('last_gap') ?? 'null'),
          clients: clients.size,
        })

      case '/events': {
        const agreementId = parseAgreementId(url)
        if (!agreementId) return json(res, 400, { error: 'agreementId must be 64 hex characters' })
        const limit = Math.min(Number(url.searchParams.get('limit')) || 500, MAX_LIMIT)
        const beforeLedger = Number(url.searchParams.get('beforeLedger')) || undefined
        return json(res, 200, {
          events: store.eventsForAgreement(agreementId, { limit, beforeLedger }),
          latestLedger: ingester.rpcLatestLedger,
        })
      }

      case '/stats':
        return json(res, 200, store.stats())

      case '/stream/events': {
        const agreementId = parseAgreementId(url)
        if (!agreementId) return json(res, 400, { error: 'agreementId must be 64 hex characters' })
        if (clients.size >= maxClients) return json(res, 503, { error: 'too many subscribers' })
        const client = openStream(req, res, clients, { agreementId, stats: false })
        send(client.res, 'snapshot', { events: store.eventsForAgreement(agreementId) })
        return
      }

      case '/stream/stats': {
        if (clients.size >= maxClients) return json(res, 503, { error: 'too many subscribers' })
        const client = openStream(req, res, clients, { agreementId: null, stats: true })
        send(client.res, 'stats', store.stats())
        return
      }

      default:
        return json(res, 404, { error: 'not found' })
    }
  })

  server.on('close', () => {
    clearInterval(heartbeat)
    for (const { res } of clients) res.end()
    clients.clear()
  })

  return server
}

function parseAgreementId(url) {
  const id = (url.searchParams.get('agreementId') ?? '').toLowerCase()
  return AGREEMENT_ID_RE.test(id) ? id : null
}

function openStream(req, res, clients, opts) {
  res.writeHead(200, {
    'Content-Type': 'text/event-stream',
    'Cache-Control': 'no-cache, no-transform',
    Connection: 'keep-alive',
    // Disable proxy buffering (nginx) so events reach the browser immediately.
    'X-Accel-Buffering': 'no',
  })
  res.write('retry: 5000\n\n')
  const client = { res, ...opts }
  clients.add(client)
  req.on('close', () => clients.delete(client))
  return client
}

function send(res, event, data) {
  res.write(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`)
}

function json(res, status, body) {
  res.writeHead(status, { 'Content-Type': 'application/json' })
  res.end(JSON.stringify(body))
}
