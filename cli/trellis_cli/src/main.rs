mod commands;
mod config;
mod history;
mod input;
mod rpc;
mod sanitizer;
mod utils;

use clap::{CommandFactory, Parser};
use commands::{Commands, OutputFormat, OutputOpts};
use config::{Network, Role};
use std::process;

// ---------------------------------------------------------------------------
// Top-level CLI definition
// ---------------------------------------------------------------------------

/// Trellis Protocol CLI — milestone-based escrow on Stellar Soroban.
///
/// Configuration can be loaded from a `.env` file in the working directory or
/// set as environment variables.  CLI flags take highest priority, then env
/// vars, then the `.env` file.
///
/// Required variables:
///   TRELLIS_CONTRACT_ID        — Bech32 contract address (`C…`)
///   TRELLIS_SOURCE_KEY         — Stellar secret key or named identity
///                                (or TRELLIS_SOURCE_KEY_FILE / --source-key-file,
///                                which take precedence)
///
/// Per-role identities, selected with `--as payer|payee|resolver`:
///   TRELLIS_PAYER_KEY / TRELLIS_PAYEE_KEY / TRELLIS_RESOLVER_KEY
///   (or the matching *_KEY_FILE variables)
///
/// Optional variables (Soroban Testnet used as default):
///   STELLAR_RPC_URL            — Soroban JSON-RPC endpoint
///   STELLAR_NETWORK_PASSPHRASE — Network passphrase for transaction signing
///   STELLAR_RPC_RETRIES        — Retries for transient RPC failures (default 3; 0 disables)
#[derive(Parser, Debug)]
#[command(
    name = "trellis",
    version = env!("CARGO_PKG_VERSION"),
    // `--version` (long form) shows this extended string; `-V` (short form)
    // still shows the bare crate version above. Includes the on-chain
    // contract's soroban-sdk compatibility range so bug reports carry
    // enough environment context without a separate lookup.
    long_version = concat!(
        env!("CARGO_PKG_VERSION"),
        "\nsoroban-sdk compat: >=22.0.0, <23 (see contracts/trellis_core/Cargo.toml)",
    ),
    author,
    about,
    long_about = None,
    propagate_version = true,
)]
struct Cli {
    /// Network preset to connect to. `custom` requires `--rpc-url` and
    /// `--network-passphrase`.
    #[arg(long, global = true, value_enum, default_value_t = Network::Testnet)]
    network: Network,

    /// Custom Soroban RPC endpoint. Required when `--network=custom`;
    /// overrides the preset / env var for any other network.
    #[arg(long, global = true)]
    rpc_url: Option<String>,

    /// Custom network passphrase. Required when `--network=custom`;
    /// overrides the preset / env var for any other network.
    #[arg(long, global = true)]
    network_passphrase: Option<String>,

    /// Read the source key from this file instead of the `TRELLIS_SOURCE_KEY`
    /// environment variable. Preferred for raw `S…` secret seeds: file
    /// contents never appear in `ps` / `/proc` the way an argv or exported
    /// env var can (#240). Precedence: this flag > `TRELLIS_SOURCE_KEY_FILE`
    /// > `TRELLIS_SOURCE_KEY`. Warns if the file is group/world-accessible.
    #[arg(long, global = true, value_name = "PATH")]
    source_key_file: Option<String>,

    /// Sign as this escrow role, using its own key from `TRELLIS_PAYER_KEY`,
    /// `TRELLIS_PAYEE_KEY` or `TRELLIS_RESOLVER_KEY` (or the matching
    /// `*_KEY_FILE` variable, which takes precedence) instead of
    /// `TRELLIS_SOURCE_KEY`. Fails if the role's key is not configured.
    #[arg(
        long = "as",
        global = true,
        value_enum,
        value_name = "ROLE",
        conflicts_with = "source_key_file"
    )]
    as_role: Option<Role>,

    #[command(subcommand)]
    command: Commands,

    /// Output a uniform, machine-parseable JSON envelope instead of raw
    /// command output. Takes priority over `--human-readable`.
    #[arg(long, global = true)]
    json: bool,

    /// Parse the underlying stellar CLI output into a human-friendly,
    /// colorized summary. Falls back to raw output if parsing fails.
    #[arg(short = 'H', long = "human-readable", global = true)]
    human_readable: bool,

    /// Suppress all output except the final JSON result. Implies `--json`.
    #[arg(long, global = true)]
    quiet: bool,

    /// Print the `stellar contract invoke` command that would run, without
    /// executing it or submitting anything on-chain.
    #[arg(long, global = true)]
    dry_run: bool,

    /// Allow a cleartext `http://` RPC endpoint for a non-localhost host.
    ///
    /// By default the CLI refuses any RPC URL that is not `https://` unless
    /// the host is loopback (`localhost` / `127.0.0.1` / `::1`), so a planted
    /// `.env` file cannot silently redirect traffic to an unauthenticated
    /// endpoint. Pass this flag for development against a self-hosted devnet.
    #[arg(long, global = true)]
    unsafe_rpc: bool,
}

// ---------------------------------------------------------------------------
// Environment validation (#68)
// ---------------------------------------------------------------------------

/// Verify that the `stellar` CLI binary is available in `PATH`.
///
/// This is called once at startup so users get a clear, actionable error
/// message instead of a cryptic OS-level "program not found" when the binary
/// is missing.
///
/// Returns `Ok(())` if the binary is found, or `Err(message)` with
/// installation instructions if it is not.
fn validate_environment() -> Result<(), String> {
    use std::process::Command;

    match Command::new(rpc::stellar_bin()).arg("--version").output() {
        Ok(_) => Ok(()),
        Err(_) => Err("Error: `stellar` CLI not found in PATH.\n\
             \n\
             Install it with:\n\
             \n\
             \tcargo install --locked stellar-cli --features opt\n\
             \n\
             Or follow the official guide:\n\
             \thttps://developers.stellar.org/docs/tools/cli/install-cli\n\
             \n\
             After installing, run `stellar --version` to confirm the installation."
            .to_string()),
    }
}

/// Build the output options from the global flags.
///
/// #77/#74: `--json` takes priority over `--human-readable`; `--quiet`
/// forces the JSON envelope so the only stdout line is the result.
fn output_opts(cli: &Cli) -> OutputOpts {
    let format = if cli.json || cli.quiet {
        OutputFormat::Json
    } else if cli.human_readable {
        OutputFormat::Human
    } else {
        OutputFormat::Raw
    };
    OutputOpts {
        format,
        quiet: cli.quiet,
        dry_run: cli.dry_run,
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    // ── #66: Load .env file before reading any env vars ───────────────────
    // dotenvy::dotenv() looks for a `.env` file in the current directory (and
    // walks up to the workspace root).  `.ok()` silently ignores a missing
    // file — users who rely purely on exported env vars are unaffected.
    // Priority: CLI args > env vars > .env file values.
    dotenvy::dotenv().ok();

    let cli = Cli::parse();

    // ── #75: Shell completions never touch the network or the stellar
    // binary, so handle them before environment validation / dispatch. ────
    // Matched by reference (not by value) so `cli.command` is still whole
    // and movable into `dispatch` below when this arm doesn't match.
    if let Commands::Completion { shell } = &cli.command {
        let mut cmd = Cli::command();
        let bin_name = cmd.get_name().to_string();
        clap_complete::generate(*shell, &mut cmd, bin_name, &mut std::io::stdout());
        return;
    }

    // ── #484: `trellis history` only reads a local file, so it needs neither
    // the stellar binary nor a valid contract/key configuration. ─────────
    if let Commands::History { limit, clear } = cli.command {
        let path = history::default_path();
        if let Err(msg) = commands::run_history(path.as_deref(), limit, clear, &output_opts(&cli)) {
            eprintln!("{msg}");
            process::exit(1);
        }
        return;
    }

    // ── #80: Resolve config from --network preset + CLI / env overrides ───
    let config = match config::Config::resolve(
        cli.network,
        cli.rpc_url.clone(),
        cli.network_passphrase.clone(),
        cli.source_key_file.clone(),
        cli.as_role,
    ) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("{msg}");
            process::exit(1);
        }
    };

    // ── #485: `trellis config` reports the resolved configuration, including
    // a missing `stellar` binary or invalid values, instead of exiting on
    // them like the checks below do. ──────────────────────────────────────
    if let Commands::Config = cli.command {
        let opts = output_opts(&cli);
        let stellar_found = validate_environment().is_ok();
        if commands::run_config(cli.network, &config, stellar_found, &opts).is_err() {
            process::exit(1);
        }
        return;
    }

    // ── #68: Validate stellar binary at startup ────────────────────────────
    if let Err(msg) = validate_environment() {
        eprintln!("{msg}");
        process::exit(1);
    }

    // ── #236/#237: Reject a malformed contract ID or RPC URL up front with
    // a clear message instead of a cryptic failure deep in the Stellar CLI. ─
    if let Err(errors) = config.validate() {
        eprintln!("Error: invalid configuration:");
        for e in &errors {
            eprintln!("  - {e}");
        }
        eprintln!(
            "\nSet the required environment variables (TRELLIS_CONTRACT_ID, \
             TRELLIS_SOURCE_KEY or TRELLIS_SOURCE_KEY_FILE) or pass the matching CLI flags."
        );
        process::exit(1);
    }

    // ── #156: Validate the resolved RPC URL before any network use ────────
    // Rejects malformed URLs and cleartext HTTP to non-localhost hosts so a
    // compromised `.env` cannot point the CLI at a forged RPC endpoint.
    // `--unsafe-rpc` downgrades the hard error to a printed warning.
    match config::validate_rpc_url(&config.rpc_url, cli.unsafe_rpc) {
        Ok(Some(warning)) => eprintln!("{warning}"),
        Ok(None) => {}
        Err(msg) => {
            eprintln!("{msg}");
            process::exit(1);
        }
    }

    let opts = output_opts(&cli);

    // ── #67: Propagate errors from dispatch; exit(1) only in main ─────────
    // All cleanup (destructors, buffer flushes) runs before the exit call
    // because process::exit is only called here, never inside library code.
    if let Err(msg) = commands::dispatch(cli.command, &config, &opts) {
        if !msg.is_empty() {
            eprintln!("{msg}");
        }
        process::exit(1);
    }
}
