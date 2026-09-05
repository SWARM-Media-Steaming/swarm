//! Persisted GUI settings: media root(s) and TMDb API key. Kept separate
//! from `ServerCore`'s own state (STUN link, library) because these are
//! needed *before* a core can even be constructed. The packaged desktop app
//! deliberately has no environment-only media-root configuration path.

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

/// One configured AI provider (issue #235's "AI" tab) — Claude, Codex, or
/// Grok, all held to the same shape so the tab can render one row per
/// provider from a loop rather than three hand-written sections. `id` is
/// the stable key (`ai::AiProviderKind::id()`), never shown to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiProviderSetting {
    pub id: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub api_key: Option<String>,
    pub model: String,
}

/// The three supported providers, seeded disabled/keyless — a brand-new
/// install (or one upgrading from a pre-#235 settings.json) starts with
/// every advanced AI feature inert until a user opts in from the AI tab.
fn default_ai_providers() -> Vec<AiProviderSetting> {
    crate::ai::AiProviderKind::all()
        .into_iter()
        .map(|kind| AiProviderSetting {
            id: kind.id().to_string(),
            enabled: false,
            api_key: None,
            model: kind.default_model().to_string(),
        })
        .collect()
}

/// What kind of media a root is expected to hold. Declared by the user when
/// a root is added (issue #252) so the server knows what to expect in that
/// location rather than inferring purely from folder layout. `Mixed` — the
/// default and the shape every pre-#252 settings.json upgrades to — imposes
/// no expectation and behaves exactly as roots did before this field
/// existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootAssetType {
    #[default]
    Mixed,
    Movies,
    Shows,
    Music,
}

impl RootAssetType {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim).unwrap_or("") {
            "" | "mixed" => Ok(RootAssetType::Mixed),
            "movies" => Ok(RootAssetType::Movies),
            "shows" | "tv" => Ok(RootAssetType::Shows),
            "music" => Ok(RootAssetType::Music),
            other => Err(format!("unknown asset type \"{other}\"")),
        }
    }

    /// Human-readable label for toasts and reorganize prompts.
    pub fn label(self) -> &'static str {
        match self {
            RootAssetType::Mixed => "Mixed",
            RootAssetType::Movies => "Movies",
            RootAssetType::Shows => "TV Shows",
            RootAssetType::Music => "Music",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaRootSetting {
    pub label: String,
    pub path: String,
    /// A safe OS mount URL captured while a network share is healthy. This
    /// lets the background health monitor ask macOS to remount the share
    /// after SMB drops without storing a password in settings.json.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconnect_url: Option<String>,
    /// The kind of media this root holds — see [`RootAssetType`]. `#[serde(default)]`
    /// so every existing settings.json loads as `Mixed`.
    #[serde(default)]
    pub asset_type: RootAssetType,
}

/// A bounded accessibility check used by the desktop UI and recovery loop.
/// It performs real directory, metadata, and representative file I/O so a
/// stale SMB mount cannot pass merely because its `/Volumes` entry exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MediaRootHealth {
    pub label: String,
    pub path: String,
    pub available: bool,
    pub error: Option<String>,
    /// macOS is refusing the read for want of a Files-and-Folders / network-
    /// volume TCC grant (EPERM/EACCES), not because the path is gone or an
    /// SMB mount dropped. Reconnecting can't fix this; only a one-time
    /// approval in System Settings can. See GitHub #196.
    pub permission_denied: bool,
    pub auto_reconnect: bool,
    pub network_protocol: Option<String>,
}

pub fn media_root_health(roots: &[MediaRootSetting]) -> Vec<MediaRootHealth> {
    roots
        .iter()
        .map(|root| {
            let auto_reconnect = root
                .reconnect_url
                .as_deref()
                .is_some_and(automatically_reconnectable);
            let network_protocol = root.reconnect_url.as_deref().and_then(network_protocol);
            match media_root_readable(root) {
                Ok(_) => MediaRootHealth {
                    label: root.label.clone(),
                    path: root.path.clone(),
                    available: true,
                    error: None,
                    permission_denied: false,
                    auto_reconnect,
                    network_protocol,
                },
                Err(error) => MediaRootHealth {
                    label: root.label.clone(),
                    path: root.path.clone(),
                    available: false,
                    permission_denied: is_permission_denied(&error),
                    error: Some(error.to_string()),
                    auto_reconnect,
                    network_protocol,
                },
            }
        })
        .collect()
}

/// A denied macOS TCC grant surfaces as EPERM ("Operation not permitted")
/// or EACCES, both of which std maps to `PermissionDenied`. Kept as a named
/// helper so the GUI's classification and its tests describe the same thing.
fn is_permission_denied(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::PermissionDenied
}

fn media_root_readable(root: &MediaRootSetting) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    if root
        .reconnect_url
        .as_deref()
        .is_some_and(|url| url.starts_with("smb://"))
        && !smb_root_is_mounted(root)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            "SMB share is not mounted",
        ));
    }
    probe_media_root(Path::new(&root.path))
}

fn probe_media_root(path: &Path) -> std::io::Result<()> {
    const MAX_ENTRIES: usize = 32;
    const MAX_DEPTH: usize = 2;

    let mut directories = vec![(path.to_path_buf(), 0usize)];
    let mut inspected = 0usize;
    while let Some((directory, depth)) = directories.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            inspected += 1;
            if metadata.is_file() {
                let mut file = std::fs::File::open(entry.path())?;
                let mut byte = [0u8; 1];
                let _ = file.read(&mut byte)?;
                return Ok(());
            }
            if metadata.is_dir() && depth < MAX_DEPTH {
                directories.push((entry.path(), depth + 1));
            }
            if inspected >= MAX_ENTRIES {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn automatically_reconnectable(url: &str) -> bool {
    url.starts_with("smb://") || url.starts_with("afp://")
}

fn network_protocol(url: &str) -> Option<String> {
    if url.starts_with("smb://") {
        Some("SMB".to_string())
    } else {
        None
    }
}

/// Fill in reconnect URLs for currently mounted macOS SMB shares. Returns
/// true when settings changed and should be persisted.
pub fn populate_reconnect_urls(settings: &mut Settings) -> bool {
    let mut changed = false;
    for root in &mut settings.media_roots {
        if root.reconnect_url.is_none() {
            if let Some(url) = discover_reconnect_url(&root.path) {
                root.reconnect_url = Some(url);
                changed = true;
            }
        }
    }
    changed
}

pub fn discover_reconnect_url(path: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("/sbin/mount").output().ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        return reconnect_url_from_mount_output(path, &text);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        None
    }
}

/// Best-effort remount. The URL is generated by this module and contains no
/// password; macOS resolves saved credentials from Keychain/mount state.
/// `open` only acknowledges that LaunchServices accepted the URL, not that
/// Finder finished mounting it, so keep this blocking worker alive until the
/// configured root passes the same real-read probe used by health checks.
pub fn reconnect_network_root(root: &MediaRootSetting) -> std::io::Result<bool> {
    let Some(url) = root
        .reconnect_url
        .as_deref()
        .filter(|url| url.starts_with("smb://") || url.starts_with("afp://"))
    else {
        return Ok(false);
    };
    #[cfg(target_os = "macos")]
    {
        let status = Command::new("/usr/bin/open")
            .args(["-g", url])
            .status()
            .map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!("could not request network-share reconnect: {error}"),
                )
            })?;
        if !status.success() {
            return Ok(false);
        }

        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if media_root_readable(root).is_ok() {
                return Ok(true);
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!(
                "macOS accepted the reconnect request, but {} did not become readable within 60 seconds",
                root.path
            ),
        ));
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = url;
        Ok(false)
    }
}

/// Explicit recovery for a stale SMB mount. Unlike the background retry,
/// this user-triggered operation may force-unmount the exact affected volume
/// before reopening its saved password-free URL through macOS/Keychain.
pub fn repair_smb_root(root: &MediaRootSetting) -> Result<(), String> {
    let url = root
        .reconnect_url
        .as_deref()
        .filter(|url| url.starts_with("smb://"))
        .ok_or_else(|| "this media root does not have a saved SMB connection".to_string())?;

    #[cfg(target_os = "macos")]
    {
        let output = Command::new("/sbin/mount")
            .output()
            .map_err(|error| format!("could not inspect mounted volumes: {error}"))?;
        let mounts = String::from_utf8_lossy(&output.stdout);
        if let Some(mount_point) = smb_mount_point_for_path(&root.path, &mounts) {
            if !safe_smb_mount_point(&mount_point) {
                return Err(format!(
                    "refusing to force-unmount unexpected SMB path {mount_point}"
                ));
            }
            let status = Command::new("/usr/sbin/diskutil")
                .args(["unmount", "force", &mount_point])
                .status()
                .map_err(|error| format!("could not unmount stale SMB volume: {error}"))?;
            if !status.success() {
                return Err(format!(
                    "macOS could not unmount stale SMB volume {mount_point}"
                ));
            }
        }

        let status = Command::new("/usr/bin/open")
            .args(["-g", url])
            .status()
            .map_err(|error| format!("could not reopen SMB connection: {error}"))?;
        if !status.success() {
            return Err("macOS could not reopen the SMB connection".to_string());
        }

        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if media_root_readable(root).is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        return Err(format!(
            "SMB reopened but {} did not become readable within 60 seconds; complete any macOS credential prompt, then retry",
            root.path
        ));
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = url;
        Err("automatic SMB repair is currently available on macOS only".to_string())
    }
}

fn smb_mount_point_for_path(path: &str, output: &str) -> Option<String> {
    output
        .lines()
        .filter_map(|line| {
            let (source, remainder) = line.split_once(" on ")?;
            if !source.starts_with("//") {
                return None;
            }
            let (mount_point, _) = remainder.rsplit_once(" (")?;
            let mount_point = mount_point.replace("\\040", " ");
            let matches = path == mount_point
                || path
                    .strip_prefix(&mount_point)
                    .is_some_and(|rest| rest.starts_with('/'));
            matches.then_some((mount_point.len(), mount_point))
        })
        .max_by_key(|(length, _)| *length)
        .map(|(_, mount_point)| mount_point)
}

#[cfg(target_os = "macos")]
fn smb_root_is_mounted(root: &MediaRootSetting) -> bool {
    Command::new("/sbin/mount")
        .output()
        .ok()
        .is_some_and(|output| {
            smb_mount_point_for_path(&root.path, &String::from_utf8_lossy(&output.stdout)).is_some()
        })
}

fn safe_smb_mount_point(path: &str) -> bool {
    let path = Path::new(path);
    path.parent() == Some(Path::new("/Volumes"))
        && path.file_name().is_some_and(|name| !name.is_empty())
}

fn reconnect_url_from_mount_output(path: &str, output: &str) -> Option<String> {
    output
        .lines()
        .filter_map(|line| {
            let (source, remainder) = line.split_once(" on ")?;
            let (mount_point, _) = remainder.rsplit_once(" (")?;
            let mount_point = mount_point.replace("\\040", " ");
            let root_matches = path == mount_point
                || path
                    .strip_prefix(&mount_point)
                    .is_some_and(|rest| rest.starts_with('/'));
            if !root_matches {
                return None;
            }
            if !source.starts_with("//") {
                return None;
            }
            let authority_and_path = source.trim_start_matches('/');
            let sanitized = if let Some((userinfo, host_path)) = authority_and_path.split_once('@')
            {
                let user = userinfo.split(':').next().unwrap_or_default();
                if user.is_empty() {
                    host_path.to_string()
                } else {
                    format!("{user}@{host_path}")
                }
            } else {
                authority_and_path.to_string()
            };
            Some((mount_point.len(), format!("smb://{sanitized}")))
        })
        .max_by_key(|(mount_len, _)| *mount_len)
        .map(|(_, url)| url)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountedNetworkShare {
    pub path: String,
    pub reconnect_url: String,
}

/// Mount an SMB share selected in the desktop UI. SWARM never receives or
/// stores its password: macOS owns the prompt and its Keychain entry.
pub fn connect_smb_share(
    label: &str,
    server: &str,
    share: &str,
    username: Option<&str>,
) -> Result<MountedNetworkShare, String> {
    validate_network_share(label, server, share)?;
    if username.is_some_and(|value| {
        value.len() > 255
            || value
                .chars()
                .any(|character| matches!(character, '\0' | '\n' | '\r'))
    }) {
        return Err("enter a valid SMB username".to_string());
    }
    #[cfg(target_os = "macos")]
    {
        let _ = label;
        mount_smb_macos(server, share, username)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (label, server, share, username);
        Err("connecting SMB shares from SWARM is currently available on macOS; mount the SMB share with your operating system, then add its local folder path".to_string())
    }
}

fn validate_network_share(label: &str, server: &str, share: &str) -> Result<(), String> {
    if label.is_empty()
        || label.len() > 64
        || !label
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
    {
        return Err("network-share labels may contain only letters, numbers, hyphens, underscores, and periods".to_string());
    }
    if server.is_empty()
        || server.len() > 255
        || !server
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ".-_:[]%".contains(character))
    {
        return Err("enter a valid NAS hostname or IP address".to_string());
    }
    if share.trim_matches('/').is_empty()
        || share.len() > 1024
        || share
            .chars()
            .any(|character| character == '\0' || character == '\n' || character == '\r')
    {
        return Err("enter a valid SMB share name".to_string());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn mount_smb_macos(
    server: &str,
    share: &str,
    username: Option<&str>,
) -> Result<MountedNetworkShare, String> {
    let share = share.trim_matches('/');
    let url = smb_url(server, share, username);
    if let Some(path) = discover_smb_mount(server, share) {
        return Ok(MountedNetworkShare {
            path,
            reconnect_url: url,
        });
    }
    let status = Command::new("/usr/bin/open")
        .args(["-g", &url])
        .status()
        .map_err(|error| format!("could not open the SMB connection: {error}"))?;
    if !status.success() {
        return Err("macOS could not open the SMB connection".to_string());
    }

    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if let Some(path) = discover_smb_mount(server, share) {
            return Ok(MountedNetworkShare {
                path,
                reconnect_url: url,
            });
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err("SMB did not become available within 60 seconds; finish or retry the macOS credential prompt".to_string())
}

#[cfg(target_os = "macos")]
fn discover_smb_mount(server: &str, share: &str) -> Option<String> {
    let output = Command::new("/sbin/mount").output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    smb_mount_point_from_output(server, share, &text)
}

fn smb_mount_point_from_output(server: &str, share: &str, output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let (source, remainder) = line.split_once(" on ")?;
        let (mount_point, _) = remainder.rsplit_once(" (")?;
        if !source.starts_with("//") {
            return None;
        }
        let decoded_source = source.replace("\\040", " ");
        let host_path = decoded_source
            .trim_start_matches('/')
            .rsplit_once('@')
            .map_or(decoded_source.trim_start_matches('/'), |(_, value)| value);
        let (mounted_server, mounted_share) = host_path.split_once('/')?;
        (mounted_server.eq_ignore_ascii_case(server)
            && mounted_share
                .trim_matches('/')
                .eq_ignore_ascii_case(share.trim_matches('/')))
        .then(|| mount_point.replace("\\040", " "))
    })
}

fn smb_url(server: &str, share: &str, username: Option<&str>) -> String {
    let userinfo = username
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("{}@", percent_encode_url_component(value)))
        .unwrap_or_default();
    let encoded_share = share
        .split('/')
        .map(percent_encode_url_component)
        .collect::<Vec<_>>()
        .join("/");
    format!("smb://{userinfo}{server}/{encoded_share}")
}

fn percent_encode_url_component(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub media_roots: Vec<MediaRootSetting>,
    /// Superseded by `media_roots` — read-only now, kept only so an older
    /// settings.json (single-root, pre-this-field) still loads. Never
    /// written by this build; see `load`'s one-time upgrade below.
    #[serde(default)]
    media_root: Option<String>,
    pub tmdb_api_key: Option<String>,
    /// OpenSubtitles.com REST API key used only for user-requested subtitle
    /// search/downloads. The settings file is permission-restricted (0600).
    #[serde(default)]
    pub opensubtitles_api_key: Option<String>,
    /// Applies the measured upload budget to internet playback. LAN
    /// playback always bypasses it regardless of this setting. Missing-field
    /// (pre-toggle settings.json) default only — see the `Default` impl
    /// below for the brand-new-install default, which deliberately differs.
    #[serde(default = "default_streaming_upload_budget_enabled")]
    pub streaming_upload_budget_enabled: bool,
    /// Copy requested artwork into app data before serving it, avoiding
    /// repeated reads from slower network shares. Off unless opted in.
    #[serde(default)]
    pub artwork_disk_cache_enabled: bool,
    /// Generate English subtitles locally with Whisper. The model and queue
    /// live in app data; disabling pauses work without discarding progress.
    #[serde(default)]
    pub local_transcription_enabled: bool,
    /// Protect playback from Whisper's sustained CPU load. Users with CPU
    /// headroom can opt out and allow both workloads to run concurrently.
    #[serde(default = "default_transcription_pause_while_streaming")]
    pub transcription_pause_while_streaming: bool,
    /// Bulk-generation preference: when true, a movie or episode that
    /// already has any subtitle track (Whisper or downloaded) is left
    /// alone rather than queued/regenerated. A user-triggered per-item
    /// generation always runs regardless of this setting.
    #[serde(default)]
    pub transcription_skip_if_subtitles_exist: bool,
    /// Whether the read-only MCP server (see `mcp.rs`) starts alongside the
    /// GUI app's core. Takes effect on next launch/restart — no hot-reload,
    /// same posture as `media_root`'s pre-multi-root upgrade above having no
    /// live-apply path either.
    #[serde(default)]
    pub mcp_enabled: bool,
    #[serde(default = "default_mcp_port")]
    pub mcp_port: u16,
    /// Bearer token required by every MCP request.
    #[serde(default)]
    pub mcp_access_token: Option<String>,
    /// Periodically re-scan every media root for added/removed/updated
    /// files and, for anything newly found, automatically trigger metadata
    /// scraping (TMDb for movies/shows, MusicBrainz for music) — see
    /// `gui.rs`'s `start_auto_library_watch`. On by default: it reuses the
    /// same scan/scrape machinery a manual Rescan/Scrape button already
    /// runs, just on a timer, so there is no new heavy resource (unlike
    /// Whisper's model download) that would justify defaulting it off.
    #[serde(default = "default_auto_library_watch_enabled")]
    pub auto_library_watch_enabled: bool,
    /// Which H.264 encoder transcodes use: `"auto"` (hardware on macOS when
    /// available and healthy), `"hardware"` (pin VideoToolbox), or
    /// `"software"` (pin libx264). See `swarm_media::transcode::VideoEncoderMode`.
    #[serde(default = "default_video_encoder_mode")]
    pub video_encoder_mode: String,
    /// Server-imposed ceiling on transcode output height regardless of what a
    /// client advertises. `0` disables the cap (source/client-limited).
    #[serde(default)]
    pub max_transcode_height: u32,
    /// HLS segment length in seconds for transcoded playback. Shorter =
    /// faster start and rebuffer recovery, slightly more overhead. Clamped
    /// to >= 2 wherever it is consumed.
    #[serde(default = "default_hls_segment_seconds")]
    pub hls_segment_seconds: u32,
    /// How the app applies new releases from GitHub: `"off"`, `"notify"`
    /// (surface a banner; the user installs), or `"auto"` (download in the
    /// background and install on the next quit — never mid-session, since the
    /// server holds live playback connections). Defaults to `"notify"`.
    #[serde(default = "default_auto_update")]
    pub auto_update: String,
    /// Configured AI providers for the AI tab's advanced features (issue
    /// #235) — always exactly the three entries `default_ai_providers`
    /// seeds, present or not on disk. Adding a fourth provider later means
    /// extending that function, not this field's shape.
    #[serde(default = "default_ai_providers")]
    pub ai_providers: Vec<AiProviderSetting>,
    /// Gates `ai::scrape assist` (AI-assisted TMDb matching for entries the
    /// ordinary scrape pass couldn't match) — off until a user opts in, even
    /// once a provider is configured, since it makes outbound calls with
    /// filenames from the user's library.
    #[serde(default)]
    pub ai_scan_assist_enabled: bool,
    /// Gates the AI reorganize feature (`reorganize.rs`): scanning a media
    /// root and proposing a rename/move plan. The plan itself always still
    /// requires explicit per-run approval before anything on disk changes —
    /// this toggle only controls whether the feature is offered at all.
    #[serde(default)]
    pub ai_reorganize_enabled: bool,
}

fn default_streaming_upload_budget_enabled() -> bool {
    true
}

fn default_transcription_pause_while_streaming() -> bool {
    true
}

fn default_mcp_port() -> u16 {
    7890
}

fn default_auto_library_watch_enabled() -> bool {
    true
}

fn default_video_encoder_mode() -> String {
    "auto".to_string()
}

fn default_hls_segment_seconds() -> u32 {
    4
}

fn default_auto_update() -> String {
    "notify".to_string()
}

// Hand-written rather than `#[derive(Default)]` so a brand-new install (no
// settings.json at all, going through `Settings::default()` in `load` below)
// gets the exact same `mcp_port` default as an *existing* settings.json
// that simply predates this field (going through serde's per-field
// `#[serde(default = "default_mcp_port")]`) — a derived `Default` would
// silently disagree (`u16::default()` is `0`, not `default_mcp_port()`).
//
// `streaming_upload_budget_enabled` is the deliberate exception: a
// brand-new install has never run unthrottled, so it starts off (opt-in)
// rather than inheriting the `true` that only exists to keep upgrading
// installs' already-running behavior unchanged — see
// `default_streaming_upload_budget_enabled` and
// `older_settings_keep_the_upload_budget_enabled`.
impl Default for Settings {
    fn default() -> Self {
        Settings {
            media_roots: Vec::new(),
            media_root: None,
            tmdb_api_key: None,
            opensubtitles_api_key: None,
            streaming_upload_budget_enabled: false,
            artwork_disk_cache_enabled: false,
            local_transcription_enabled: false,
            transcription_pause_while_streaming: true,
            // Fresh-install default: a brand-new library has never had bulk
            // Whisper scanning run over it, so start in the conservative
            // "leave anything that already has a subtitle alone" mode. An
            // existing settings.json keeps whatever it had (serde
            // missing-field default below stays `false`) — same split as
            // `streaming_upload_budget_enabled` above.
            transcription_skip_if_subtitles_exist: true,
            mcp_enabled: false,
            mcp_port: default_mcp_port(),
            mcp_access_token: None,
            auto_library_watch_enabled: true,
            video_encoder_mode: default_video_encoder_mode(),
            max_transcode_height: 0,
            hls_segment_seconds: default_hls_segment_seconds(),
            auto_update: default_auto_update(),
            ai_providers: default_ai_providers(),
            ai_scan_assist_enabled: false,
            ai_reorganize_enabled: false,
        }
    }
}

fn settings_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("settings.json")
}

/// Loads persisted settings, transparently upgrading a pre-multi-root
/// settings.json (single `media_root: Option<String>`, no `media_roots`)
/// into the new shape in memory. Not written back to disk here — the next
/// `save` call (any settings change) persists the upgraded shape naturally.
pub fn load(app_data_dir: &Path) -> Settings {
    let mut settings: Settings = std::fs::read_to_string(settings_path(app_data_dir))
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default();
    if settings.media_roots.is_empty() {
        if let Some(path) = settings.media_root.take() {
            settings.media_roots.push(MediaRootSetting {
                label: "local".to_string(),
                path,
                reconnect_url: None,
                asset_type: RootAssetType::default(),
            });
        }
    }
    settings
}

pub fn save(app_data_dir: &Path, settings: &Settings) -> std::io::Result<()> {
    std::fs::create_dir_all(app_data_dir)?;
    let json = serde_json::to_string_pretty(settings).unwrap_or_default();
    let path = settings_path(app_data_dir);
    std::fs::write(&path, json)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_settings_keep_the_upload_budget_enabled() {
        let dir =
            std::env::temp_dir().join(format!("swarm-settings-test-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            settings_path(&dir),
            r#"{"media_roots":[{"label":"local","path":"/media"}],"tmdb_api_key":null,"mcp_enabled":false,"mcp_port":7890}"#,
        )
        .unwrap();
        let loaded = load(&dir);
        assert!(loaded.streaming_upload_budget_enabled);
        assert!(!loaded.artwork_disk_cache_enabled);
        assert!(!loaded.local_transcription_enabled);
        assert!(loaded.transcription_pause_while_streaming);
        assert!(!loaded.transcription_skip_if_subtitles_exist);
        assert_eq!(loaded.mcp_access_token, None);
        assert!(loaded.auto_library_watch_enabled);
        // Transcoding controls default in for a config that predates them.
        assert_eq!(loaded.video_encoder_mode, "auto");
        assert_eq!(loaded.max_transcode_height, 0);
        assert_eq!(loaded.hls_segment_seconds, 4);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fresh_install_defaults_the_upload_budget_off() {
        let dir =
            std::env::temp_dir().join(format!("swarm-settings-test-{}", rand::random::<u64>()));
        // No settings.json written at all — a genuinely first-ever run.
        let loaded = load(&dir);
        assert!(!loaded.streaming_upload_budget_enabled);
        // Whisper generation itself is opt-in, and a fresh library also
        // starts out skipping anything that already has subtitles.
        assert!(!loaded.local_transcription_enabled);
        assert!(loaded.transcription_skip_if_subtitles_exist);
        assert_eq!(loaded.video_encoder_mode, "auto");
        assert_eq!(loaded.hls_segment_seconds, 4);
    }

    #[test]
    fn artwork_disk_cache_preference_round_trips() {
        let dir = std::env::temp_dir().join(format!(
            "swarm-artwork-cache-settings-test-{}",
            rand::random::<u64>()
        ));
        let mut settings = Settings::default();
        settings.artwork_disk_cache_enabled = true;

        save(&dir, &settings).unwrap();

        assert!(load(&dir).artwork_disk_cache_enabled);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn root_asset_type_parses_the_ui_values_and_defaults_to_mixed() {
        assert_eq!(RootAssetType::parse(None).unwrap(), RootAssetType::Mixed);
        assert_eq!(RootAssetType::parse(Some("")).unwrap(), RootAssetType::Mixed);
        assert_eq!(RootAssetType::parse(Some("mixed")).unwrap(), RootAssetType::Mixed);
        assert_eq!(RootAssetType::parse(Some("movies")).unwrap(), RootAssetType::Movies);
        assert_eq!(RootAssetType::parse(Some("shows")).unwrap(), RootAssetType::Shows);
        assert_eq!(RootAssetType::parse(Some("tv")).unwrap(), RootAssetType::Shows);
        assert_eq!(RootAssetType::parse(Some("music")).unwrap(), RootAssetType::Music);
        assert!(RootAssetType::parse(Some("games")).is_err());
    }

    #[test]
    fn media_root_setting_without_asset_type_loads_as_mixed() {
        let json = r#"{"label":"local","path":"/media"}"#;
        let parsed: MediaRootSetting = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.asset_type, RootAssetType::Mixed);
    }

    #[test]
    fn media_root_health_reports_readable_and_missing_roots() {
        let dir =
            std::env::temp_dir().join(format!("swarm-root-health-test-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("disconnected-share");
        let roots = vec![
            MediaRootSetting {
                label: "local".into(),
                path: dir.to_string_lossy().into_owned(),
                reconnect_url: None,
                asset_type: RootAssetType::default(),
            },
            MediaRootSetting {
                label: "nas".into(),
                path: missing.to_string_lossy().into_owned(),
                reconnect_url: Some("smb://nas/share".into()),
                asset_type: RootAssetType::default(),
            },
        ];

        let health = media_root_health(&roots);
        assert!(health[0].available);
        assert_eq!(health[0].error, None);
        assert!(!health[0].permission_denied);
        assert!(!health[1].available);
        assert!(health[1].error.is_some());
        // A missing path is "not connected", never a permission problem.
        assert!(!health[1].permission_denied);
        assert!(health[1].auto_reconnect);
        assert_eq!(health[1].network_protocol.as_deref(), Some("SMB"));

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn media_root_health_flags_a_permission_denied_root() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir()
            .join(format!("swarm-root-denied-test-{}", rand::random::<u64>()));
        let locked = dir.join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("movie.mkv"), b"media").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let roots = vec![MediaRootSetting {
            label: "locked".into(),
            path: locked.to_string_lossy().into_owned(),
            reconnect_url: None,
            asset_type: RootAssetType::default(),
        }];
        let health = media_root_health(&roots);

        // Running as root ignores the mode bits; only assert when the read
        // actually got refused, which is the case this test exists for.
        if !health[0].available {
            assert!(
                health[0].permission_denied,
                "a refused read should classify as permission_denied, got {:?}",
                health[0].error
            );
        }

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn media_root_probe_reads_a_nested_representative_file() {
        let dir =
            std::env::temp_dir().join(format!("swarm-root-probe-test-{}", rand::random::<u64>()));
        let nested = dir.join("movies").join("Example Movie");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("movie.mkv"), b"media").unwrap();

        assert!(probe_media_root(&dir).is_ok());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn macos_mount_output_selects_longest_share_and_strips_password() {
        let mounts = "//guest:secret@nas.local/share on /Volumes/share (smbfs, nodev)\n//other@nas.local/archive on /Volumes/archive (smbfs)";
        assert_eq!(
            reconnect_url_from_mount_output("/Volumes/share/movies", mounts).as_deref(),
            Some("smb://guest@nas.local/share")
        );
    }

    #[test]
    fn smb_mount_matching_ignores_username_and_decodes_mount_point_spaces() {
        let mounts = "//jerrod@NAS.local/Media on /Volumes/NAS\\040Media (smbfs, nodev)";
        assert_eq!(
            smb_mount_point_from_output("nas.local", "media", mounts).as_deref(),
            Some("/Volumes/NAS Media")
        );
    }

    #[test]
    fn smb_repair_resolves_only_the_containing_mounted_volume() {
        let mounts = "//user@nas.local/media on /Volumes/media (smbfs, nodev)\n//user@nas.local/media2 on /Volumes/media2 (smbfs, nodev)\n/dev/disk3s1 on /Volumes/local (apfs)";
        assert_eq!(
            smb_mount_point_for_path("/Volumes/media/movies", mounts).as_deref(),
            Some("/Volumes/media")
        );
        assert!(safe_smb_mount_point("/Volumes/media"));
        assert!(!safe_smb_mount_point("/"));
        assert!(!safe_smb_mount_point("/Volumes"));
        assert!(!safe_smb_mount_point("/Users/example/media"));
    }

    #[test]
    fn smb_url_encodes_user_and_share_without_accepting_a_password() {
        assert_eq!(
            smb_url("nas.local", "Family Media/Movies", Some("media user")),
            "smb://media%20user@nas.local/Family%20Media/Movies"
        );
    }

    #[test]
    fn network_share_validation_rejects_shell_metacharacters_in_server() {
        assert!(validate_network_share("movies", "nas.local;touch bad", "/media").is_err());
        assert!(validate_network_share("movies", "nas.local", "/media").is_ok());
    }
}
