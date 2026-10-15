//! Persisted GUI settings: media root(s) and TMDb API key. Kept separate
//! from `ServerCore`'s own state (STUN link, library) because these are
//! needed *before* a core can even be constructed. The packaged desktop app
//! deliberately has no environment-only media-root configuration path.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Mutex};
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
    PhotosVideos,
}

impl RootAssetType {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim).unwrap_or("") {
            "" => Err("choose an asset type".to_string()),
            "mixed" => Err("choose a specific asset type".to_string()),
            "movies" => Ok(RootAssetType::Movies),
            "shows" | "tv" => Ok(RootAssetType::Shows),
            "music" => Ok(RootAssetType::Music),
            "photos_videos" | "photos-videos" => Ok(RootAssetType::PhotosVideos),
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
            RootAssetType::PhotosVideos => "Photos & videos",
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
    pub network_protocol: Option<String>,
}

pub fn media_root_health(roots: &[MediaRootSetting]) -> Vec<MediaRootHealth> {
    roots
        .iter()
        .map(|root| {
            let network_protocol = root.reconnect_url.as_deref().and_then(network_protocol);
            match probe_root(root) {
                Ok(_) => MediaRootHealth {
                    label: root.label.clone(),
                    path: root.path.clone(),
                    available: true,
                    error: None,
                    permission_denied: false,
                    network_protocol,
                },
                Err(error) => MediaRootHealth {
                    label: root.label.clone(),
                    path: root.path.clone(),
                    available: false,
                    permission_denied: is_permission_denied(&error),
                    error: Some(error.to_string()),
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

/// How long one real read of a media root may take before it counts as
/// unavailable. A stale SMB session can block a read indefinitely.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Paths whose read is still running after its timeout. A blocked read cannot
/// be cancelled, so a new probe of the same path is refused until the old one
/// returns, rather than stacking one stuck thread per health poll.
static PROBES_IN_FLIGHT: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn probe_root(root: &MediaRootSetting) -> std::io::Result<()> {
    let root = root.clone();
    let key = PathBuf::from(&root.path);
    bounded_probe(key, PROBE_TIMEOUT, move || media_root_readable(&root))
}

fn bounded_probe<F>(key: PathBuf, timeout: Duration, probe: F) -> std::io::Result<()>
where
    F: FnOnce() -> std::io::Result<()> + Send + 'static,
{
    {
        let mut in_flight = PROBES_IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        if in_flight.contains(&key) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("an earlier read of {} is still pending", key.display()),
            ));
        }
        in_flight.push(key.clone());
    }

    let (sender, receiver) = mpsc::channel();
    let worker_key = key.clone();
    std::thread::spawn(move || {
        let result = probe();
        PROBES_IN_FLIGHT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|path| path != &worker_key);
        let _ = sender.send(result);
    });

    match receiver.recv_timeout(timeout) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!(
                "reading {} took longer than {} seconds",
                key.display(),
                timeout.as_secs()
            ),
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(std::io::Error::other(format!(
            "the read of {} stopped unexpectedly",
            key.display()
        ))),
    }
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

/// Serializes manual and automatic repairs. Two overlapping runs would fight
/// over the same mount points and could each leave a fresh copy behind.
static SMB_REPAIR_LOCK: Mutex<()> = Mutex::new(());

/// Make a stale SMB media root readable again. Used by the Repair SMB button
/// and by the recovery loop's automatic reconnect (see `SmbReconnectPolicy`).
/// Returns the path the root is readable at, which differs from `root.path`
/// when the share is mounted under another name (the caller re-points the
/// root there).
///
/// macOS names a second mount of a share `share-1`, `share-2`, and so on
/// whenever the original name is taken, so the share often comes back under a
/// different folder than the one configured. The steps, in order:
///
/// 1. A live, readable copy of the share anywhere is adopted as is.
/// 2. Otherwise every live copy is force-unmounted, because macOS refuses to
///    mount a share a second time while an old session is alive.
/// 3. The saved URL is reopened, and whichever copy becomes readable wins.
///
/// Only mounts on their own device count as live. After a bad unmount macOS can
/// leave a plain directory at the old mount point and a ghost entry in the
/// mount table; neither can be unmounted and neither holds a session.
pub fn repair_smb_root(root: &MediaRootSetting) -> Result<String, String> {
    let url = root
        .reconnect_url
        .as_deref()
        .filter(|url| url.starts_with("smb://"))
        .ok_or_else(|| "this media root does not have a saved SMB connection".to_string())?;
    let _repair = SMB_REPAIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    #[cfg(target_os = "macos")]
    {
        let live = live_smb_mounts(url)?;
        if let Some(path) = readable_path_on_mounts(root, &live) {
            return Ok(path);
        }

        for mount_point in &live {
            if !safe_smb_mount_point(mount_point) {
                return Err(format!(
                    "refusing to force-unmount unexpected SMB path {mount_point}"
                ));
            }
            if !run_bounded(
                "/usr/sbin/diskutil",
                &["unmount", "force", mount_point],
                SMB_UNMOUNT_TIMEOUT,
            )? {
                return Err(format!(
                    "macOS could not unmount stale SMB volume {mount_point}"
                ));
            }
        }
        if !live_smb_mounts(url)?.is_empty() {
            return Err(format!(
                "a copy of {url} is still mounted after unmounting; restart the Mac if it persists"
            ));
        }

        if !run_bounded("/usr/bin/open", &["-g", url], Duration::from_secs(15))? {
            return Err("macOS could not reopen the SMB connection".to_string());
        }

        let deadline = Instant::now() + SMB_REPAIR_WAIT;
        while Instant::now() < deadline {
            if probe_root(root).is_ok() {
                return Ok(root.path.clone());
            }
            // The configured name may be taken by a leftover directory, in
            // which case macOS mounts the share beside it instead.
            if let Some(path) =
                readable_path_on_mounts(root, &live_smb_mounts(url).unwrap_or_default())
            {
                return Ok(path);
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        Err(format!(
            "SMB reopened but {} did not become readable within {} seconds; complete any macOS credential prompt, then retry",
            root.path,
            SMB_REPAIR_WAIT.as_secs()
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = url;
        Err("automatic SMB repair is currently available on macOS only".to_string())
    }
}

#[cfg(target_os = "macos")]
const SMB_UNMOUNT_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(target_os = "macos")]
const SMB_REPAIR_WAIT: Duration = Duration::from_secs(60);

#[cfg(target_os = "macos")]
fn list_mounts() -> Result<String, String> {
    let output = Command::new("/sbin/mount")
        .output()
        .map_err(|error| format!("could not inspect mounted volumes: {error}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Mount points of the live mounts of `url`'s share, ghost entries excluded.
#[cfg(target_os = "macos")]
fn live_smb_mounts(url: &str) -> Result<Vec<String>, String> {
    Ok(smb_mount_points_for_share(url, &list_mounts()?)
        .into_iter()
        .filter(|mount_point| is_live_mount(mount_point))
        .collect())
}

/// Whether `mount_point` is a real mounted volume rather than a leftover
/// directory still named in the mount table. The check is bounded, since a
/// wedged SMB session can block even a `stat` of its mount root.
#[cfg(target_os = "macos")]
fn is_live_mount(mount_point: &str) -> bool {
    let path = PathBuf::from(mount_point);
    bounded_probe(
        PathBuf::from(format!("{mount_point}#device")),
        PROBE_TIMEOUT,
        move || {
            if differs_from_parent_device(&path, Path::new("/Volumes"))? {
                Ok(())
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "not a mounted volume",
                ))
            }
        },
    )
    .is_ok()
}

/// A mounted volume reports a different device id than the folder it is
/// mounted in; a plain directory reports the same one.
#[cfg(all(unix, any(target_os = "macos", test)))]
fn differs_from_parent_device(path: &Path, parent: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    Ok(std::fs::metadata(path)?.dev() != std::fs::metadata(parent)?.dev())
}

/// The path `root` is readable at on the first of `mounts` that serves it,
/// found by swapping the root's own `/Volumes/<name>` prefix for each mount.
#[cfg(target_os = "macos")]
fn readable_path_on_mounts(root: &MediaRootSetting, mounts: &[String]) -> Option<String> {
    let old_mount = volume_mount_point(&root.path)?;
    mounts.iter().find_map(|mount_point| {
        let candidate = rebase_path(&root.path, &old_mount, mount_point)?;
        let mut probe = root.clone();
        probe.path = candidate.clone();
        probe_root(&probe).is_ok().then_some(candidate)
    })
}

/// The `/Volumes/<name>` folder macOS mounts a share on, taken from a path
/// beneath it.
pub fn volume_mount_point(path: &str) -> Option<String> {
    let name = path
        .strip_prefix("/Volumes/")?
        .split('/')
        .next()
        .filter(|name| !name.is_empty())?;
    Some(format!("/Volumes/{name}"))
}

/// `path` with its leading `old_mount` folder replaced by `new_mount`.
fn rebase_path(path: &str, old_mount: &str, new_mount: &str) -> Option<String> {
    let rest = path.strip_prefix(old_mount)?;
    (rest.is_empty() || rest.starts_with('/')).then(|| format!("{new_mount}{rest}"))
}

/// Move every root on the share `share_url` that lives under `old_mount` to
/// `new_mount`. Library entries are stored relative to their root, so nothing
/// in the catalog changes. Returns whether any root moved.
pub fn repoint_share_roots(
    roots: &mut [MediaRootSetting],
    share_url: &str,
    old_mount: &str,
    new_mount: &str,
) -> bool {
    let mut moved = false;
    for root in roots
        .iter_mut()
        .filter(|root| root.reconnect_url.as_deref() == Some(share_url))
    {
        if let Some(path) = rebase_path(&root.path, old_mount, new_mount) {
            root.path = path;
            moved = true;
        }
    }
    moved
}

/// Run a system command, killing it if it outlives `timeout`. A command that
/// touches a wedged SMB session can block without end, which would otherwise
/// hold the repair lock forever. Returns whether the command exited 0.
#[cfg(target_os = "macos")]
fn run_bounded(program: &str, args: &[&str], timeout: Duration) -> Result<bool, String> {
    let mut child = Command::new(program)
        .args(args)
        .spawn()
        .map_err(|error| format!("could not run {program}: {error}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.success()),
            Ok(None) if Instant::now() >= deadline => {
                // Deliberately not waited on: a process stuck in the kernel
                // cannot be reaped until its I/O returns.
                let _ = child.kill();
                return Err(format!(
                    "{program} did not finish within {} seconds",
                    timeout.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(error) => return Err(format!("could not wait for {program}: {error}")),
        }
    }
}

/// Canonical `host/share` for an SMB URL (`smb://user@host/share`) or a mount
/// source (`//user:pass@host/share`). The user is dropped and the result is
/// lowercased, so both spellings of one share compare equal.
#[cfg(any(target_os = "macos", test))]
fn smb_share_identity(value: &str) -> Option<String> {
    let rest = value
        .strip_prefix("smb://")
        .or_else(|| value.strip_prefix("//"))?;
    let host_path = rest.rsplit_once('@').map_or(rest, |(_, host_path)| host_path);
    Some(host_path.trim_end_matches('/').to_ascii_lowercase())
}

/// Mount points of every SMB mount serving the same share as `url`, including
/// duplicates macOS created as `share-1`, `share-2`, and so on.
#[cfg(any(target_os = "macos", test))]
fn smb_mount_points_for_share(url: &str, mounts: &str) -> Vec<String> {
    let Some(wanted) = smb_share_identity(url) else {
        return Vec::new();
    };
    mounts
        .lines()
        .filter_map(|line| {
            let (source, remainder) = line.split_once(" on ")?;
            let (mount_point, _) = remainder.rsplit_once(" (")?;
            (smb_share_identity(source)? == wanted).then(|| mount_point.replace("\\040", " "))
        })
        .collect()
}

/// Decides when SWARM may remount an unreadable SMB media root without the
/// user. A dropped share usually returns within seconds, so a share must stay
/// unreadable for `SMB_RECONNECT_GRACE` before the first attempt. Failed
/// attempts back off exponentially. After `SMB_RECONNECT_MAX_FAILURES` in a
/// row the policy stops until the share reads again or the user presses Repair.
#[derive(Default)]
pub struct SmbReconnectPolicy {
    shares: HashMap<String, ShareReconnectState>,
}

#[derive(Default)]
struct ShareReconnectState {
    unreadable_since: Option<Instant>,
    failures: u32,
    next_attempt_at: Option<Instant>,
    in_progress: bool,
    gave_up: bool,
}

pub const SMB_RECONNECT_GRACE: Duration = Duration::from_secs(20);
pub const SMB_RECONNECT_MAX_FAILURES: u32 = 5;
const SMB_RECONNECT_BASE_BACKOFF: Duration = Duration::from_secs(60);

impl SmbReconnectPolicy {
    /// Record that `share` (its reconnect URL) was seen unreadable at `now`.
    pub fn observe_unreadable(&mut self, share: &str, now: Instant) {
        self.shares
            .entry(share.to_string())
            .or_default()
            .unreadable_since
            .get_or_insert(now);
    }

    /// Consecutive failed attempts for `share` since it last read.
    pub fn failure_count(&self, share: &str) -> u32 {
        self.shares.get(share).map_or(0, |state| state.failures)
    }

    /// The share reads again, so every retry and give-up state is cleared.
    pub fn observe_readable(&mut self, share: &str) {
        self.shares.remove(share);
    }

    /// Whether to remount `share` now. A `true` answer marks the attempt in
    /// progress, so a second caller cannot start an overlapping one.
    pub fn try_begin(&mut self, share: &str, now: Instant) -> bool {
        let Some(state) = self.shares.get_mut(share) else {
            return false;
        };
        let Some(since) = state.unreadable_since else {
            return false;
        };
        if state.in_progress
            || state.gave_up
            || now.duration_since(since) < SMB_RECONNECT_GRACE
            || state.next_attempt_at.is_some_and(|at| now < at)
        {
            return false;
        }
        state.in_progress = true;
        true
    }

    /// Record the outcome of an attempt. Returns `true` when this failure
    /// used up the retries, so the caller can tell the user once.
    pub fn finish(&mut self, share: &str, now: Instant, succeeded: bool) -> bool {
        let state = self.shares.entry(share.to_string()).or_default();
        state.in_progress = false;
        if succeeded {
            state.failures = 0;
            state.next_attempt_at = None;
            state.unreadable_since = None;
            return false;
        }
        state.failures += 1;
        let backoff = SMB_RECONNECT_BASE_BACKOFF * 2u32.saturating_pow(state.failures - 1);
        state.next_attempt_at = Some(now + backoff);
        if state.failures >= SMB_RECONNECT_MAX_FAILURES {
            state.gave_up = true;
            return true;
        }
        false
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
    #[serde(default = "default_mcp_port")]
    pub mcp_port: u16,
    /// Bearer token required by every MCP request. The MCP server (see
    /// `mcp.rs`) starts alongside the GUI app's core whenever this is set —
    /// there is no separate enable toggle; creating a token is the enable
    /// action. Takes effect on next launch/restart — no hot-reload, same
    /// posture as `media_root`'s pre-multi-root upgrade above having no
    /// live-apply path either.
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
    /// Verify unchanged files by content after the fast path. Off by
    /// default because it intentionally performs disk reads for every file.
    #[serde(default)]
    pub comprehensive_check: bool,
    /// Index individual audio files. Off by default so music roots are
    /// handled at the artist/album level unless explicitly requested.
    #[serde(default)]
    pub scan_music_tracks: bool,
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
    /// How the app applies new releases from GitHub: `"notify"` (surface a
    /// banner; the user installs) or `"auto"` ("Automatically" in the UI —
    /// wait for zero active playback/transcode sessions, then download and
    /// install). A literal `"off"` — removed in issue #309 — upgrades to
    /// `"notify"` on load; see `load`. Defaults to `"notify"`.
    #[serde(default = "default_auto_update")]
    pub auto_update: String,
    /// Configured AI providers for the AI tab's advanced features (issue
    /// #235) — always exactly the three entries `default_ai_providers`
    /// seeds, present or not on disk. Adding a fourth provider later means
    /// extending that function, not this field's shape.
    #[serde(default = "default_ai_providers")]
    pub ai_providers: Vec<AiProviderSetting>,
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
            mcp_port: default_mcp_port(),
            mcp_access_token: None,
            auto_library_watch_enabled: true,
            comprehensive_check: false,
            scan_music_tracks: false,
            video_encoder_mode: default_video_encoder_mode(),
            max_transcode_height: 0,
            hls_segment_seconds: default_hls_segment_seconds(),
            auto_update: default_auto_update(),
            ai_providers: default_ai_providers(),
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
    // Issue #309: "Don't check for updates" was removed from the UI — every
    // update is either applied automatically once playback is idle, or the
    // user is notified and installs manually. A pre-#309 settings.json still
    // holding the literal "off" upgrades to "notify" in memory, same
    // not-written-back-until-the-next-save pattern as the media_root upgrade
    // above.
    if settings.auto_update == "off" {
        settings.auto_update = default_auto_update();
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
        assert!(!loaded.comprehensive_check);
        assert!(!loaded.scan_music_tracks);
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
        assert!(!loaded.comprehensive_check);
        assert!(!loaded.scan_music_tracks);
    }

    #[test]
    fn auto_update_off_migrates_to_notify_on_load() {
        let dir = std::env::temp_dir().join(format!(
            "swarm-auto-update-settings-test-{}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            settings_path(&dir),
            r#"{"media_roots":[],"tmdb_api_key":null,"auto_update":"off"}"#,
        )
        .unwrap();

        assert_eq!(load(&dir).auto_update, "notify");
        std::fs::remove_dir_all(dir).unwrap();
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
    fn root_asset_type_requires_and_parses_specific_ui_values() {
        assert!(RootAssetType::parse(None).is_err());
        assert!(RootAssetType::parse(Some("")).is_err());
        assert!(RootAssetType::parse(Some("mixed")).is_err());
        assert_eq!(RootAssetType::parse(Some("movies")).unwrap(), RootAssetType::Movies);
        assert_eq!(RootAssetType::parse(Some("shows")).unwrap(), RootAssetType::Shows);
        assert_eq!(RootAssetType::parse(Some("tv")).unwrap(), RootAssetType::Shows);
        assert_eq!(RootAssetType::parse(Some("music")).unwrap(), RootAssetType::Music);
        assert_eq!(
            RootAssetType::parse(Some("photos_videos")).unwrap(),
            RootAssetType::PhotosVideos
        );
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

    #[test]
    fn smb_share_identity_matches_the_url_and_the_mount_source() {
        assert_eq!(
            smb_share_identity("smb://GUEST@batocera.local/share").as_deref(),
            Some("batocera.local/share")
        );
        assert_eq!(
            smb_share_identity("//GUEST:@Batocera.local/share").as_deref(),
            Some("batocera.local/share")
        );
        assert_eq!(smb_share_identity("/dev/disk3s5"), None);
    }

    #[test]
    fn smb_mount_points_for_share_includes_duplicate_copies_only() {
        let mounts = "\
//GUEST:@batocera.local/share on /Volumes/share (smbfs, nodev, nosuid, noowners, mounted by Jerrod)
//GUEST:@batocera.local/share on /Volumes/share-1 (smbfs, nodev, nosuid, noowners, quarantine, mounted by Jerrod)
//GUEST:@batocera.local/other on /Volumes/other (smbfs, nodev, mounted by Jerrod)
/dev/disk3s5 on /System/Volumes/Data (apfs, local, journaled)
";
        assert_eq!(
            smb_mount_points_for_share("smb://GUEST@batocera.local/share", mounts),
            vec!["/Volumes/share".to_string(), "/Volumes/share-1".to_string()]
        );
    }

    #[test]
    fn volume_mount_point_is_the_first_folder_under_volumes() {
        assert_eq!(
            volume_mount_point("/Volumes/share/movies").as_deref(),
            Some("/Volumes/share")
        );
        assert_eq!(
            volume_mount_point("/Volumes/share-1").as_deref(),
            Some("/Volumes/share-1")
        );
        assert_eq!(volume_mount_point("/Users/me/movies"), None);
        assert_eq!(volume_mount_point("/Volumes/"), None);
    }

    #[test]
    fn rebase_path_swaps_only_a_whole_leading_folder() {
        assert_eq!(
            rebase_path("/Volumes/share/movies", "/Volumes/share", "/Volumes/share-1").as_deref(),
            Some("/Volumes/share-1/movies")
        );
        assert_eq!(
            rebase_path("/Volumes/share", "/Volumes/share", "/Volumes/share-1").as_deref(),
            Some("/Volumes/share-1")
        );
        // "/Volumes/share-1" must not be treated as being under "/Volumes/share".
        assert_eq!(
            rebase_path("/Volumes/share-1/movies", "/Volumes/share", "/Volumes/x"),
            None
        );
    }

    #[test]
    fn repoint_share_roots_moves_only_roots_on_that_share_and_mount() {
        let root = |label: &str, path: &str, url: Option<&str>| MediaRootSetting {
            label: label.into(),
            path: path.into(),
            reconnect_url: url.map(str::to_string),
            asset_type: RootAssetType::default(),
        };
        let share = "smb://GUEST@batocera.local/share";
        let mut roots = vec![
            root("movies", "/Volumes/share/movies", Some(share)),
            root("music", "/Volumes/share/music", Some(share)),
            root("other", "/Volumes/share/other", Some("smb://nas/elsewhere")),
            root("local", "/Users/me/media", None),
        ];

        assert!(repoint_share_roots(&mut roots, share, "/Volumes/share", "/Volumes/share-1"));
        assert_eq!(roots[0].path, "/Volumes/share-1/movies");
        assert_eq!(roots[1].path, "/Volumes/share-1/music");
        assert_eq!(roots[2].path, "/Volumes/share/other");
        assert_eq!(roots[3].path, "/Users/me/media");
        assert!(!repoint_share_roots(&mut roots, share, "/Volumes/share", "/Volumes/share-2"));
    }

    #[test]
    #[cfg(unix)]
    fn a_plain_directory_is_not_a_separate_device_from_its_parent() {
        let parent =
            std::env::temp_dir().join(format!("swarm-device-test-{}", rand::random::<u64>()));
        let child = parent.join("leftover");
        std::fs::create_dir_all(&child).unwrap();

        assert!(!differs_from_parent_device(&child, &parent).unwrap());
        assert!(differs_from_parent_device(&parent.join("missing"), &parent).is_err());

        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn smb_failure_count_resets_when_the_share_reads_again() {
        let share = "smb://GUEST@batocera.local/share";
        let start = Instant::now();
        let mut policy = SmbReconnectPolicy::default();
        policy.observe_unreadable(share, start);
        assert!(policy.try_begin(share, start + SMB_RECONNECT_GRACE));
        policy.finish(share, start + SMB_RECONNECT_GRACE, false);
        assert_eq!(policy.failure_count(share), 1);
        policy.observe_readable(share);
        assert_eq!(policy.failure_count(share), 0);
    }

    #[test]
    fn smb_reconnect_waits_out_the_grace_period_then_backs_off() {
        let share = "smb://GUEST@batocera.local/share";
        let start = Instant::now();
        let mut policy = SmbReconnectPolicy::default();
        policy.observe_unreadable(share, start);

        assert!(!policy.try_begin(share, start + SMB_RECONNECT_GRACE - Duration::from_secs(1)));
        let first = start + SMB_RECONNECT_GRACE;
        assert!(policy.try_begin(share, first));
        assert!(
            !policy.try_begin(share, first),
            "an attempt in progress must block an overlapping one"
        );
        assert!(!policy.finish(share, first, false));
        assert!(!policy.try_begin(share, first + Duration::from_secs(59)));
        assert!(policy.try_begin(share, first + Duration::from_secs(60)));
    }

    #[test]
    fn smb_reconnect_gives_up_after_consecutive_failures_until_readable_again() {
        let share = "smb://GUEST@batocera.local/share";
        let start = Instant::now();
        let mut policy = SmbReconnectPolicy::default();
        policy.observe_unreadable(share, start);

        let mut now = start + SMB_RECONNECT_GRACE;
        for attempt in 1..=SMB_RECONNECT_MAX_FAILURES {
            assert!(policy.try_begin(share, now), "attempt {attempt} should start");
            let gave_up = policy.finish(share, now, false);
            assert_eq!(gave_up, attempt == SMB_RECONNECT_MAX_FAILURES);
            now += Duration::from_secs(3600);
        }
        assert!(
            !policy.try_begin(share, now),
            "automatic retries must stop after giving up"
        );

        policy.observe_readable(share);
        policy.observe_unreadable(share, now);
        assert!(policy.try_begin(share, now + SMB_RECONNECT_GRACE));
    }

    #[test]
    fn smb_reconnect_success_restarts_the_grace_period_for_the_next_outage() {
        let share = "smb://GUEST@batocera.local/share";
        let start = Instant::now();
        let mut policy = SmbReconnectPolicy::default();
        policy.observe_unreadable(share, start);
        assert!(policy.try_begin(share, start + SMB_RECONNECT_GRACE));
        assert!(!policy.finish(share, start + SMB_RECONNECT_GRACE, true));

        let next_outage = start + Duration::from_secs(600);
        policy.observe_unreadable(share, next_outage);
        assert!(!policy.try_begin(share, next_outage));
        assert!(policy.try_begin(share, next_outage + SMB_RECONNECT_GRACE));
    }

    #[test]
    fn bounded_probe_times_out_and_refuses_a_second_read_of_the_same_path() {
        let key = PathBuf::from("/bounded-probe-test/stalled-share");
        let (release, stalled) = mpsc::channel::<()>();
        let result = bounded_probe(key.clone(), Duration::from_millis(50), move || {
            let _ = stalled.recv();
            Ok(())
        });
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);

        let refused = bounded_probe(key.clone(), Duration::from_millis(50), || -> std::io::Result<()> {
            panic!("a second read must not start while the first is stalled")
        });
        assert_eq!(refused.unwrap_err().kind(), std::io::ErrorKind::TimedOut);

        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match bounded_probe(key.clone(), Duration::from_secs(1), || Ok(())) {
                Ok(()) => break,
                Err(error) if Instant::now() < deadline => {
                    let _ = error;
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("the released read should clear the path: {error}"),
            }
        }
    }
}
