//! Read-only MCP (Model Context Protocol) server — lets an AI client
//! (Claude Desktop, Claude Code, or any other MCP client) query this
//! server's library, swarm roster, and recent client errors over
//! Streamable HTTP. See `apps/server/ui`'s "AI" dashboard tab for what MCP
//! is and how to point a client at it.
//!
//! It starts alongside `ServerCore` whenever `Settings::mcp_access_token` is
//! set — there is no separate enable toggle, creating a token is the enable
//! action — and shares the desktop application's lifecycle, including while
//! hidden.
//!
//! Every tool here is read-only by design (v1 scope, decided explicitly):
//! an AI client can search/inspect the library and check swarm/error state,
//! but cannot trigger a rescan/rescrape or change any setting. Mutating
//! tools are a real, separate trust decision (an AI client acting on your
//! library, not just reading it) — left for a later version if it turns
//! out to be wanted.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::ServerInfo;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use swarm_core::peer::MediaKind;

use crate::ServerCore;

fn kind_str(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Movie => "movie",
        MediaKind::Episode => "episode",
        MediaKind::Track => "track",
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchLibraryParams {
    /// Free-text search against title, show name, and artist.
    pub query: Option<String>,
    /// Restrict to one kind: "movie", "episode", or "track".
    pub kind: Option<String>,
    /// Restrict to entries carrying this exact genre/category.
    pub genre: Option<String>,
    /// Restrict to entries carrying this exact content rating (e.g. "PG-13", "TV-MA").
    pub rating: Option<String>,
    /// Only entries liked by at least one device.
    pub liked_only: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchResultEntry {
    /// Pass this to `get_entry_details` for the full record.
    pub entry_key: String,
    pub title: String,
    pub kind: String,
    pub year: Option<u32>,
    pub genres: Vec<String>,
    pub rating: Option<String>,
    /// Provider community score normalized to 0–10.
    pub community_rating: Option<f64>,
    pub community_rating_votes: Option<u64>,
    pub like_count: u32,
    /// Filename exactly as it exists beneath the configured media root.
    pub file_name: String,
    /// Stored root-relative path, including the root label on multi-root servers.
    pub relative_path: String,
}

/// Caps how much of a large library one `search_library` call can return —
/// this is a browsing/lookup tool for an AI conversation, not a bulk export;
/// a narrower `query`/`genre`/`kind` is the right way to find more than this
/// many matches.
const SEARCH_RESULT_LIMIT: usize = 200;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EntryKeyParams {
    /// The `entry_key` returned by `search_library`.
    pub entry_key: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EntryDetails {
    pub entry_key: String,
    pub title: String,
    pub kind: String,
    pub year: Option<u32>,
    pub genres: Vec<String>,
    pub rating: Option<String>,
    /// Provider community score normalized to 0–10.
    pub community_rating: Option<f64>,
    pub community_rating_votes: Option<u64>,
    pub like_count: u32,
    pub overview: Option<String>,
    /// `"Name as Character"`, or bare `"Name"` when no character is on file.
    pub cast: Vec<String>,
    pub show_title: Option<String>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub file_name: String,
    pub folder: String,
    pub relative_path: String,
    pub absolute_path: String,
    pub media_root_label: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowseMediaFilesParams {
    /// Optional case-insensitive folder/path fragment to narrow the result.
    pub path_prefix: Option<String>,
    /// Optional configured media-root label.
    pub root_label: Option<String>,
    /// Maximum results (defaults to 200, capped at 500).
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MediaFileInfo {
    pub entry_key: String,
    pub kind: String,
    pub title: String,
    pub media_root_label: String,
    pub file_name: String,
    pub folder: String,
    pub relative_path: String,
    pub absolute_path: String,
    pub size_bytes: u64,
}

fn path_parts(relative_path: &str) -> (String, String) {
    let path = Path::new(relative_path);
    let file_name = path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    let folder = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string());
    (file_name, folder)
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SwarmDeviceInfo {
    pub name: String,
    pub device_type: String,
    pub online: bool,
    /// RFC 3339, `None` if this device has never been seen online.
    pub last_seen_at: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ClientErrorInfo {
    pub device_name: String,
    pub asset_title: Option<String>,
    pub message: String,
    pub context: Option<String>,
    /// Unix milliseconds, client-observed wall-clock time.
    pub occurred_at_ms: i64,
}

#[derive(Clone)]
pub struct McpServer {
    core: Arc<ServerCore>,
}

// No stored `ToolRouter` field: `#[tool_handler]` below defaults to calling
// the `Self::tool_router()` associated function this macro generates fresh
// per dispatch (see its own doc comment on the `router` attribute) — a
// cached field is only needed when overriding that default via
// `#[tool_handler(router = self.tool_router)]`, which this doesn't.
#[tool_router]
impl McpServer {
    pub fn new(core: Arc<ServerCore>) -> Self {
        Self { core }
    }

    #[tool(
        description = "Search the media library by title, kind, genre, content rating, and/or liked status. Returns at most 200 matches — narrow the query if you need more precision."
    )]
    async fn search_library(
        &self,
        Parameters(params): Parameters<SearchLibraryParams>,
    ) -> Result<Json<Vec<SearchResultEntry>>, String> {
        let entries = self.core.library.list().await.map_err(|e| e.to_string())?;
        let like_counts = self
            .core
            .library
            .like_counts()
            .await
            .map_err(|e| e.to_string())?;
        let query = params.query.as_deref().map(str::to_lowercase);
        let results = entries
            .into_iter()
            .filter(|e| {
                params
                    .kind
                    .as_deref()
                    .is_none_or(|k| kind_str(e.kind).eq_ignore_ascii_case(k))
            })
            .filter(|e| {
                params
                    .genre
                    .as_deref()
                    .is_none_or(|g| e.genres.iter().any(|eg| eg.eq_ignore_ascii_case(g)))
            })
            .filter(|e| {
                params.rating.as_deref().is_none_or(|r| {
                    e.rating
                        .as_deref()
                        .is_some_and(|er| er.eq_ignore_ascii_case(r))
                })
            })
            .filter(|e| {
                !params.liked_only.unwrap_or(false)
                    || like_counts.get(&e.entry_key).copied().unwrap_or(0) > 0
            })
            .filter(|e| {
                query.as_deref().is_none_or(|q| {
                    let title = e
                        .scraped_title
                        .as_deref()
                        .unwrap_or(&e.title)
                        .to_lowercase();
                    title.contains(q)
                        || e.show_title
                            .as_deref()
                            .is_some_and(|s| s.to_lowercase().contains(q))
                        || e.artist
                            .as_deref()
                            .is_some_and(|a| a.to_lowercase().contains(q))
                })
            })
            .take(SEARCH_RESULT_LIMIT)
            .map(|e| {
                let (file_name, _) = path_parts(&e.relative_path);
                SearchResultEntry {
                    like_count: like_counts.get(&e.entry_key).copied().unwrap_or(0),
                    entry_key: e.entry_key,
                    title: e.scraped_title.unwrap_or(e.title),
                    kind: kind_str(e.kind).to_string(),
                    year: e.year,
                    genres: e.genres,
                    rating: e.rating,
                    community_rating: e.community_rating,
                    community_rating_votes: e.community_rating_votes,
                    file_name,
                    relative_path: e.relative_path,
                }
            })
            .collect();
        Ok(Json(results))
    }

    #[tool(
        description = "Get full details (synopsis, cast, rating, genres, like count) for one library entry by its entry_key."
    )]
    async fn get_entry_details(
        &self,
        Parameters(params): Parameters<EntryKeyParams>,
    ) -> Result<Json<EntryDetails>, String> {
        let entry = self
            .core
            .library
            .get(&params.entry_key)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no library entry with entry_key \"{}\"", params.entry_key))?;
        let like_counts = self
            .core
            .library
            .like_counts()
            .await
            .map_err(|e| e.to_string())?;
        let (file_name, folder) = path_parts(&entry.relative_path);
        let absolute_path = self.core.media_roots.resolve(&entry.relative_path);
        let media_root_label = self.core.media_roots.label_for(&entry.relative_path);
        Ok(Json(EntryDetails {
            like_count: like_counts.get(&entry.entry_key).copied().unwrap_or(0),
            entry_key: entry.entry_key.clone(),
            title: entry
                .scraped_title
                .clone()
                .unwrap_or_else(|| entry.title.clone()),
            kind: kind_str(entry.kind).to_string(),
            year: entry.year,
            genres: entry.genres,
            rating: entry.rating,
            community_rating: entry.community_rating,
            community_rating_votes: entry.community_rating_votes,
            overview: entry.overview,
            cast: entry
                .cast
                .into_iter()
                .map(|c| match c.character {
                    Some(character) => format!("{} as {character}", c.name),
                    None => c.name,
                })
                .collect(),
            show_title: entry.show_title,
            season: entry.season,
            episode: entry.episode,
            artist: entry.artist,
            album: entry.album,
            file_name,
            folder,
            relative_path: entry.relative_path,
            absolute_path: absolute_path.to_string_lossy().into_owned(),
            media_root_label,
        }))
    }

    #[tool(
        description = "Browse the media server's real filesystem layout. Returns filenames, folders, root-relative paths, and resolved absolute paths for indexed media files. Read-only; defaults to 200 and is capped at 500 results."
    )]
    async fn browse_media_files(
        &self,
        Parameters(params): Parameters<BrowseMediaFilesParams>,
    ) -> Result<Json<Vec<MediaFileInfo>>, String> {
        let entries = self.core.library.list().await.map_err(|e| e.to_string())?;
        let path_query = params.path_prefix.as_deref().map(str::to_lowercase);
        let root_query = params.root_label.as_deref();
        let limit = params.limit.unwrap_or(200).clamp(1, 500);
        let results = entries
            .into_iter()
            .filter(|entry| {
                path_query
                    .as_deref()
                    .is_none_or(|query| entry.relative_path.to_lowercase().contains(query))
            })
            .filter(|entry| {
                root_query.is_none_or(|root| {
                    self.core
                        .media_roots
                        .label_for(&entry.relative_path)
                        .eq_ignore_ascii_case(root)
                })
            })
            .take(limit)
            .map(|entry| {
                let (file_name, folder) = path_parts(&entry.relative_path);
                MediaFileInfo {
                    entry_key: entry.entry_key,
                    kind: kind_str(entry.kind).to_string(),
                    title: entry.scraped_title.unwrap_or(entry.title),
                    media_root_label: self.core.media_roots.label_for(&entry.relative_path),
                    file_name,
                    folder,
                    absolute_path: self
                        .core
                        .media_roots
                        .resolve(&entry.relative_path)
                        .to_string_lossy()
                        .into_owned(),
                    relative_path: entry.relative_path,
                    size_bytes: entry.size,
                }
            })
            .collect();
        Ok(Json(results))
    }

    #[tool(
        description = "List every device in this server's swarm(s) and whether it's currently online."
    )]
    async fn list_swarm_devices(&self) -> Result<Json<Vec<SwarmDeviceInfo>>, String> {
        let Some(link) = self.core.stun_link().await else {
            return Ok(Json(Vec::new()));
        };
        let mut devices = Vec::new();
        for swarm in link.swarms {
            let response = self
                .core
                .swarm_devices(&swarm.id)
                .await
                .map_err(|e| e.to_string())?;
            devices.extend(response.devices.into_iter().map(|d| SwarmDeviceInfo {
                name: d.name,
                device_type: format!("{:?}", d.device_type).to_lowercase(),
                online: d.online,
                last_seen_at: d.last_seen_at,
            }));
        }
        Ok(Json(devices))
    }

    #[tool(
        description = "List recent client-reported errors (playback failures, unreachable servers, user-reported asset problems) for triage — newest first."
    )]
    async fn list_client_errors(&self) -> Result<Json<Vec<ClientErrorInfo>>, String> {
        let errors = self
            .core
            .library
            .list_client_errors()
            .await
            .map_err(|e| e.to_string())?;
        Ok(Json(
            errors
                .into_iter()
                .map(|e| ClientErrorInfo {
                    device_name: e.device_name,
                    asset_title: e.asset_title,
                    message: e.message,
                    context: e.context,
                    occurred_at_ms: e.occurred_at_ms,
                })
                .collect(),
        ))
    }
}

#[tool_handler]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        // Field-assignment, not a `ServerInfo { .., ..Default::default() }`
        // literal: `ServerInfo` (= `InitializeResult`) is `#[non_exhaustive]`
        // upstream, which blocks struct-literal construction entirely from
        // outside its own crate, even with a `..` base.
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Query this SWARM media server: search the library (search_library), \
             inspect one entry in full (get_entry_details), browse actual filenames \
             and folder structure (browse_media_files), check which swarm \
             devices are online (list_swarm_devices), and review recent \
             client-reported errors (list_client_errors). All tools are read-only."
                .into(),
        );
        info
    }
}

/// Binds `0.0.0.0:{port}` and serves the MCP endpoint at `/mcp` until the
/// process exits — spawned as a background task, never awaited by its
/// caller (see `AppState::core`). `disable_allowed_hosts` because this
/// server is meant to be reachable from other devices on the LAN (an AI
/// client rarely runs on the same machine as a headless media server); the
/// SDK's default DNS-rebinding protection only allows loopback hosts, which
/// would silently reject every real LAN request. A bearer-token middleware
/// still authenticates every request before it reaches the MCP service.
pub async fn serve(core: Arc<ServerCore>, port: u16, access_token: String) -> std::io::Result<()> {
    let addr: SocketAddr = ([0, 0, 0, 0], port).into();
    let session_manager = Arc::new(LocalSessionManager::default());
    let config = StreamableHttpServerConfig::default().disable_allowed_hosts();
    let service = StreamableHttpService::new(
        move || Ok(McpServer::new(Arc::clone(&core))),
        session_manager,
        config,
    );
    let router =
        axum::Router::new()
            .nest_service("/mcp", service)
            .layer(middleware::from_fn_with_state(
                Arc::<str>::from(access_token),
                require_access_token,
            ));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(port, "MCP server listening");
    axum::serve(listener, router).await
}

async fn require_access_token(
    State(expected): State<Arc<str>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let authorized = has_valid_bearer(request.headers(), &expected);
    if !authorized {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(request).await)
}

fn has_valid_bearer(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|provided| provided == expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_bearer_token_is_required_and_must_match() {
        let mut headers = HeaderMap::new();
        assert!(!has_valid_bearer(&headers, "secret"));
        headers.insert(header::AUTHORIZATION, "Bearer wrong".parse().unwrap());
        assert!(!has_valid_bearer(&headers, "secret"));
        headers.insert(header::AUTHORIZATION, "Bearer secret".parse().unwrap());
        assert!(has_valid_bearer(&headers, "secret"));
    }

    #[test]
    fn filesystem_path_parts_keep_real_filename_and_folder() {
        assert_eq!(
            path_parts("Shows/Dexter/Season 03/Dexter - S03E01.mkv"),
            (
                "Dexter - S03E01.mkv".to_string(),
                "Shows/Dexter/Season 03".to_string()
            )
        );
    }
}
