//! Obtaining a credential for the model provider.
//!
//! Two paths, in priority order:
//!
//! 1. `KINGDOM_API_KEY` -- a plain key, for anyone who already holds a token.
//! 2. `KINGDOM_API_KEY_HELPER` -- a shell command that prints one, defaulting to
//!    `agency auth github`.
//!
//! The helper contract is the fiddly part, and is taken from a working
//! implementation rather than invented: **the last non-empty line of stdout is
//! the credential, and stderr is diagnostics only.** `agency auth github`
//! prints a wall of tracing output on stderr and the bare token on stdout, so a
//! naive "capture all output" implementation silently sends log lines to the
//! API as a bearer token and gets an opaque 401 back.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Default helper for Copilot. Agency is Microsoft's agent platform CLI; this
/// subcommand mints a short-lived GitHub token.
pub const DEFAULT_COPILOT_HELPER: &str = "agency auth github";

/// How long a minted credential is reused before the helper is re-run.
const DEFAULT_TTL: Duration = Duration::from_secs(60 * 60);

/// Where a credential came from, for the status badge. Never the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Read straight from `KINGDOM_API_KEY`.
    PlainKey,
    /// Minted by running a helper command.
    Helper(String),
}

impl Source {
    pub fn describe(&self) -> String {
        match self {
            Source::PlainKey => "KINGDOM_API_KEY".to_string(),
            Source::Helper(cmd) => format!("`{cmd}`"),
        }
    }
}

#[derive(Debug)]
pub struct Credential {
    pub token: String,
    pub source: Source,
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("no credential configured: set KINGDOM_API_KEY, or KINGDOM_API_KEY_HELPER to a command that prints one")]
    NotConfigured,
    #[error("could not run `{command}`: {source}")]
    Spawn {
        command: String,
        source: std::io::Error,
    },
    #[error("`{command}` exited with {code}: {stderr}")]
    Failed {
        command: String,
        code: String,
        stderr: String,
    },
    #[error("`{command}` printed nothing on stdout")]
    Empty { command: String },
}

/// A minted credential and when it goes stale.
static CACHE: Mutex<Option<(String, Source, Instant)>> = Mutex::new(None);

/// Resolves a credential, re-using a cached one until its TTL expires.
///
/// `default_helper` is used when `KINGDOM_API_KEY_HELPER` is unset, so the
/// Copilot provider works with no configuration beyond choosing it.
pub async fn resolve(default_helper: Option<&str>) -> Result<Credential, CredentialError> {
    // A plain key wins, and short-circuits before any subprocess is spawned.
    if let Some(key) = non_empty_env("KINGDOM_API_KEY") {
        return Ok(Credential {
            token: key,
            source: Source::PlainKey,
        });
    }

    let command = non_empty_env("KINGDOM_API_KEY_HELPER")
        .or_else(|| default_helper.map(str::to_string))
        .ok_or(CredentialError::NotConfigured)?;

    if let Some(cached) = cached_for(&command) {
        return Ok(cached);
    }

    let token = run_helper(&command).await?;
    let source = Source::Helper(command);

    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some((token.clone(), source.clone(), Instant::now() + ttl()));
    }

    Ok(Credential { token, source })
}

/// Runs the helper and extracts the credential from its stdout.
async fn run_helper(command: &str) -> Result<String, CredentialError> {
    let output = tokio::process::Command::new("sh")
        .args(["-c", command])
        .output()
        .await
        .map_err(|source| CredentialError::Spawn {
            command: command.to_string(),
            source,
        })?;

    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        return Err(CredentialError::Failed {
            command: command.to_string(),
            code: output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "a signal".to_string()),
            // The tail is where the actual complaint lives; the head is banner
            // noise. Bound it so a chatty helper cannot flood the UI.
            stderr: tail(&stderr, 400),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    last_non_empty_line(&stdout).ok_or_else(|| CredentialError::Empty {
        command: command.to_string(),
    })
}

/// The credential is the last non-empty line of stdout. See the module docs:
/// helpers legitimately print progress above it.
fn last_non_empty_line(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(str::to_string)
}

fn cached_for(command: &str) -> Option<Credential> {
    let cache = CACHE.lock().ok()?;
    let (token, source, expires) = cache.as_ref()?;
    if Instant::now() >= *expires {
        return None;
    }
    // A changed helper command must re-mint rather than reuse the old token.
    match source {
        Source::Helper(c) if c == command => Some(Credential {
            token: token.clone(),
            source: source.clone(),
        }),
        _ => None,
    }
}

fn ttl() -> Duration {
    std::env::var("KINGDOM_API_KEY_HELPER_TTL_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TTL)
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn tail(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    let count = trimmed.chars().count();
    if count <= max {
        return trimmed.to_string();
    }
    let skipped = count - max;
    format!("…{}", trimmed.chars().skip(skipped).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The helper contract, which is the whole reason this module exists.
    ///
    /// A real helper (`agency auth github`) prints the token on stdout and a
    /// large amount of tracing on stderr. Getting this wrong means sending a log
    /// line as a bearer token and debugging an opaque 401.
    #[tokio::test]
    async fn helper_takes_the_last_stdout_line_and_ignores_stderr() {
        let token = run_helper(
            "echo 'connecting…'; echo 'noisy diagnostics' >&2; echo gho_thetoken; echo '' ",
        )
        .await
        .expect("helper should succeed");

        assert_eq!(token, "gho_thetoken");
    }

    #[tokio::test]
    async fn helper_failure_surfaces_the_exit_code_and_stderr() {
        let err = run_helper("echo 'not signed in' >&2; exit 3")
            .await
            .expect_err("non-zero exit must be an error");

        let msg = err.to_string();
        assert!(msg.contains('3'), "exit code should be reported: {msg}");
        assert!(
            msg.contains("not signed in"),
            "stderr should be reported so the King can act on it: {msg}"
        );
    }

    #[tokio::test]
    async fn helper_that_prints_nothing_is_an_error_not_an_empty_token() {
        let err = run_helper("true").await.expect_err("empty is an error");
        assert!(matches!(err, CredentialError::Empty { .. }));
    }
}
