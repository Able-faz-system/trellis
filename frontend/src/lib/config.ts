/**
 * Lazy environment configuration.
 *
 * Values are validated on first access rather than at module evaluation time,
 * so a missing env var no longer crashes the app before React mounts — the
 * error can be caught by an error boundary and shown as a friendly UI message.
 *
 * Dev-mode fallbacks (console.warn) let the app load in a non-configured
 * environment without a hard crash.
 */

const DEV_FALLBACKS: Record<string, string> = {
  VITE_CONTRACT_ID: 'CAUAO7CYKULE2K4EJMQ6LLRUHP7Y7JYOH6G2VBXKYG7PTETE3UZ3DU7Q',
  VITE_RPC_URL: 'https://soroban-testnet.stellar.org',
  VITE_NETWORK_PASSPHRASE: 'Test SDF Network ; September 2015',
}

function getEnv(name: keyof typeof DEV_FALLBACKS): string {
  const value = import.meta.env[name]

  if (typeof value === 'string' && value) {
    return value
  }

  const fallback = DEV_FALLBACKS[name]

  if (import.meta.env.DEV) {
    console.warn(
      `[config] ${name} is not set. Using dev fallback: "${fallback}". ` +
        'Copy .env.example to .env and fill in real values.',
    )
    return fallback
  }

  throw new Error(
    `Environment variable ${name} is not set. Copy .env.example to .env and fill in values.`,
  )
}

// Cached values — validated once per session, not on every call.
let _contractId: string | undefined
let _rpcUrl: string | undefined
let _networkPassphrase: string | undefined

export function getContractId(): string {
  return (_contractId ??= getEnv('VITE_CONTRACT_ID'))
}

export function getRpcUrl(): string {
  return (_rpcUrl ??= getEnv('VITE_RPC_URL'))
}

export function getNetworkPassphrase(): string {
  return (_networkPassphrase ??= getEnv('VITE_NETWORK_PASSPHRASE'))
}

/**
 * Base URL of the optional Trellis indexer/relay service (see indexer/ and
 * DEPLOYMENT.md). Unlike the variables above this one is optional: when it is
 * unset the event hooks poll Soroban RPC directly, exactly as before.
 */
export function getIndexerUrl(): string | null {
  const value = import.meta.env.VITE_INDEXER_URL
  return typeof value === 'string' && value ? value.replace(/\/+$/, '') : null
}

/**
 * Named re-exports kept for backwards compatibility with existing consumers.
 * The module-level constants are now derived via the lazy getters, so they
 * are still evaluated lazily on the first import rather than unconditionally
 * at bundle evaluation time.
 *
 * @deprecated Prefer the explicit getter functions (getContractId, getRpcUrl,
 *   getNetworkPassphrase) at new call sites.
 */
export const CONTRACT_ID = getContractId()
export const RPC_URL = getRpcUrl()
export const NETWORK_PASSPHRASE = getNetworkPassphrase()
