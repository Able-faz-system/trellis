import { EventStore } from './store.js'
import { Ingester } from './ingest.js'
import { createIndexerServer } from './server.js'

function env(name, fallback) {
  const value = process.env[name]
  if (value) return value
  if (fallback !== undefined) return fallback
  console.error(`[indexer] ${name} is required (see indexer/.env.example)`)
  process.exit(1)
}

const config = {
  rpcUrl: env('RPC_URL'),
  contractId: env('CONTRACT_ID'),
  dbPath: env('DB_PATH', './data/trellis-events.db'),
  port: Number(env('PORT', '8787')),
  pollIntervalMs: Number(env('POLL_INTERVAL_MS', '5000')),
  corsOrigin: env('CORS_ORIGIN', '*'),
  maxClients: Number(env('MAX_SSE_CLIENTS', '1000')),
  startLedger: process.env.START_LEDGER,
}

const store = new EventStore(config.dbPath)

// One database per contract: refuse to mix events from a different contract
// into an existing file (e.g. after redeploying and forgetting to change DB_PATH).
const storedContract = store.getMeta('contract_id')
if (storedContract && storedContract !== config.contractId) {
  console.error(
    `[indexer] ${config.dbPath} indexes ${storedContract}, not ${config.contractId}. ` +
      'Point DB_PATH at a new file when the contract changes.',
  )
  process.exit(1)
}
store.setMeta('contract_id', config.contractId)
if (config.startLedger && !store.getMeta('cursor')) {
  store.setMeta('start_ledger', config.startLedger)
}

const ingester = new Ingester({
  store,
  rpcUrl: config.rpcUrl,
  contractId: config.contractId,
  pollIntervalMs: config.pollIntervalMs,
})
ingester.on('gap', (gap) => console.warn('[indexer] retention gap detected:', gap))

const server = createIndexerServer({
  store,
  ingester,
  corsOrigin: config.corsOrigin,
  maxClients: config.maxClients,
})

server.listen(config.port, () => {
  console.log(`[indexer] indexing ${config.contractId} from ${config.rpcUrl}`)
  console.log(`[indexer] listening on :${config.port}`)
  ingester.start()
})

for (const signal of ['SIGINT', 'SIGTERM']) {
  process.on(signal, () => {
    ingester.stop()
    server.close(() => process.exit(0))
    server.closeAllConnections()
  })
}
