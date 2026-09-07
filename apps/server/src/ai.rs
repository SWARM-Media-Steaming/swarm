//! Outbound calls to a user-configured AI provider (Claude/Codex/Grok) —
//! issue #235's "AI" tab. Every advanced feature that uses this
//! (`ai_scrape_assist` in `gui.rs`, `reorganize.rs`'s unparseable-filename
//! fallback) treats the provider as a single-turn text-completion box: send
//! a system prompt plus one user prompt, get plain text back, parse
//! whatever JSON is embedded in it. Nothing here ever writes to disk or the
//! library itself — callers own that, after a human approves.
//!
//! Provider model names remain persisted for backward compatibility, but
//! the UI intentionally exposes only each installed CLI and its existing
//! machine sign-in. AI calls are fail-closed unless the CLI reports at least
//! ten percent usage remaining.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

pub const MINIMUM_USAGE_REMAINING_PERCENT: f64 = 10.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiProviderKind {
    Claude,
    Codex,
    Grok,
}

impl AiProviderKind {
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "claude" => Some(AiProviderKind::Claude),
            "codex" => Some(AiProviderKind::Codex),
            "grok" => Some(AiProviderKind::Grok),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            AiProviderKind::Claude => "claude",
            AiProviderKind::Codex => "codex",
            AiProviderKind::Grok => "grok",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AiProviderKind::Claude => "Claude",
            AiProviderKind::Codex => "Codex",
            AiProviderKind::Grok => "Grok",
        }
    }

    /// A reasonable starting point, not a guarantee the model still exists
    /// — `settings::AiProviderSetting::model` is plain user-editable text
    /// precisely because providers move faster than this app can track.
    pub fn default_model(self) -> &'static str {
        match self {
            AiProviderKind::Claude => "claude-sonnet-5",
            AiProviderKind::Codex => "gpt-5.1-codex",
            AiProviderKind::Grok => "grok-4",
        }
    }

    pub fn all() -> [AiProviderKind; 3] {
        [AiProviderKind::Claude, AiProviderKind::Codex, AiProviderKind::Grok]
    }

    /// The executable name of this provider's CLI on `PATH`.
    fn cli_name(self) -> &'static str {
        match self {
            AiProviderKind::Claude => "claude",
            AiProviderKind::Codex => "codex",
            AiProviderKind::Grok => "grok",
        }
    }

    pub fn cli_label(self) -> &'static str {
        match self {
            AiProviderKind::Claude => "Claude Code",
            AiProviderKind::Codex => "Codex CLI",
            AiProviderKind::Grok => "Grok CLI",
        }
    }

    pub fn docs_url(self) -> &'static str {
        match self {
            AiProviderKind::Claude => "https://docs.anthropic.com/en/docs/claude-code/overview",
            AiProviderKind::Codex => "https://developers.openai.com/codex/cli/",
            AiProviderKind::Grok => "https://x.ai/",
        }
    }
}

// ---- CLI detection (issue #252) -------------------------------------------
//
// The AI tab no longer asks for an API key: it detects each provider's
// locally-installed CLI and uses the machine's existing sign-in, mirroring
// the SWARM Automation app's "Enabled AI tools" panel. Detection shells out
// to `<cli> --version` and the provider's own auth-status subcommand, over a
// PATH augmented the same way a login shell would resolve it (GUI apps on
// macOS otherwise inherit a bare PATH that misses Homebrew / npm-global).

/// One provider CLI's detection result, serialized to the AI tab.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiToolInfo {
    pub id: String,
    pub label: String,
    pub cli_label: String,
    pub installed: bool,
    pub path: String,
    pub version: String,
    pub signed_in: bool,
    pub usage_remaining_percent: Option<f64>,
    pub usage_available: bool,
    pub usage_status: String,
    pub status: String,
    pub docs_url: String,
}

/// PATH as a login shell would see it — GUI-launched apps on macOS inherit a
/// minimal PATH that misses Homebrew, `~/.local/bin`, and npm-global, where
/// these CLIs usually live. Ported from the SWARM Automation app's
/// `tools::enhanced_path`.
fn enhanced_path() -> String {
    let mut values = Vec::<String>::new();
    if let Ok(output) = std::process::Command::new("/bin/zsh")
        .args(["-lic", "printf '%s' \"$PATH\""])
        .output()
    {
        if output.status.success() {
            values.extend(
                String::from_utf8_lossy(&output.stdout)
                    .split(':')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
            );
        }
    }
    if let Ok(current) = std::env::var("PATH") {
        values.extend(
            current
                .split(':')
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        );
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for suffix in [".local/bin", ".npm-global/bin", ".cargo/bin"] {
            values.push(home.join(suffix).to_string_lossy().into_owned());
        }
    }
    values.extend(
        ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"]
            .into_iter()
            .map(str::to_string),
    );
    values.dedup();
    values.join(":")
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn find_executable(name: &str) -> Option<PathBuf> {
    enhanced_path()
        .split(':')
        .map(|dir| Path::new(dir).join(name))
        .find(|candidate| is_executable(candidate))
}

fn command_output(program: &Path, args: &[&str]) -> (bool, String) {
    match std::process::Command::new(program)
        .args(args)
        .env("PATH", enhanced_path())
        .output()
    {
        Ok(output) => {
            let text = if output.stdout.is_empty() {
                String::from_utf8_lossy(&output.stderr).trim().to_string()
            } else {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            };
            (output.status.success(), text)
        }
        Err(error) => (false, error.to_string()),
    }
}

fn cli_signed_in(kind: AiProviderKind, bin: &Path) -> bool {
    match kind {
        AiProviderKind::Claude => {
            let (ok, out) = command_output(bin, &["auth", "status", "--json"]);
            ok && serde_json::from_str::<serde_json::Value>(&out)
                .ok()
                .and_then(|v| v.get("loggedIn").and_then(|v| v.as_bool()))
                .unwrap_or(false)
        }
        AiProviderKind::Codex => {
            let (ok, out) = command_output(bin, &["login", "status"]);
            ok && out.to_lowercase().contains("logged in")
        }
        AiProviderKind::Grok => {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".grok/auth.json").is_file())
                .unwrap_or(false)
                || std::env::var_os("XAI_API_KEY").is_some()
        }
    }
}

fn percent_used_after_prefix(line: &str, prefix: &str) -> Option<f64> {
    let value = line.strip_prefix(prefix)?.split("% used").next()?.trim();
    value.parse::<f64>().ok()
}

fn claude_usage_remaining(bin: &Path) -> Option<f64> {
    let (ok, output) = command_output(
        bin,
        &["-p", "/usage", "--output-format", "json", "--tools", "", "--no-session-persistence"],
    );
    if !ok {
        return None;
    }
    let usage = serde_json::from_str::<serde_json::Value>(&output)
        .ok()?
        .get("result")?
        .as_str()?
        .to_string();
    let mut remaining = Vec::new();
    for line in usage.lines() {
        if let Some(used) = percent_used_after_prefix(line, "Current session:") {
            remaining.push(100.0 - used);
        } else if line.starts_with("Current week") {
            let used = line.split(':').nth(1)?.split("% used").next()?.trim().parse::<f64>().ok()?;
            remaining.push(100.0 - used);
        }
    }
    remaining.into_iter().reduce(f64::min)
}

fn receive_codex_response(
    receiver: &std::sync::mpsc::Receiver<serde_json::Value>,
    id: u64,
) -> Option<serde_json::Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let timeout = deadline.checked_duration_since(std::time::Instant::now())?;
        let message = receiver.recv_timeout(timeout).ok()?;
        if message.get("id").and_then(serde_json::Value::as_u64) == Some(id) {
            return Some(message);
        }
    }
}

fn codex_remaining_from_limits(limits: &serde_json::Value) -> Option<f64> {
    if limits.get("rateLimitReachedType").is_some_and(|value| !value.is_null())
        || limits.get("spendControlReached").and_then(serde_json::Value::as_bool) == Some(true)
    {
        return Some(0.0);
    }
    ["primary", "secondary"]
        .into_iter()
        .filter_map(|key| limits.get(key)?.get("usedPercent")?.as_f64().map(|used| 100.0 - used))
        .reduce(f64::min)
}

fn codex_usage_remaining(bin: &Path) -> Option<f64> {
    let mut child = std::process::Command::new(bin)
        .args(["app-server", "--listen", "stdio://"])
        .env("PATH", enhanced_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(message) = serde_json::from_str(&line) {
                let _ = sender.send(message);
            }
        }
    });
    let result = (|| {
        let stdin = child.stdin.as_mut()?;
        writeln!(stdin, "{}", serde_json::json!({
            "method": "initialize", "id": 1,
            "params": {"clientInfo": {"name": "swarm_media_server", "title": "SWARM Media Server", "version": "1.0.0"}}
        })).ok()?;
        stdin.flush().ok()?;
        let initialized = receive_codex_response(&receiver, 1)?;
        if initialized.get("error").is_some() {
            return None;
        }
        writeln!(stdin, "{}", serde_json::json!({"method": "initialized", "params": {}})).ok()?;
        writeln!(stdin, "{}", serde_json::json!({"method": "account/rateLimits/read", "id": 2, "params": {}})).ok()?;
        stdin.flush().ok()?;
        let response = receive_codex_response(&receiver, 2)?;
        let limits = response.get("result")?.get("rateLimits")?;
        codex_remaining_from_limits(limits)
    })();
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn cli_usage_remaining(kind: AiProviderKind, bin: &Path) -> Option<f64> {
    match kind {
        AiProviderKind::Claude => claude_usage_remaining(bin),
        AiProviderKind::Codex => codex_usage_remaining(bin),
        AiProviderKind::Grok => Some(100.0),
    }
}

fn usage_is_available(remaining: Option<f64>) -> bool {
    remaining.is_some_and(|value| value >= MINIMUM_USAGE_REMAINING_PERCENT)
}

pub fn detect_provider(kind: AiProviderKind) -> AiToolInfo {
    let bin = find_executable(kind.cli_name());
    let installed = bin.is_some();
    let version = bin
        .as_deref()
        .map(|path| command_output(path, &["--version"]).1.lines().next().unwrap_or_default().trim().to_string())
        .unwrap_or_default();
    let signed_in = bin.as_deref().map(|path| cli_signed_in(kind, path)).unwrap_or(false);
    let usage_remaining_percent = if signed_in {
        bin.as_deref().and_then(|path| cli_usage_remaining(kind, path))
    } else {
        None
    };
    let usage_available = usage_is_available(usage_remaining_percent);
    let usage_status = match usage_remaining_percent {
        Some(remaining) => format!("{remaining:.0}% usage remaining"),
        None if signed_in => "Usage unavailable".to_string(),
        None => String::new(),
    };
    let status = if !installed {
        "Not installed".to_string()
    } else if signed_in {
        "Signed in".to_string()
    } else {
        "Sign-in required".to_string()
    };
    AiToolInfo {
        id: kind.id().to_string(),
        label: kind.label().to_string(),
        cli_label: kind.cli_label().to_string(),
        installed,
        path: bin.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        version,
        signed_in,
        usage_remaining_percent,
        usage_available,
        usage_status,
        status,
        docs_url: kind.docs_url().to_string(),
    }
}

pub fn detect_all() -> Vec<AiToolInfo> {
    AiProviderKind::all().into_iter().map(detect_provider).collect()
}

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("{0} request failed: {1}")]
    Http(&'static str, String),
    #[error("{0} returned an error: {1}")]
    Api(&'static str, String),
    #[error("{0} response could not be parsed: {1}")]
    Parse(&'static str, String),
}

enum Transport {
    /// Direct HTTP to the provider's API using a saved key.
    #[cfg(test)]
    Http { api_key: String, base_url: String, http: reqwest::Client },
    /// Shell out to the provider's locally-installed, already-signed-in CLI
    /// (issue #252) — no API key needed.
    Cli { bin: PathBuf },
}

pub struct AiClient {
    kind: AiProviderKind,
    model: String,
    transport: Transport,
}

impl AiClient {
    #[cfg(test)]
    pub fn with_base_url(kind: AiProviderKind, api_key: String, model: String, base_url: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        Self { kind, model, transport: Transport::Http { api_key, base_url, http } }
    }

    /// A client that drives the provider's installed CLI. Errors if the CLI
    /// is not on PATH.
    pub fn cli(kind: AiProviderKind, model: String) -> Result<Self, String> {
        let bin = find_executable(kind.cli_name())
            .ok_or_else(|| format!("{} was not found — install it and sign in.", kind.cli_label()))?;
        Ok(Self { kind, model, transport: Transport::Cli { bin } })
    }

    /// Sends a single-turn prompt and returns the model's plain-text reply.
    pub async fn complete(&self, system: &str, user: &str) -> Result<String, AiError> {
        match &self.transport {
            Transport::Cli { bin } => self.complete_cli(bin, system, user).await,
            #[cfg(test)]
            Transport::Http { .. } => match self.kind {
                AiProviderKind::Claude => self.complete_anthropic(system, user).await,
                AiProviderKind::Codex | AiProviderKind::Grok => {
                    self.complete_openai_compatible(system, user).await
                }
            },
        }
    }

    /// Runs the provider CLI in non-interactive "one prompt, print the
    /// answer" mode. Best-effort per-CLI invocation — the exact flags each
    /// tool exposes for this move faster than this app can track, so a
    /// failure here surfaces the CLI's own stderr rather than being masked.
    async fn complete_cli(&self, bin: &Path, system: &str, user: &str) -> Result<String, AiError> {
        let prompt = format!("{system}\n\n{user}");
        let args: Vec<String> = match self.kind {
            AiProviderKind::Claude => {
                vec!["-p".into(), prompt, "--model".into(), self.model.clone()]
            }
            AiProviderKind::Codex => vec!["exec".into(), "--skip-git-repo-check".into(), prompt],
            AiProviderKind::Grok => vec!["-p".into(), prompt],
        };
        let label = self.kind.cli_label();
        let path_env = tokio::task::spawn_blocking(enhanced_path)
            .await
            .map_err(|e| AiError::Http(label, e.to_string()))?;
        let output = tokio::process::Command::new(bin)
            .args(&args)
            .env("PATH", path_env)
            .output()
            .await
            .map_err(|e| AiError::Http(label, e.to_string()))?;
        if !output.status.success() {
            return Err(AiError::Api(
                label,
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if text.is_empty() {
            return Err(AiError::Parse(label, "the CLI produced no output".to_string()));
        }
        Ok(text)
    }

    #[cfg(test)]
    fn api_key(&self) -> &str {
        match &self.transport {
            Transport::Http { api_key, .. } => api_key,
            Transport::Cli { .. } => "",
        }
    }

    #[cfg(test)]
    fn base_url(&self) -> &str {
        match &self.transport {
            Transport::Http { base_url, .. } => base_url,
            Transport::Cli { .. } => "",
        }
    }

    #[cfg(test)]
    fn http(&self) -> &reqwest::Client {
        match &self.transport {
            Transport::Http { http, .. } => http,
            Transport::Cli { .. } => unreachable!("http() is only reached on the Http transport"),
        }
    }

    #[cfg(test)]
    async fn complete_anthropic(&self, system: &str, user: &str) -> Result<String, AiError> {
        let url = format!("{}/v1/messages", self.base_url());
        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": 1024,
            "system": system,
            "messages": [{"role": "user", "content": user}],
        });
        let response = self
            .http()
            .post(&url)
            .header("x-api-key", self.api_key())
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .map_err(|e| AiError::Http(self.kind.label(), e.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| AiError::Http(self.kind.label(), e.to_string()))?;
        if !status.is_success() {
            return Err(AiError::Api(self.kind.label(), api_error_message(&text)));
        }
        #[derive(serde::Deserialize)]
        struct ContentBlock {
            text: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct MessagesResponse {
            content: Vec<ContentBlock>,
        }
        let parsed: MessagesResponse =
            serde_json::from_str(&text).map_err(|e| AiError::Parse(self.kind.label(), e.to_string()))?;
        parsed
            .content
            .into_iter()
            .find_map(|c| c.text)
            .ok_or_else(|| AiError::Parse(self.kind.label(), "empty response".to_string()))
    }

    /// OpenAI's and xAI's chat-completions endpoints are wire-compatible
    /// (same request/response shape, both accept a bearer token) — one
    /// implementation covers Codex and Grok.
    #[cfg(test)]
    async fn complete_openai_compatible(&self, system: &str, user: &str) -> Result<String, AiError> {
        let url = format!("{}/v1/chat/completions", self.base_url());
        let body = serde_json::json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
        });
        let response = self
            .http()
            .post(&url)
            .bearer_auth(self.api_key())
            .json(&body)
            .send()
            .await
            .map_err(|e| AiError::Http(self.kind.label(), e.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| AiError::Http(self.kind.label(), e.to_string()))?;
        if !status.is_success() {
            return Err(AiError::Api(self.kind.label(), api_error_message(&text)));
        }
        #[derive(serde::Deserialize)]
        struct Message {
            content: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct Choice {
            message: Message,
        }
        #[derive(serde::Deserialize)]
        struct ChatResponse {
            choices: Vec<Choice>,
        }
        let parsed: ChatResponse =
            serde_json::from_str(&text).map_err(|e| AiError::Parse(self.kind.label(), e.to_string()))?;
        parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| AiError::Parse(self.kind.label(), "empty response".to_string()))
    }
}

/// Best-effort extraction of `{"error": {"message": "..."}}` (both
/// Anthropic's and OpenAI-compatible APIs' error shape); falls back to the
/// raw body so a genuinely different error shape is still visible to the
/// user rather than swallowed.
#[cfg(test)]
fn api_error_message(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("error")?.get("message")?.as_str().map(str::to_string))
        .unwrap_or_else(|| body.to_string())
}

/// Models are asked to reply with nothing but a JSON object but don't
/// always comply (prose preamble, a wrapping code fence) — pull out the
/// first balanced-looking `{...}` span and parse just that.
pub fn parse_json_object<T: serde::de::DeserializeOwned>(text: &str) -> Option<T> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }
    serde_json::from_str(&text[start..=end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::json;

    async fn spawn_mock(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn claude_sends_x_api_key_header_and_parses_content_text() {
        let router = Router::new().route(
            "/v1/messages",
            post(
                |headers: axum::http::HeaderMap, Json(body): Json<serde_json::Value>| async move {
                    assert_eq!(headers.get("x-api-key").unwrap(), "secret-key");
                    assert_eq!(headers.get("anthropic-version").unwrap(), "2023-06-01");
                    assert_eq!(body["model"], "claude-sonnet-5");
                    Json(json!({"content": [{"type": "text", "text": "ok"}]}))
                },
            ),
        );
        let base = spawn_mock(router).await;
        let client = AiClient::with_base_url(
            AiProviderKind::Claude,
            "secret-key".to_string(),
            "claude-sonnet-5".to_string(),
            base,
        );
        let reply = client.complete("system", "user").await.unwrap();
        assert_eq!(reply, "ok");
    }

    #[tokio::test]
    async fn openai_compatible_sends_bearer_auth_and_parses_choice_content() {
        let router = Router::new().route(
            "/v1/chat/completions",
            post(
                |headers: axum::http::HeaderMap, Json(body): Json<serde_json::Value>| async move {
                    assert_eq!(headers.get(axum::http::header::AUTHORIZATION).unwrap(), "Bearer secret-key");
                    assert_eq!(body["model"], "gpt-5.1-codex");
                    Json(json!({"choices": [{"message": {"content": "ok"}}]}))
                },
            ),
        );
        let base = spawn_mock(router).await;
        let client = AiClient::with_base_url(
            AiProviderKind::Codex,
            "secret-key".to_string(),
            "gpt-5.1-codex".to_string(),
            base,
        );
        let reply = client.complete("system", "user").await.unwrap();
        assert_eq!(reply, "ok");
    }

    #[tokio::test]
    async fn non_success_status_surfaces_the_provider_error_message() {
        let router = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                (
                    axum::http::StatusCode::UNAUTHORIZED,
                    Json(json!({"error": {"message": "invalid API key"}})),
                )
            }),
        );
        let base = spawn_mock(router).await;
        let client = AiClient::with_base_url(
            AiProviderKind::Grok,
            "bad-key".to_string(),
            "grok-4".to_string(),
            base,
        );
        let err = client.complete("system", "user").await.unwrap_err();
        assert!(matches!(err, AiError::Api(_, message) if message == "invalid API key"));
    }

    #[test]
    fn parses_json_object_wrapped_in_prose_and_code_fences() {
        let text = "Sure, here you go:\n```json\n{\"title\": \"Heat\", \"year\": 1995}\n```\nHope that helps!";
        #[derive(serde::Deserialize)]
        struct Guess {
            title: String,
            year: u32,
        }
        let guess: Guess = parse_json_object(text).unwrap();
        assert_eq!(guess.title, "Heat");
        assert_eq!(guess.year, 1995);
    }

    #[test]
    fn returns_none_for_text_with_no_json_object() {
        assert!(parse_json_object::<serde_json::Value>("no json here").is_none());
    }

    #[test]
    fn parses_claude_usage_percentage_lines() {
        assert_eq!(percent_used_after_prefix("Current session: 91.5% used", "Current session:"), Some(91.5));
        assert_eq!(percent_used_after_prefix("Current week: 20% used", "Current session:"), None);
    }

    #[test]
    fn codex_usage_uses_the_most_constrained_window() {
        let limits = json!({
            "primary": {"usedPercent": 82.0},
            "secondary": {"usedPercent": 95.0},
            "rateLimitReachedType": null,
            "spendControlReached": false
        });
        assert_eq!(codex_remaining_from_limits(&limits), Some(5.0));
    }

    #[test]
    fn codex_usage_reports_zero_when_a_limit_is_reached() {
        let limits = json!({"primary": {"usedPercent": 10.0}, "rateLimitReachedType": "primary"});
        assert_eq!(codex_remaining_from_limits(&limits), Some(0.0));
    }

    #[test]
    fn ai_usage_gate_is_fail_closed_at_ten_percent() {
        assert!(!usage_is_available(None));
        assert!(!usage_is_available(Some(9.9)));
        assert!(usage_is_available(Some(10.0)));
    }
}
