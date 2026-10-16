mod commands;
mod config;
mod history;
mod input;
mod keystore;
mod rpc;
mod sanitizer;
mod strkey;
mod utils;

#[cfg(test)]
#[path = "../sdk_compat.rs"]
mod sdk_compat;

use clap::{ArgAction, CommandFactory, Parser};
use commands::{Commands, OutputFormat, OutputOpts};
use config::{Network, Role};
use std::process;

// ---------------------------------------------------------------------------
// Top-level CLI definition
// ---------------------------------------------------------------------------

/// Trellis Protocol CLI — milestone-based escrow on Stellar Soroban.
///
/// Configuration can be loaded from a `.env` file in the working directory (or
/// an explicit file via `--env-file` / `TRELLIS_ENV_FILE`) or set as
/// environment variables.  CLI flags take highest priority, then env vars,
/// then the env file.
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
    //
    // COUPLING: the range is not hardcoded — `build.rs` reads it from the
    // `soroban-sdk` entry in contracts/trellis_core/Cargo.toml and fails the
    // build if it cannot be found, so a contract SDK bump is picked up here
    // automatically.
    long_version = concat!(
        env!("CARGO_PKG_VERSION"),
        "\nsoroban-sdk compat: ",
        env!("TRELLIS_SOROBAN_SDK_COMPAT"),
        " (see contracts/trellis_core/Cargo.toml)",
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

    /// Trace RPC attempts to stderr for debugging retry behavior. `-v` shows
    /// each attempt's exit code, full error output and why retrying stopped;
    /// `-vv` also prints the full (secret-redacted) command and per-attempt
    /// timing. Ignored under `--quiet`.
    #[arg(short = 'v', long, global = true, action = ArgAction::Count)]
    verbose: u8,

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
// Env file loading (#66, #487)
// ---------------------------------------------------------------------------

/// Load configuration variables from an env file.
///
/// When `explicit` is set (from `--env-file` or `TRELLIS_ENV_FILE`) only that
/// file is loaded, and a missing/unreadable file is an error — silently
/// falling back to CWD discovery would run the command against the wrong
/// identity or network. Otherwise `.env` is discovered in the current
/// directory (and its ancestors) exactly as before, and a missing file is
/// ignored so users who rely purely on exported env vars are unaffected.
///
/// Variables already present in the process environment are never
/// overridden. Returns the path that was loaded, if any.
fn load_env_file(explicit: Option<&Path>) -> Result<Option<PathBuf>, String> {
    match explicit {
        Some(path) => dotenvy::from_path(path)
            .map(|()| Some(path.to_path_buf()))
            .map_err(|e| format!("Error: failed to load env file `{}`: {e}", path.display())),
        None => Ok(dotenvy::dotenv().ok()),
    }
}

/// Resolve the explicit env file path: `--env-file` wins over
/// `TRELLIS_ENV_FILE`; an empty env var counts as unset.
fn explicit_env_file(flag: Option<PathBuf>) -> Option<PathBuf> {
    flag.or_else(|| {
        std::env::var_os("TRELLIS_ENV_FILE")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    })
}

// ---------------------------------------------------------------------------
// Environment validation (#68)
// ---------------------------------------------------------------------------

/// The minimum `stellar` CLI major version required by this tool.
///
/// DEPLOYMENT.md documents `stellar CLI 26.x+` as a prerequisite.
const STELLAR_MIN_MAJOR: u64 = 26;

/// Verify that the `stellar` CLI binary is available in `PATH` **and** that
/// its reported major version meets the documented minimum (`26.x+`).
///
/// This is called once at startup so users get a clear, actionable error
/// message instead of a cryptic OS-level "program not found" or an obscure
/// RPC/argument error deep inside the invoke path.
///
/// Returns `Ok(())` if the binary is found and version-compatible, or
/// `Err(message)` with installation/upgrade instructions otherwise.
fn validate_environment() -> Result<(), String> {
    use std::process::Command;

    let output = Command::new(rpc::stellar_bin())
        .arg("--version")
        .output()
        .map_err(|_| {
            "Error: `stellar` CLI not found in PATH.\n\
             \n\
             Install it with:\n\
             \n\
             \tcargo install --locked stellar-cli --features opt\n\
             \n\
             Or follow the official guide:\n\
             \thttps://developers.stellar.org/docs/tools/cli/install-cli\n\
             \n\
             After installing, run `stellar --version` to confirm the installation."
                .to_string()
        })?;

    // `stellar --version` prints something like "stellar 26.0.0\n" to stdout.
    // Parse the first version-looking token (digits separated by dots) from
    // the combined stdout + stderr so we are robust against minor format
    // changes across releases.
    let version_output = String::from_utf8_lossy(&output.stdout);
    let version_output = version_output.trim();

    let detected_major = version_output
        .split_whitespace()
        .find_map(|token| {
            // Take the first token that looks like a semver / version number
            // (starts with a digit, contains at least one dot).
            if token.starts_with(|c: char| c.is_ascii_digit()) && token.contains('.') {
                token
                    .split('.')
                    .next()
                    .and_then(|major| major.parse::<u64>().ok())
            } else {
                None
            }
        })
        .ok_or_else(|| {
            format!(
                "Error: could not determine the `stellar` CLI version.\n\
                 \n\
                 `stellar --version` produced unexpected output: {version_output:?}\n\
                 \n\
                 Expected output like `stellar 26.0.0`.  Please ensure you have\n\
                 stellar CLI {STELLAR_MIN_MAJOR}.x or newer installed:\n\
                 \n\
                 \tcargo install --locked stellar-cli --features opt\n\
                 \thttps://developers.stellar.org/docs/tools/cli/install-cli"
            )
        })?;

    if detected_major < STELLAR_MIN_MAJOR {
        return Err(format!(
            "Error: `stellar` CLI version {detected_major}.x detected, \
             but version {STELLAR_MIN_MAJOR}.x or newer is required.\n\
             \n\
             Upgrade with:\n\
             \n\
             \tcargo install --locked stellar-cli --features opt\n\
             \n\
             Or follow the official guide:\n\
             \thttps://developers.stellar.org/docs/tools/cli/install-cli"
        ));
    }

    Ok(())
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
    let cli = Cli::parse();

    // ── #66/#487: Load the env file before reading any config env vars ────
    // No clap arg reads from the environment, so parsing first is safe and
    // lets `--env-file` pick the file. Without it, `.env` is discovered from
    // the CWD as before. Priority: CLI args > env vars > env file values.
    if let Err(msg) = load_env_file(explicit_env_file(cli.env_file.clone()).as_deref()) {
        eprintln!("{msg}");
        process::exit(1);
    }

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
    // ── #406: Skip stellar-binary check for --dry-run ─────────────────────
    // --dry-run only prints the command that would run; it never spawns the
    // stellar binary itself.  Requiring the binary here blocks a legitimate
    // use-case: previewing command construction on a machine where the
    // stellar CLI is not (yet) installed, or in CI environments that only
    // need to inspect the generated invocation.
    //
    // ── #68: Validate stellar binary at startup (non-dry-run only) ────────
    // `health` talks to the RPC endpoint natively (#467/#468), so it needs
    // neither the `stellar` binary nor a contract ID / source key.
    let native_only = matches!(cli.command, Commands::Health);

    if !cli.dry_run && !native_only {
        if let Err(msg) = validate_environment() {
            eprintln!("{msg}");
            process::exit(1);
        }
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
    let validation = if native_only {
        config::validate_rpc_url(&config.rpc_url).map_err(|e| vec![e])
    } else {
        config.validate()
    };
    if let Err(errors) = validation {
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
    match config::check_rpc_transport(&config.rpc_url, cli.unsafe_rpc) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strkey::{decode, encode, StrkeyError, StrkeyKind};
    use std::fs;

    #[test]
    fn long_version_reports_manifest_sdk_range() {
        let rendered = Cli::command().render_long_version();
        let expected = format!(
            "soroban-sdk compat: {} (see contracts/trellis_core/Cargo.toml)",
            env!("TRELLIS_SOROBAN_SDK_COMPAT")
        );
        assert!(rendered.contains(&expected), "got: {rendered}");
    }

    #[test]
    fn short_version_stays_bare() {
        let rendered = Cli::command().render_version();
        assert!(!rendered.contains("soroban-sdk"), "got: {rendered}");
        assert!(rendered.contains(env!("CARGO_PKG_VERSION")));
    }

    // ── Strkey codec smoke tests ──────────────────────────────────────────
    // The full test-vector suite lives in `strkey.rs`; these tests ensure the
    // module is wired into the CLI crate and that the public API round-trips
    // through the same entry points downstream sub-tasks will use.

    #[test]
    fn strkey_round_trips_ed25519_public_key() {
        // Known-good G-address (Stellar docs example).
        let g = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF";
        let (kind, bytes) = decode(g).expect("valid G-address");
        assert_eq!(kind, StrkeyKind::Ed25519PublicKey);
        assert_eq!(bytes.len(), 32);
        assert_eq!(encode(StrkeyKind::Ed25519PublicKey, &bytes).unwrap(), g);
    }

    #[test]
    fn strkey_round_trips_ed25519_secret_seed() {
        // Known-good S-address (Stellar docs example).
        let s = "SAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let (kind, bytes) = decode(s).expect("valid S-address");
        assert_eq!(kind, StrkeyKind::Ed25519SecretSeed);
        assert_eq!(bytes.len(), 32);
        assert_eq!(encode(StrkeyKind::Ed25519SecretSeed, &bytes).unwrap(), s);
    }

    #[test]
    fn strkey_round_trips_contract() {
        // Known-good C-address (Stellar docs example).
        let c = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD2KM";
        let (kind, bytes) = decode(c).expect("valid C-address");
        assert_eq!(kind, StrkeyKind::Contract);
        assert_eq!(bytes.len(), 32);
        assert_eq!(encode(StrkeyKind::Contract, &bytes).unwrap(), c);
    }

    #[test]
    fn strkey_rejects_corrupted_checksum() {
        // Flip the final base32 character of a valid G-address; the CRC16
        // trailer must reject it rather than silently returning garbage.
        let mut corrupted = String::from("GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF");
        corrupted.pop();
        corrupted.push('G');
        match decode(&corrupted) {
            Err(StrkeyError::InvalidChecksum) => {}
            other => panic!("expected InvalidChecksum, got {other:?}"),
        }
    }

    #[test]
    fn strkey_rejects_unknown_version_byte() {
        // A valid base32 + CRC16 payload with an unrecognized version byte
        // must be rejected — guards against silently accepting future or
        // foreign strkey variants.
        let bad = encode(StrkeyKind::Ed25519PublicKey, &[0u8; 32]).unwrap();
        let mut bytes = crate::strkey::base32_decode(&bad).unwrap();
        bytes[0] = 0xFF;
        let reencoded = crate::strkey::base32_encode(&bytes);
        match decode(&reencoded) {
            Err(StrkeyError::UnknownVersion(0xFF)) => {}
            other => panic!("expected UnknownVersion(0xFF), got {other:?}"),
        }
    }

    /// Write `contents` to a uniquely-named file in a fresh temp directory
    /// (outside the crate, so CWD-relative discovery could never find it).
    fn temp_env_file(name: &str, contents: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "trellis-env-file-{name}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("custom.env");
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn loads_variables_from_explicit_path_outside_cwd() {
        let path = temp_env_file("explicit", "TRELLIS_TEST_ENV_FILE_A=from-file\n");
        std::env::remove_var("TRELLIS_TEST_ENV_FILE_A");

        let loaded = load_env_file(Some(&path)).expect("explicit file loads");

        assert_eq!(loaded.as_deref(), Some(path.as_path()));
        assert_eq!(
            std::env::var("TRELLIS_TEST_ENV_FILE_A").as_deref(),
            Ok("from-file")
        );
        std::env::remove_var("TRELLIS_TEST_ENV_FILE_A");
    }

    #[test]
    fn explicit_path_does_not_override_exported_vars() {
        let path = temp_env_file("precedence", "TRELLIS_TEST_ENV_FILE_B=from-file\n");
        std::env::set_var("TRELLIS_TEST_ENV_FILE_B", "exported");

        load_env_file(Some(&path)).expect("explicit file loads");

        assert_eq!(
            std::env::var("TRELLIS_TEST_ENV_FILE_B").as_deref(),
            Ok("exported")
        );
        std::env::remove_var("TRELLIS_TEST_ENV_FILE_B");
    }

    #[test]
    fn missing_explicit_path_is_an_error() {
        let path = std::env::temp_dir().join("trellis-env-file-does-not-exist.env");
        let err = load_env_file(Some(&path)).unwrap_err();
        assert!(err.contains("failed to load env file"), "{err}");
        assert!(err.contains("trellis-env-file-does-not-exist.env"), "{err}");
    }

    #[test]
    fn no_explicit_path_falls_back_to_discovery_without_error() {
        // Whether or not a `.env` exists up the tree, discovery never fails.
        assert!(load_env_file(None).is_ok());
    }

    #[test]
    fn env_file_flag_takes_priority_over_env_var() {
        std::env::set_var("TRELLIS_ENV_FILE", "/from/env/var.env");
        assert_eq!(
            explicit_env_file(Some(PathBuf::from("/from/flag.env"))),
            Some(PathBuf::from("/from/flag.env"))
        );
        assert_eq!(
            explicit_env_file(None),
            Some(PathBuf::from("/from/env/var.env"))
        );
        std::env::set_var("TRELLIS_ENV_FILE", "");
        assert_eq!(explicit_env_file(None), None);
        std::env::remove_var("TRELLIS_ENV_FILE");
    }
}
