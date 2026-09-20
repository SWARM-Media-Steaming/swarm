/**
 * The merged multi-server catalog — the payoff of `peer_addr` self-report
 * plus [app.swarm.tv.core.catalog.CatalogSession]: every server in the
 * swarm that's currently dialable is connected to directly, and their
 * libraries appear as one browsable list grouped by kind. Movies stay a
 * flat shelf (each is its own leaf); Shows and Music are grouped
 * client-side ([app.swarm.tv.core.catalog.CatalogGrouping]) into Show and
 * Artist shelves — clicking a card in either goes straight one level
 * deeper ([SeasonScreen]/[AlbumScreen]), and the row's own header opens a
 * fuller grid ([ShowShelfScreen]/[ArtistShelfScreen]) for browsing many at
 * once.
 *
 * No title/subtitle/Back button here on purpose — every pixel is real
 * estate a 10-foot UI is short on, and the remote's own physical Back
 * button (wired via [BackHandler]) already does what an on-screen "Back"
 * button would. Like Netflix, a fixed top bar (search icon, Movies, Shows,
 * Music) picks which kind of library the page shows, and the first row of
 * the page is a strip of category tiles ([CategoryRow]) — no filter sidebar.
 * The search icon opens [SearchOverlay], which drives the same on-screen
 * keyboard flow the old inline search box did.
 */
package app.swarm.tv.app.ui.screens

import androidx.activity.compose.BackHandler
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.grid.LazyGridState
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.itemsIndexed as gridItemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusDirection
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.tv.material3.Border
import androidx.tv.material3.Button
import androidx.tv.material3.Card
import androidx.tv.material3.CardDefaults
import app.swarm.tv.R
import app.swarm.tv.app.data.BrowsePreview
import app.swarm.tv.app.data.WatchlistKeys
import app.swarm.tv.app.ui.components.SwarmLoadingIndicator
import app.swarm.tv.app.ui.components.TvOutlinedTextField
import app.swarm.tv.app.ui.components.swarmActionButtonColors
import app.swarm.tv.app.ui.PrefetchArtworkRow
import app.swarm.tv.app.ui.UatTestTags
import app.swarm.tv.app.ui.theme.SwarmAccent
import app.swarm.tv.app.ui.theme.SwarmAccentHot
import app.swarm.tv.app.ui.theme.SwarmBackground
import app.swarm.tv.app.ui.theme.SwarmLike
import app.swarm.tv.app.ui.theme.SwarmBorder
import app.swarm.tv.app.ui.theme.SwarmMuted
import app.swarm.tv.app.ui.theme.SwarmSurface
import app.swarm.tv.app.ui.theme.SwarmSurfaceMuted
import app.swarm.tv.app.ui.theme.SwarmText
import app.swarm.tv.core.catalog.ArtistGroup
import app.swarm.tv.core.catalog.CatalogGrouping
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.catalog.ShowGroup
import app.swarm.tv.core.catalog.displayTitle
import app.swarm.tv.core.peer.MediaKind
import app.swarm.tv.core.rest.SwarmDevice
import app.swarm.tv.core.watch.WatchState
import kotlinx.coroutines.launch

/** The top-bar destinations, in display order. Movies is the landing page. A search is the only thing that spans all three. */
internal enum class KindFilter(val label: String) {
    MOVIES("Movies"), SHOWS("Shows"), MUSIC("Music"),
}

internal data class CatalogBrowseState(
    val searchText: String = "",
    val appliedSearchQuery: String = "",
    val kindFilter: KindFilter = KindFilter.MOVIES,
    val genreFilter: String? = null,
)

/** Cap on visible assets per Movies/Shows/Music row (root or genre) before a
 * "Browse All" tile takes over showing the rest — a real 10-foot remote
 * can't usefully scroll an unbounded row, and every row already has a full
 * grid destination one click away. */
private const val MAX_SHELF_ITEMS = 20

/**
 * Sentinel [initialFocusMovieKey]/[initialFocusShowKey]/[initialFocusArtistKey]
 * value meaning "focus this row's Browse All tile". Set by MainActivity when
 * the user entered a [MovieShelfScreen]/[ShowShelfScreen]/[ArtistShelfScreen]
 * via that tile, so pressing Back out of the grid returns focus to the tile
 * that opened it rather than defaulting to the top of the page (#159). No real
 * entry key can collide with it — entry keys are server-derived paths.
 */
internal const val BROWSE_ALL_TILE_FOCUS_KEY = "__browse_all_tile__"

/**
 * Which index (if any) a top-level shelf row should restore D-pad focus to.
 * A remembered entry key resolves to that card's position; the
 * [BROWSE_ALL_TILE_FOCUS_KEY] sentinel resolves to the Browse All tile's
 * position (the item right after the [MAX_SHELF_ITEMS] visible cards), but
 * only in the horizontal-shelf layout that actually has a tile — the
 * genre-filtered full grid has none, so it falls back to first-card focus.
 */
internal fun <T> shelfRestoreIndex(
    items: List<T>,
    focusKey: String?,
    genreFiltered: Boolean,
    keyOf: (T) -> String,
): Int? = when {
    focusKey == null -> null
    focusKey == BROWSE_ALL_TILE_FOCUS_KEY ->
        if (genreFiltered) null else items.size.coerceAtMost(MAX_SHELF_ITEMS).takeIf { items.size > MAX_SHELF_ITEMS }
    else -> items.indexOfFirst { keyOf(it) == focusKey }.takeIf { it >= 0 }
}

/** Last-watched-wins cap on the Continue Watching row. */
private const val MAX_CONTINUE_WATCHING = 6

/** A genre sub-shelf needs at least this many distinct assets to be worth
 * its own row — fewer reads as a scraping gap, not a real category. */
private const val MIN_GENRE_SHELF_SIZE = 6

/** How many genre sub-shelves each kind (Movies/Shows/Music) shows on the
 * browse page — trimmed down from 5 (the previous cap) per live feedback
 * asking for a shorter, less repetitive scroll to reach the actual
 * Movies/Shows/Music rows. */
private const val MAX_GENRE_SHELVES = 3

private enum class QuickAccessKind { MOVIE, EPISODE, SHOW }

private data class QuickAccessItem(
    val key: String,
    val title: String,
    val subtitle: String,
    val representative: MergedEntry,
    val kind: QuickAccessKind,
    val progress: Float? = null,
    val updatedAt: Long = 0,
    val show: ShowGroup? = null,
)

@Composable
internal fun CatalogScreen(
    entries: List<MergedEntry>,
    loading: Boolean,
    unreachable: List<SwarmDevice>,
    playbackError: String?,
    artworkUrl: (MergedEntry) -> String?,
    artistPhotoUrl: (MergedEntry) -> String?,
    onOpenMovie: (MergedEntry) -> Unit,
    // Take the row's own (root or genre-filtered) list so a genre sub-shelf's
    // "Browse All" tile can reuse the same un-titled full-grid screen the
    // top-level row's tile does, just pre-filtered — see
    // [app.swarm.tv.app.data.SwarmViewModel.openMovieShelf]'s doc comment.
    onOpenMovieShelf: (List<MergedEntry>) -> Unit,
    onOpenArtistShelf: (List<ArtistGroup>) -> Unit,
    onOpenArtist: (ArtistGroup) -> Unit,
    onOpenShowShelf: (List<ShowGroup>) -> Unit,
    onOpenShow: (ShowGroup) -> Unit,
    onOpenSwarm: () -> Unit,
    onOpenBuzz: () -> Unit,
    onBack: () -> Unit,
    // Which card should get initial D-pad focus in the Movies/Shows/Music
    // *top-level* row specifically (not a genre sub-shelf — see MainActivity's
    // doc comment on where these come from) — set from whichever card was
    // last opened into a detail/season/album screen, so coming back via the
    // remote's Back button lands focus where the user actually was instead
    // of always resetting to the first card. `null` (a first-ever visit, or
    // the remembered card no longer matches anything currently shown) falls
    // back to the previous "focus the first card of the first non-empty
    // section" behavior.
    initialFocusMovieKey: String? = null,
    initialFocusShowKey: String? = null,
    initialFocusArtistKey: String? = null,
    isLiked: (MergedEntry) -> Boolean = { false },
    watchStates: Map<String, WatchState>,
    watchlistKeys: Set<String>,
    onPlay: (MergedEntry) -> Unit,
    /** Continue Watching opens straight into the pause overlay (cast,
     * synopsis, "More like this," an explicit Resume) instead of
     * autoplaying — see [app.swarm.tv.app.data.SwarmViewModel.play]'s
     * `startPaused` param. */
    onPlayPaused: (MergedEntry) -> Unit,
    preview: BrowsePreview?,
    onStartPreview: (MergedEntry) -> Unit,
    onStopPreview: () -> Unit,
    onPreviewFinished: (String) -> Unit,
    initialBrowseState: CatalogBrowseState = CatalogBrowseState(),
    onBrowseStateChange: (CatalogBrowseState) -> Unit = {},
) {
    // The two-stage warm-up/expand hover-preview flow, shared verbatim with
    // the "Browse All" grids (#159) — see [rememberBrowsePreviewCoordinator].
    val previewCoordinator = rememberBrowsePreviewCoordinator(
        preview = preview,
        onStartPreview = onStartPreview,
        onStopPreview = onStopPreview,
        onPreviewFinished = onPreviewFinished,
    )
    val expandedPreviewEntryKey = previewCoordinator.expandedPreviewEntryKey
    val previewFinished = previewCoordinator.onPreviewFinished
    val previewFocusChanged = previewCoordinator.onPreviewFocusChanged
    // searchText is what's live in the field as the user types; appliedSearchQuery
    // is what actually drives filtering below. Keeping them separate is the fix for
    // a real bug: when a single `searchQuery` backed both the field and the
    // `remember(entries, searchQuery, kindFilter)` filter, every keystroke forced a
    // full recomposition of the catalog list underneath the still-focused field,
    // which disrupted focus/IME state and made the search box appear to "exit" after
    // one letter. appliedSearchQuery now only updates from TvOutlinedTextField's
    // onSubmit (D-pad Enter/Done), so typing no longer touches the list at all.
    var searchText by remember { mutableStateOf(initialBrowseState.searchText) }
    var appliedSearchQuery by remember { mutableStateOf(initialBrowseState.appliedSearchQuery) }
    var kindFilter by remember { mutableStateOf(initialBrowseState.kindFilter) }
    // Categories = genres, same field the media server's own category picker
    // writes — null means "no genre filter", matching that server-side
    // filter's "All categories" option.
    var genreFilter by remember { mutableStateOf(initialBrowseState.genreFilter) }
    var searchOpen by remember { mutableStateOf(false) }
    var topBarHasFocus by remember { mutableStateOf(false) }
    // A genre pick (or un-pick) swaps the shelves for the full grid or back,
    // which recomposes the category row in a new tree and drops D-pad focus
    // with it. The tile that was clicked is remembered here so the row can
    // hand focus back to it, letting the user keep browsing categories.
    var categoryFocusGenre by remember { mutableStateOf<String?>(null) }
    var topBarWasCovered by remember { mutableStateOf(false) }
    var automaticInitialFocusEnabled by remember { mutableStateOf(true) }
    val currentBrowseState by rememberUpdatedState(
        CatalogBrowseState(searchText, appliedSearchQuery, kindFilter, genreFilter),
    )
    // Preview teardown on exit is handled by [rememberBrowsePreviewCoordinator].
    DisposableEffect(Unit) {
        onDispose { onBrowseStateChange(currentBrowseState) }
    }
    // A search spans Movies, Shows and Music together (like Netflix's own),
    // so while one is applied no single kind or category applies.
    val searchActive = appliedSearchQuery.isNotBlank()
    val scopeKind = kindFilter.takeUnless { searchActive }
    // Keep navigation from the top bar separate from initial/restored focus.
    // A restored requester can remain attached to a card far down the catalog;
    // once that lazy item is disposed it cannot be used to leave the top bar.
    // This requester is always attached to the first active card.
    val catalogEntryFocusRequester = remember { FocusRequester() }
    val initialCatalogFocusRequester = remember { FocusRequester() }
    val watchlistRowFocusRequester = remember { FocusRequester() }
    val topBarFocusRequester = remember { FocusRequester() }
    // Where DOWN from the top bar lands: the category tile row's entry tile.
    val categoryEntryFocusRequester = remember { FocusRequester() }
    val catalogListState = rememberLazyListState()
    // Hoisted (rather than owned by GenreFilteredGrid) so the top bar can
    // scroll the picked-category grid back to its first row before focusing
    // into it. Keyed like the tile row: each pick starts a fresh grid at the top.
    val genreGridState = remember(genreFilter) { LazyGridState() }
    val categoryListState = remember(kindFilter) { LazyListState() }
    val focusNavigationScope = rememberCoroutineScope()
    val focusManager = LocalFocusManager.current

    // Tabs are navigation, not filters: switching drops any search/genre so
    // each tab always opens on its own unfiltered page, and focus stays on
    // the tab instead of jumping into the new content.
    val selectKind = { kind: KindFilter ->
        automaticInitialFocusEnabled = false
        kindFilter = kind
        genreFilter = null
        searchText = ""
        appliedSearchQuery = ""
    }
    val clearSearch = {
        automaticInitialFocusEnabled = false
        searchText = ""
        appliedSearchQuery = ""
    }
    val toggleGenre = { genre: String ->
        automaticInitialFocusEnabled = false
        categoryFocusGenre = genre
        genreFilter = genre.takeUnless { it == genreFilter }
    }

    // Back is layered: it first closes the search overlay, then (from
    // anywhere in the content) scrolls to the top and lands on the top bar —
    // a real ask from live use, since that is a much shorter trip than
    // scrolling a long catalog back by hand — and only a further press with
    // the top bar already focused falls through to the normal exit
    // ([onBack] decides between the exit-confirm modal and returning to the
    // dashboard).
    BackHandler(enabled = searchOpen) { searchOpen = false }
    BackHandler(enabled = !searchOpen && !topBarHasFocus) {
        focusNavigationScope.launch {
            runCatching { if (genreFilter != null) genreGridState.scrollToItem(0) else catalogListState.scrollToItem(0) }
            withFrameNanos {}
            runCatching { topBarFocusRequester.requestFocus() }
        }
    }
    BackHandler(enabled = !searchOpen && topBarHasFocus, onBack = onBack)

    // Categories follow the selected tab and are ranked by how many assets
    // carry each one. They belong to a tab's own page, so a search hides them.
    val categories = remember(entries, kindFilter) {
        val scoped = entries.filter { kindMatches(it, kindFilter) }
        when (kindFilter) {
            KindFilter.MOVIES -> CatalogGrouping.movies(scoped).map { it.entry.genres }
            KindFilter.SHOWS -> CatalogGrouping.groupEpisodesByShowSeason(scoped)
                .map { show -> show.seasons.flatMap { season -> season.episodes.flatMap { it.entry.genres } } }
            KindFilter.MUSIC -> CatalogGrouping.groupTracksByArtistAlbum(scoped)
                .map { artist -> artist.albums.flatMap { album -> album.tracks.flatMap { it.entry.genres } } }
        }.let(::rankCategories)
    }
    val showCategoryRow = !searchActive && categories.isNotEmpty()
    // The category row is item 0 of the page's scrolling list when shown, and
    // every "index of the Nth section" below is offset by it.
    val headerCount = if (showCategoryRow) 1 else 0
    // A rescan can drop the only asset carrying the picked category; without
    // this the page would stay stuck on an empty filter with nothing to undo it.
    LaunchedEffect(categories, loading, entries.isEmpty()) {
        if (!loading && entries.isNotEmpty() && genreFilter != null && categories.none { it.genre == genreFilter }) {
            genreFilter = null
        }
    }
    // DOWN from the top bar. The top bar sits outside the lazy list, so the
    // default geometric focus search cannot be trusted to find (or compose) a
    // target inside it — real bug from live use: the top bar could be reached
    // but never left downward. Like every other "leave a header downward" hop
    // on this screen it is explicit: scroll the target into composition,
    // wait a frame, then request focus. It lands on the picked category's
    // tile (else the first tile), or straight on the first asset when the
    // page has no category row.
    val categoryEntryIndex = categories.indexOfFirst { it.genre == genreFilter }.coerceAtLeast(0)
    val enterContentFromTopBar: () -> Unit = {
        automaticInitialFocusEnabled = false
        focusNavigationScope.launch {
            val inGrid = genreFilter != null
            var focused = false
            if (showCategoryRow) {
                runCatching { if (inGrid) genreGridState.scrollToItem(0) else catalogListState.scrollToItem(0) }
                runCatching { categoryListState.scrollToItem(categoryEntryIndex) }
                withFrameNanos {}
                focused = runCatching { categoryEntryFocusRequester.requestFocus() }.isSuccess
            } else {
                runCatching { if (inGrid) genreGridState.scrollToItem(headerCount + 1) else catalogListState.scrollToItem(headerCount) }
                withFrameNanos {}
                focused = runCatching { catalogEntryFocusRequester.requestFocus() }.isSuccess
            }
            withFrameNanos {}
            // Nothing took the request (or it didn't move focus off the top
            // bar): fall back to the platform's own DOWN search.
            if (!focused || topBarHasFocus) focusManager.moveFocus(FocusDirection.Down)
        }
    }
    val categoryRow: @Composable ((() -> Unit)?) -> Unit = { onNavigateDown ->
        CategoryRow(
            categories = categories,
            selectedGenre = genreFilter,
            entryIndex = categoryEntryIndex,
            entryFocusRequester = categoryEntryFocusRequester,
            listState = categoryListState,
            focusGenre = categoryFocusGenre,
            onFocusHandled = { categoryFocusGenre = null },
            onSelect = toggleGenre,
            onNavigateDown = onNavigateDown,
        )
    }

    Box(modifier = Modifier.fillMaxSize()) {
        // Small padding here, not the ~40dp this screen used to carry: MainActivity's
        // contentModifier already reserves the TV-safe overscan margin around every
        // non-Player screen, so a second, separate margin here just doubled up as extra
        // dead space on every edge — confirmed live as a persistent empty border around
        // the browse page no matter how this screen's own padding was tuned.
        Column(
            modifier = Modifier.fillMaxSize()
                .padding(horizontal = 8.dp, vertical = 8.dp)
                // Trap D-pad focus while the search overlay is up — the overlay
                // is only visually modal otherwise.
                .focusProperties { canFocus = !searchOpen },
        ) {
            CatalogTopBar(
                selectedKind = scopeKind,
                searchActive = searchActive,
                appliedSearchQuery = appliedSearchQuery,
                onSelectKind = selectKind,
                onOpenSearch = { searchOpen = true },
                onClearSearch = clearSearch,
                onOpenSwarm = onOpenSwarm,
                onOpenBuzz = onOpenBuzz,
                unreachable = unreachable,
                playbackError = playbackError,
                focusRequester = topBarFocusRequester,
                onFocusChanged = { topBarHasFocus = it },
                onNavigateDown = enterContentFromTopBar.takeIf { !loading && entries.isNotEmpty() },
            )
            Column(modifier = Modifier.weight(1f).fillMaxWidth()) {
                when {
                    // Same GIF/caption treatment PlayerScreen's own "negotiated,
                    // now waiting" state uses — real feedback from live use:
                    // merging every reachable server's catalog is a real,
                    // sometimes-noticeable network wait too, and there's no
                    // reason it should feel less alive than the player's.
                    loading -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { SwarmLoadingIndicator() }
                    entries.isEmpty() -> Text("Nothing in the catalog yet.", color = SwarmMuted, fontSize = 14.sp)
                    else -> {
                        // Same multi-field match the media server's own search box
                        // uses (`media.js`'s `filteredEntries`) — matches on
                        // whichever identifying name field is present, so
                        // searching a show/artist name keeps every episode/track
                        // under it even though the entry's own title might not
                        // mention it.
                        val filtered = remember(entries, appliedSearchQuery, scopeKind, genreFilter) {
                            val q = appliedSearchQuery.trim().lowercase()
                            entries.filter { merged ->
                                val e = merged.entry
                                if (!kindMatches(merged, scopeKind)) return@filter false
                                if (genreFilter != null && !e.genres.contains(genreFilter)) return@filter false
                                if (q.isEmpty()) return@filter true
                                listOfNotNull(e.scrapedTitle, e.title, e.artist, e.album, e.showTitle).any { it.lowercase().contains(q) }
                            }
                        }
                        // Highest rated/reviewed first in every shelf this feeds
                        // (root rows, genre sub-shelves, and the genre-selected
                        // full grid alike) — real feedback from live use asking
                        // browse order to actually reflect quality, not just
                        // alphabetical/insertion order.
                        val movies = remember(filtered) {
                            CatalogGrouping.movies(filtered)
                                .sortedWith(compareByDescending<MergedEntry> { it.ratingScore() }.thenBy { it.entry.displayTitle().lowercase() })
                        }
                        val shows = remember(filtered) {
                            CatalogGrouping.groupEpisodesByShowSeason(filtered)
                                .sortedWith(compareByDescending<ShowGroup> { it.ratingScore() }.thenBy { it.show.lowercase() })
                        }
                        val artists = remember(filtered) {
                            CatalogGrouping.groupTracksByArtistAlbum(filtered)
                                .sortedWith(compareByDescending<ArtistGroup> { it.ratingScore() }.thenBy { it.artist.lowercase() })
                        }

                        // Quick-access rows intentionally use the full currently-visible
                        // catalog of the selected tab, not a genre/search subset, and
                        // disappear while a user is actively filtering. That keeps them
                        // predictable home rows rather than making saved items appear
                        // to vanish mid-search.
                        val showQuickAccess = !searchActive && genreFilter == null
                        val allShows = remember(entries) { CatalogGrouping.groupEpisodesByShowSeason(entries) }
                        val showByEpisode = remember(allShows) {
                            buildMap {
                                for (show in allShows) {
                                    for (season in show.seasons) {
                                        for (episode in season.episodes) put(episode.entry.fingerprint, show)
                                    }
                                }
                            }
                        }
                        val continueWatching = remember(entries, watchStates, showQuickAccess, showByEpisode, kindFilter) {
                            if (!showQuickAccess) {
                                emptyList()
                            } else {
                                val inProgress = entries.mapNotNull { entry ->
                                    if (entry.entry.kind == MediaKind.TRACK) return@mapNotNull null
                                    val saved = watchStates[entry.entry.fingerprint] ?: return@mapNotNull null
                                    if (saved.watched || saved.positionSecs <= 0.0) return@mapNotNull null
                                    entry to saved
                                }
                                val movieItems = inProgress
                                    .filter { it.first.entry.kind == MediaKind.MOVIE && it.first.entry.extraType == null }
                                    .map { (entry, saved) ->
                                        QuickAccessItem(
                                            key = "continue-movie-${entry.entry.fingerprint}",
                                            title = entry.entry.scrapedTitle ?: entry.entry.title,
                                            subtitle = "Movie • ${saved.percentComplete()}% watched",
                                            representative = entry,
                                            kind = QuickAccessKind.MOVIE,
                                            progress = saved.progressFraction(),
                                            updatedAt = saved.updatedAt,
                                        )
                                    }
                                val episodeItems = inProgress
                                    .filter { it.first.entry.kind == MediaKind.EPISODE }
                                    .groupBy { (entry, _) -> showByEpisode[entry.entry.fingerprint]?.show ?: entry.entry.showTitle.orEmpty() }
                                    .values
                                    .mapNotNull { candidates -> candidates.maxByOrNull { it.second.updatedAt } }
                                    .map { (entry, saved) ->
                                        val show = showByEpisode[entry.entry.fingerprint]
                                        val episodeLabel = listOfNotNull(
                                            entry.entry.season?.let { "S$it" },
                                            entry.entry.episode?.let { "E$it" },
                                        ).joinToString(" ")
                                        QuickAccessItem(
                                            key = "continue-episode-${entry.entry.fingerprint}",
                                            title = show?.show ?: entry.entry.showTitle ?: entry.entry.title,
                                            subtitle = listOf(episodeLabel, "${saved.percentComplete()}% watched").filter { it.isNotBlank() }.joinToString(" • "),
                                            representative = entry,
                                            kind = QuickAccessKind.EPISODE,
                                            progress = saved.progressFraction(),
                                            updatedAt = saved.updatedAt,
                                            show = show,
                                        )
                                    }
                                // Last-watched-wins: only the 6 most recently
                                // touched titles stay on the home row, so an old
                                // in-progress title doesn't linger indefinitely.
                                (movieItems + episodeItems)
                                    .filter { quickAccessMatches(it.kind, kindFilter) }
                                    .sortedByDescending { it.updatedAt }
                                    .take(MAX_CONTINUE_WATCHING)
                            }
                        }
                        val watchlist = remember(entries, allShows, watchStates, watchlistKeys, showQuickAccess, kindFilter) {
                            if (!showQuickAccess) {
                                emptyList()
                            } else {
                                val movieItems = entries
                                    .filter { entry ->
                                        entry.entry.kind == MediaKind.MOVIE &&
                                            entry.entry.extraType == null &&
                                            WatchlistKeys.movie(entry) in watchlistKeys &&
                                            watchStates[entry.entry.fingerprint]?.watched != true
                                    }
                                    .map { entry ->
                                        QuickAccessItem(
                                            key = WatchlistKeys.movie(entry),
                                            title = entry.entry.scrapedTitle ?: entry.entry.title,
                                            subtitle = "Movie",
                                            representative = entry,
                                            kind = QuickAccessKind.MOVIE,
                                        )
                                    }
                                val showItems = allShows
                                    .filter { show -> WatchlistKeys.show(show) in watchlistKeys && !show.isWatched(watchStates) }
                                    .mapNotNull { show ->
                                        val representative = CatalogGrouping.previewSeasons(show).firstOrNull()?.episodes?.firstOrNull()
                                            ?: show.seasons.firstOrNull()?.episodes?.firstOrNull()
                                            ?: return@mapNotNull null
                                        val seasons = CatalogGrouping.previewSeasons(show).size
                                        QuickAccessItem(
                                            key = WatchlistKeys.show(show),
                                            title = show.show,
                                            subtitle = "$seasons season" + if (seasons == 1) "" else "s",
                                            representative = representative,
                                            kind = QuickAccessKind.SHOW,
                                            show = show,
                                        )
                                    }
                                (movieItems + showItems).filter { quickAccessMatches(it.kind, kindFilter) }.sortedBy { it.title.lowercase() }
                            }
                        }

                        // Netflix-style "Top picks in <genre>" sub-shelves, one per
                        // kind — only while browsing unfiltered-by-genre (once a
                        // genre is actually picked via the Genre button, `filtered`
                        // above already reduces every existing shelf to just that
                        // genre, so a further breakdown would be redundant).
                        val movieGenreShelves = remember(movies, genreFilter) {
                            if (genreFilter != null) emptyList() else topGenreShelves(movies) { it }
                        }
                        val showGenreShelves = remember(filtered, genreFilter) {
                            if (genreFilter != null) emptyList() else {
                                topGenreShelves(filtered.filter { it.entry.kind == MediaKind.EPISODE }) { CatalogGrouping.groupEpisodesByShowSeason(it) }
                            }
                        }
                        val musicGenreShelves = remember(filtered, genreFilter) {
                            if (genreFilter != null) emptyList() else {
                                topGenreShelves(filtered.filter { it.entry.kind == MediaKind.TRACK }) { CatalogGrouping.groupTracksByArtistAlbum(it) }
                            }
                        }

                        if (movies.isEmpty() && shows.isEmpty() && artists.isEmpty()) {
                            Column {
                                // Keep the category row so a picked category can still be undone.
                                if (showCategoryRow) {
                                    categoryRow(null)
                                    Spacer(Modifier.height(20.dp))
                                }
                                if (searchActive || genreFilter != null) {
                                    Text(
                                        "No matches for the current search/filter.",
                                        color = SwarmMuted,
                                        fontSize = 14.sp,
                                        modifier = Modifier.testTag(UatTestTags.SEARCH_NO_MATCHES),
                                    )
                                } else {
                                    Text("No ${kindFilter.label.lowercase()} in the catalog yet.", color = SwarmMuted, fontSize = 14.sp)
                                }
                            }
                        } else {
                            // Which top-level row gets *default* (first-card)
                            // focus when nothing is being restored — unchanged
                            // from before, just no longer paired with a single
                            // externally-owned FocusRequester (each row now
                            // decides its own target index, see MovieRow/
                            // ShowShelfRow/ArtistShelfRow's restoreFocusIndex).
                            val firstSection = when {
                                continueWatching.isNotEmpty() -> "continue"
                                watchlist.isNotEmpty() -> "watchlist"
                                movies.isNotEmpty() -> "movies"
                                shows.isNotEmpty() -> "shows"
                                artists.isNotEmpty() -> "music"
                                else -> null
                            }
                            // -1 (not found) becomes null: "nothing to restore in
                            // this particular row" is exactly the same case as
                            // "nothing was ever remembered" from the row's own
                            // point of view. A [BROWSE_ALL_TILE_FOCUS_KEY] sentinel
                            // instead restores focus to that row's Browse All tile
                            // (#159) — see [shelfRestoreIndex].
                            val movieRestoreIndex = remember(movies, initialFocusMovieKey, genreFilter) {
                                shelfRestoreIndex(movies, initialFocusMovieKey, genreFilter != null) { it.entry.entryKey }
                            }
                            val showRestoreIndex = remember(shows, initialFocusShowKey, genreFilter) {
                                shelfRestoreIndex(shows, initialFocusShowKey, genreFilter != null) { it.show }
                            }
                            val artistRestoreIndex = remember(artists, initialFocusArtistKey, genreFilter) {
                                shelfRestoreIndex(artists, initialFocusArtistKey, genreFilter != null) { it.artist }
                            }

                            // The horizontal row containing a restored card may
                            // be well below the viewport and therefore not yet
                            // composed. Scroll the parent list to that row first;
                            // the row's own focus-restoration effect then scrolls
                            // horizontally and focuses the exact selected title.
                            var nextSectionIndex = headerCount
                            if (continueWatching.isNotEmpty()) nextSectionIndex++
                            val watchlistSectionIndex = nextSectionIndex.takeIf { watchlist.isNotEmpty() }
                            if (watchlist.isNotEmpty()) nextSectionIndex++
                            val movieSectionIndex = nextSectionIndex.takeIf { movies.isNotEmpty() }
                            if (movies.isNotEmpty()) nextSectionIndex++
                            nextSectionIndex += movieGenreShelves.size
                            val showSectionIndex = nextSectionIndex.takeIf { shows.isNotEmpty() }
                            if (shows.isNotEmpty()) nextSectionIndex++
                            nextSectionIndex += showGenreShelves.size
                            val artistSectionIndex = nextSectionIndex.takeIf { artists.isNotEmpty() }
                            val restoreSectionIndex = when {
                                movieRestoreIndex != null -> movieSectionIndex
                                showRestoreIndex != null -> showSectionIndex
                                artistRestoreIndex != null -> artistSectionIndex
                                else -> null
                            }
                            LaunchedEffect(restoreSectionIndex) {
                                if (restoreSectionIndex != null) {
                                    catalogListState.scrollToItem(restoreSectionIndex)
                                }
                                // Otherwise a remembered title disappeared after a
                                // rescan/filter change: fall back to the first
                                // visible content card.
                            }
                            val restoringSelection = restoreSectionIndex != null

                            // A genre is selected: swap the Netflix-style
                            // horizontal shelves for the same full-grid "Browse
                            // all" layout MovieShelfScreen/ShowShelfScreen/
                            // ArtistShelfScreen already use, one section per
                            // kind — real feedback from live use. A single
                            // horizontal row is a fine width for "here's a taste
                            // of Action movies" browsing, but a bad one for
                            // "show me everything tagged Action", which is
                            // exactly what picking a genre is asking for.
                            if (genreFilter != null) {
                                GenreFilteredGrid(
                                    movies,
                                    shows,
                                    artists,
                                    artworkUrl,
                                    artistPhotoUrl,
                                    onOpenMovie,
                                    onOpenShow,
                                    onOpenArtist,
                                    isLiked,
                                    firstFocusRequester = initialCatalogFocusRequester,
                                    firstEntryFocusRequester = catalogEntryFocusRequester,
                                    requestInitialFocus = automaticInitialFocusEnabled,
                                    initialFocusMovieKey = initialFocusMovieKey,
                                    initialFocusShowKey = initialFocusShowKey,
                                    initialFocusArtistKey = initialFocusArtistKey,
                                    hasHeader = showCategoryRow,
                                    header = categoryRow,
                                    gridState = genreGridState,
                                )
                            } else {
                                val navigateToFirstCatalogEntry = {
                                    automaticInitialFocusEnabled = false
                                    focusNavigationScope.launch {
                                        // Item 0 is the category row. Scrolling the
                                        // first content row into composition before
                                        // requesting focus keeps DOWN working when
                                        // that row was disposed off-screen.
                                        runCatching { catalogListState.scrollToItem(headerCount) }
                                        withFrameNanos {}
                                        runCatching { catalogEntryFocusRequester.requestFocus() }
                                    }
                                    Unit
                                }
                                LazyColumn(
                                    state = catalogListState,
                                    modifier = Modifier.fillMaxSize(),
                                    verticalArrangement = Arrangement.spacedBy(28.dp),
                                ) {
                                    if (showCategoryRow) {
                                        item(key = "catalog-categories", contentType = "categories") {
                                            categoryRow(navigateToFirstCatalogEntry)
                                        }
                                    }
                                    if (continueWatching.isNotEmpty()) {
                                        item(key = "continue-watching", contentType = "quick-access") {
                                            QuickAccessRow(
                                                title = "Continue Watching",
                                                items = continueWatching,
                                                artworkUrl = artworkUrl,
                                                onClick = { item -> onPlayPaused(item.representative) },
                                                isLiked = isLiked,
                                                isDefaultFocusRow = firstSection == "continue",
                                                defaultFocusRequester = initialCatalogFocusRequester.takeIf { !restoringSelection && firstSection == "continue" },
                                                firstCardFocusRequester = catalogEntryFocusRequester.takeIf { firstSection == "continue" },
                                                onNavigateDown = watchlistSectionIndex?.let { sectionIndex ->
                                                    {
                                                        automaticInitialFocusEnabled = false
                                                        focusNavigationScope.launch {
                                                            catalogListState.scrollToItem(sectionIndex)
                                                            withFrameNanos {}
                                                            runCatching { watchlistRowFocusRequester.requestFocus() }
                                                        }
                                                    }
                                                },
                                                requestInitialFocus = automaticInitialFocusEnabled,
                                            )
                                        }
                                    }
                                    if (watchlist.isNotEmpty()) {
                                        item(key = "watchlist", contentType = "quick-access") {
                                            QuickAccessRow(
                                                title = "Watchlist",
                                                items = watchlist,
                                                artworkUrl = artworkUrl,
                                                onClick = { item ->
                                                    if (item.kind == QuickAccessKind.SHOW) item.show?.let(onOpenShow)
                                                    else onOpenMovie(item.representative)
                                                },
                                                isLiked = isLiked,
                                                isDefaultFocusRow = firstSection == "watchlist",
                                                defaultFocusRequester = initialCatalogFocusRequester.takeIf { !restoringSelection && firstSection == "watchlist" },
                                                firstCardFocusRequester = catalogEntryFocusRequester.takeIf { firstSection == "watchlist" }
                                                    ?: watchlistRowFocusRequester,
                                                requestInitialFocus = automaticInitialFocusEnabled,
                                            )
                                        }
                                    }
                                    if (movies.isNotEmpty()) {
                                        item {
                                            MovieRow(
                                                "Movies", movies, artworkUrl, onOpenMovie, onOpenMovieShelf, isTopLevel = true, movieRestoreIndex,
                                                isDefaultFocusRow = firstSection == "movies",
                                                isLiked = isLiked,
                                                defaultFocusRequester = initialCatalogFocusRequester.takeIf {
                                                    movieRestoreIndex != null || (!restoringSelection && firstSection == "movies")
                                                },
                                                firstCardFocusRequester = catalogEntryFocusRequester.takeIf { firstSection == "movies" },
                                                requestInitialFocus = automaticInitialFocusEnabled,
                                                preview = preview,
                                                expandedPreviewEntryKey = expandedPreviewEntryKey,
                                                onPreviewFocusChanged = previewFocusChanged,
                                                onPreviewFinished = previewFinished,
                                            )
                                        }
                                    }
                                    items(movieGenreShelves, key = { "movie-genre-${it.first}" }) { (genre, genreMovies) ->
                                        MovieRow(
                                            genre,
                                            genreMovies,
                                            artworkUrl,
                                            onOpenMovie,
                                            onOpenShelf = onOpenMovieShelf,
                                            isTopLevel = false,
                                            restoreFocusIndex = null,
                                            isDefaultFocusRow = false,
                                            isLiked = isLiked,
                                            preview = preview,
                                            expandedPreviewEntryKey = expandedPreviewEntryKey,
                                            onPreviewFocusChanged = previewFocusChanged,
                                            onPreviewFinished = previewFinished,
                                        )
                                    }
                                    if (shows.isNotEmpty()) {
                                        item {
                                            ShowShelfRow(
                                                "Shows", shows, artworkUrl, onOpenShowShelf, onOpenShow, isTopLevel = true, showRestoreIndex,
                                                isDefaultFocusRow = firstSection == "shows",
                                                defaultFocusRequester = initialCatalogFocusRequester.takeIf {
                                                    showRestoreIndex != null || (!restoringSelection && firstSection == "shows")
                                                },
                                                firstCardFocusRequester = catalogEntryFocusRequester.takeIf { firstSection == "shows" },
                                                requestInitialFocus = automaticInitialFocusEnabled,
                                                preview = preview,
                                                expandedPreviewEntryKey = expandedPreviewEntryKey,
                                                onPreviewFocusChanged = previewFocusChanged,
                                                onPreviewFinished = previewFinished,
                                            )
                                        }
                                    }
                                    items(showGenreShelves, key = { "show-genre-${it.first}" }) { (genre, genreShows) ->
                                        ShowShelfRow(
                                            genre,
                                            genreShows,
                                            artworkUrl,
                                            onOpenShowShelf = onOpenShowShelf,
                                            onOpenShow = onOpenShow,
                                            isTopLevel = false,
                                            restoreFocusIndex = null,
                                            isDefaultFocusRow = false,
                                            preview = preview,
                                            expandedPreviewEntryKey = expandedPreviewEntryKey,
                                            onPreviewFocusChanged = previewFocusChanged,
                                            onPreviewFinished = previewFinished,
                                        )
                                    }
                                    if (artists.isNotEmpty()) {
                                        item {
                                            ArtistShelfRow(
                                                "Music", artists, artworkUrl, artistPhotoUrl, onOpenArtistShelf, onOpenArtist, isTopLevel = true, artistRestoreIndex,
                                                isDefaultFocusRow = firstSection == "music",
                                                defaultFocusRequester = initialCatalogFocusRequester.takeIf {
                                                    artistRestoreIndex != null || (!restoringSelection && firstSection == "music")
                                                },
                                                firstCardFocusRequester = catalogEntryFocusRequester.takeIf { firstSection == "music" },
                                                requestInitialFocus = automaticInitialFocusEnabled,
                                                preview = preview,
                                                expandedPreviewEntryKey = expandedPreviewEntryKey,
                                                onPreviewFocusChanged = previewFocusChanged,
                                                onPreviewFinished = previewFinished,
                                            )
                                        }
                                    }
                                    items(musicGenreShelves, key = { "music-genre-${it.first}" }) { (genre, genreArtists) ->
                                        ArtistShelfRow(
                                            genre,
                                            genreArtists,
                                            artworkUrl,
                                            artistPhotoUrl,
                                            onOpenArtistShelf = onOpenArtistShelf,
                                            onOpenArtist = onOpenArtist,
                                            isTopLevel = false,
                                            restoreFocusIndex = null,
                                            isDefaultFocusRow = false,
                                            preview = preview,
                                            expandedPreviewEntryKey = expandedPreviewEntryKey,
                                            onPreviewFocusChanged = previewFocusChanged,
                                            onPreviewFinished = previewFinished,
                                        )
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        if (searchOpen) {
            SearchOverlay(
                text = searchText,
                onTextChange = { searchText = it },
                onSubmit = {
                    val query = searchText.trim()
                    automaticInitialFocusEnabled = false
                    searchText = query
                    appliedSearchQuery = query
                    genreFilter = null
                    searchOpen = false
                },
                onDismiss = { searchOpen = false },
            )
        }
    }
    // Hand focus back to the top bar once the overlay is gone — the Column
    // behind it is un-focusable while it is up, so focus was dropped.
    LaunchedEffect(searchOpen) {
        if (!searchOpen && topBarWasCovered) {
            withFrameNanos {}
            runCatching { topBarFocusRequester.requestFocus() }
        }
        topBarWasCovered = searchOpen
    }
}

/** Continue Watching / Watchlist follow the selected tab: Movies shows movies, Shows shows episodes and series, Music has neither. */
private fun quickAccessMatches(kind: QuickAccessKind, filter: KindFilter): Boolean = when (filter) {
    KindFilter.MOVIES -> kind == QuickAccessKind.MOVIE
    KindFilter.SHOWS -> kind == QuickAccessKind.EPISODE || kind == QuickAccessKind.SHOW
    KindFilter.MUSIC -> false
}

/** A search (`kind == null`) spans every kind; otherwise the tab's own kind. */
private fun kindMatches(entry: MergedEntry, filter: KindFilter?): Boolean = when (filter) {
    null -> true
    KindFilter.MOVIES -> entry.entry.kind == MediaKind.MOVIE && entry.entry.extraType == null
    KindFilter.SHOWS -> entry.entry.kind == MediaKind.EPISODE
    KindFilter.MUSIC -> entry.entry.kind == MediaKind.TRACK
}

/**
 * The fixed Netflix-style top bar: search icon, then Movies / Shows / Music
 * (in that order), with the Buzz and SWARM settings buttons pinned right.
 * The three destinations are deliberately *not* [swarmActionButtonColors]
 * buttons — see [TopNavButton] for why they read as translucent boxes.
 * [selectedKind] is null while a search is applied, since a search spans all
 * three kinds.
 */
@Composable
private fun CatalogTopBar(
    selectedKind: KindFilter?,
    searchActive: Boolean,
    appliedSearchQuery: String,
    onSelectKind: (KindFilter) -> Unit,
    onOpenSearch: () -> Unit,
    onClearSearch: () -> Unit,
    onOpenSwarm: () -> Unit,
    onOpenBuzz: () -> Unit,
    unreachable: List<SwarmDevice>,
    playbackError: String?,
    focusRequester: FocusRequester,
    onFocusChanged: (Boolean) -> Unit,
    onNavigateDown: (() -> Unit)?,
) {
    Column(
        modifier = Modifier.fillMaxWidth()
            .onFocusChanged { onFocusChanged(it.hasFocus) }
            // DOWN from any control on the bar drops into the page below.
            .onPreviewKeyEvent { event ->
                if (onNavigateDown != null && event.type == KeyEventType.KeyDown && event.key == Key.DirectionDown) {
                    onNavigateDown()
                    true
                } else {
                    false
                }
            }
            .testTag(UatTestTags.FILTER_RAIL),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            // "Home" of the bar: the selected tab, or the search icon while a
            // search is showing results. Back lands here.
            TopNavButton(
                onClick = onOpenSearch,
                selected = searchActive,
                focusRequester = focusRequester.takeIf { selectedKind == null },
                testTag = UatTestTags.SEARCH_BUTTON,
            ) {
                Icon(
                    painter = painterResource(R.drawable.ic_search),
                    contentDescription = "Search",
                    tint = if (searchActive) SwarmText else SwarmMuted,
                    modifier = Modifier.size(22.dp),
                )
            }
            for (kind in KindFilter.entries) {
                val isSelected = kind == selectedKind
                TopNavButton(
                    onClick = { onSelectKind(kind) },
                    selected = isSelected,
                    focusRequester = focusRequester.takeIf { isSelected },
                    testTag = UatTestTags.FILTER_KIND_PREFIX + kind.name,
                ) {
                    Text(
                        kind.label,
                        color = if (isSelected) SwarmText else SwarmMuted,
                        fontSize = 16.sp,
                        fontWeight = if (isSelected) FontWeight.Black else FontWeight.SemiBold,
                        maxLines = 1,
                    )
                }
            }
            if (searchActive) {
                Button(
                    onClick = onClearSearch,
                    colors = swarmActionButtonColors(),
                    modifier = Modifier.widthIn(max = 260.dp).testTag(UatTestTags.SEARCH_CLEAR_BUTTON),
                ) {
                    Text("Clear \u201c$appliedSearchQuery\u201d", fontSize = 13.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
            }
            Spacer(Modifier.weight(1f))
            Button(onClick = onOpenBuzz, colors = swarmActionButtonColors()) {
                Text("Ask Buzz", fontSize = 13.sp)
            }
            Button(
                onClick = onOpenSwarm,
                colors = swarmActionButtonColors(),
                contentPadding = PaddingValues(10.dp),
                modifier = Modifier.size(44.dp).testTag(UatTestTags.OPEN_SWARM_BUTTON),
            ) {
                Icon(
                    painter = painterResource(R.drawable.ic_settings),
                    contentDescription = "Open SWARM",
                    modifier = Modifier.size(22.dp),
                )
            }
        }
        if (unreachable.isNotEmpty()) {
            Spacer(Modifier.height(10.dp))
            Text(
                "${unreachable.size} server(s) not reachable yet: ${unreachable.joinToString { it.name }}",
                color = SwarmMuted,
                fontSize = 12.sp,
            )
        }
        if (playbackError != null) {
            Spacer(Modifier.height(10.dp))
            Text(playbackError, color = SwarmAccent, fontSize = 12.sp)
        }
        Spacer(Modifier.height(16.dp))
    }
}

private val TOP_NAV_SHAPE = RoundedCornerShape(4.dp)

/**
 * A top-bar destination: a squared-off (4dp corners, not the pill the action
 * buttons use), see-through box. At rest it is transparent with a hairline
 * border; the selected destination gets a faint light wash and full-strength
 * text; focus adds a stronger wash and an accent border. Every interaction
 * state is set explicitly rather than left to tv-material3's defaults.
 */
@Composable
private fun TopNavButton(
    onClick: () -> Unit,
    selected: Boolean,
    focusRequester: FocusRequester?,
    testTag: String,
    content: @Composable () -> Unit,
) {
    Card(
        onClick = onClick,
        colors = CardDefaults.colors(
            containerColor = if (selected) SwarmText.copy(alpha = 0.12f) else Color.Transparent,
            contentColor = SwarmText,
            focusedContainerColor = SwarmText.copy(alpha = 0.2f),
            focusedContentColor = SwarmText,
            pressedContainerColor = SwarmText.copy(alpha = 0.3f),
            pressedContentColor = SwarmText,
        ),
        scale = CardDefaults.scale(scale = 1f, focusedScale = 1f, pressedScale = 0.98f),
        border = CardDefaults.border(
            border = Border(BorderStroke(1.dp, if (selected) SwarmText.copy(alpha = 0.55f) else SwarmBorder.copy(alpha = 0.6f)), shape = TOP_NAV_SHAPE),
            focusedBorder = Border(BorderStroke(2.dp, SwarmAccent), shape = TOP_NAV_SHAPE),
            pressedBorder = Border(BorderStroke(2.dp, SwarmAccentHot), shape = TOP_NAV_SHAPE),
        ),
        shape = CardDefaults.shape(TOP_NAV_SHAPE, TOP_NAV_SHAPE, TOP_NAV_SHAPE),
        modifier = Modifier
            .then(if (focusRequester != null) Modifier.focusRequester(focusRequester) else Modifier)
            .testTag(testTag),
    ) {
        Box(
            modifier = Modifier.defaultMinSize(minHeight = 44.dp).padding(horizontal = 18.dp, vertical = 8.dp),
            contentAlignment = Alignment.Center,
        ) { content() }
    }
}

private val CATEGORY_TILE_HEIGHT = 56.dp
private val CATEGORY_TILE_SHAPE = RoundedCornerShape(10.dp)

/**
 * The first row of a Movies/Shows/Music page: one tile per category, most
 * assets first ([rankCategories]). Tiles are the same width as an asset card
 * but a third of the height, and deliberately look nothing like one — no
 * artwork, an outlined accent frame on a transparent fill, centered text —
 * so the row reads as "pick a category" rather than "more titles". Picking
 * a tile filters the page to it; picking it again clears it.
 */
@Composable
private fun CategoryRow(
    categories: List<CategoryCount>,
    selectedGenre: String?,
    entryIndex: Int,
    entryFocusRequester: FocusRequester,
    listState: LazyListState,
    focusGenre: String?,
    onFocusHandled: () -> Unit,
    onSelect: (String) -> Unit,
    onNavigateDown: (() -> Unit)?,
) {
    val tileFocusRequester = remember { FocusRequester() }
    val focusIndex = if (focusGenre == null) -1 else categories.indexOfFirst { it.genre == focusGenre }
    LaunchedEffect(focusGenre, categories) {
        if (focusGenre != null) {
            if (focusIndex >= 0) {
                listState.scrollToItem(focusIndex)
                withFrameNanos {}
                runCatching { tileFocusRequester.requestFocus() }
            }
            onFocusHandled()
        }
    }
    Column(
        modifier = Modifier.fillMaxWidth()
            .testTag(UatTestTags.CATEGORY_ROW)
            .onPreviewKeyEvent { event ->
                if (onNavigateDown != null && event.type == KeyEventType.KeyDown && event.key == Key.DirectionDown) {
                    onNavigateDown()
                    true
                } else {
                    false
                }
            },
    ) {
        ShelfHeader("Categories", GENRE_TITLE_SIZE)
        Spacer(Modifier.height(GENRE_TITLE_SPACING))
        LazyRow(
            state = listState,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            contentPadding = PaddingValues(horizontal = 12.dp),
        ) {
            itemsIndexed(
                items = categories,
                key = { _, category -> category.genre },
                contentType = { _, _ -> "category" },
            ) { index, category ->
                CategoryTile(
                    label = category.genre,
                    selected = category.genre == selectedGenre,
                    onClick = { onSelect(category.genre) },
                    focusRequester = tileFocusRequester.takeIf { index == focusIndex },
                    entryFocusRequester = entryFocusRequester.takeIf { index == entryIndex },
                )
            }
        }
    }
}

@Composable
private fun CategoryTile(
    label: String,
    selected: Boolean,
    onClick: () -> Unit,
    focusRequester: FocusRequester?,
    entryFocusRequester: FocusRequester?,
) {
    Card(
        onClick = onClick,
        colors = CardDefaults.colors(
            containerColor = if (selected) SwarmAccent else Color.Transparent,
            contentColor = if (selected) ON_ACCENT else SwarmText,
            focusedContainerColor = if (selected) SwarmAccent else SwarmSurfaceMuted,
            focusedContentColor = if (selected) ON_ACCENT else SwarmText,
            pressedContainerColor = SwarmAccentHot,
            pressedContentColor = ON_ACCENT,
        ),
        scale = CardDefaults.scale(scale = 1f, focusedScale = 1f, pressedScale = 0.99f),
        border = CardDefaults.border(
            border = Border(BorderStroke(1.5.dp, SwarmAccent.copy(alpha = if (selected) 1f else 0.55f)), shape = CATEGORY_TILE_SHAPE),
            focusedBorder = Border(BorderStroke(3.dp, SwarmText), shape = CATEGORY_TILE_SHAPE),
            pressedBorder = Border(BorderStroke(3.dp, SwarmAccentHot), shape = CATEGORY_TILE_SHAPE),
        ),
        shape = CardDefaults.shape(CATEGORY_TILE_SHAPE, CATEGORY_TILE_SHAPE, CATEGORY_TILE_SHAPE),
        modifier = Modifier.width(CARD_WIDTH).height(CATEGORY_TILE_HEIGHT)
            .then(if (focusRequester != null) Modifier.focusRequester(focusRequester) else Modifier)
            .then(if (entryFocusRequester != null) Modifier.focusRequester(entryFocusRequester) else Modifier)
            .testTag(UatTestTags.FILTER_GENRE_PREFIX + label),
    ) {
        // Two lines, ellipsized, with inner padding: a long or multi-word
        // category name wraps or truncates inside the tile instead of running
        // over its edge.
        Box(Modifier.fillMaxSize().padding(horizontal = 10.dp, vertical = 4.dp), contentAlignment = Alignment.Center) {
            Text(
                label,
                color = if (selected) ON_ACCENT else SwarmText,
                fontSize = 14.sp,
                fontWeight = FontWeight.Bold,
                textAlign = TextAlign.Center,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}

/** Near-black navy text on [SwarmAccent] — this app's standing on-accent color. */
private val ON_ACCENT = Color(0xFF04263A)

/**
 * The search popup. The top bar's search icon opens this instead of an inline
 * text box; it hosts the same [TvOutlinedTextField] (so selecting the field
 * brings up the same on-screen keyboard, and Done applies the search).
 * Physical Back closes it — no on-screen Cancel.
 */
@Composable
private fun SearchOverlay(
    text: String,
    onTextChange: (String) -> Unit,
    onSubmit: () -> Unit,
    onDismiss: () -> Unit,
) {
    val fieldFocusRequester = remember { FocusRequester() }
    BackHandler(onBack = onDismiss)
    LaunchedEffect(Unit) { runCatching { fieldFocusRequester.requestFocus() } }
    Box(
        modifier = Modifier.fillMaxSize().background(SwarmBackground.copy(alpha = 0.94f)),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            modifier = Modifier.width(640.dp)
                .clip(RoundedCornerShape(16.dp))
                .background(SwarmSurface)
                .padding(28.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Text("Search", color = SwarmText, fontSize = 22.sp, fontWeight = FontWeight.Black)
            TvOutlinedTextField(
                value = text,
                onValueChange = onTextChange,
                placeholder = { Text("Search title, artist, show…", color = SwarmMuted) },
                colors = searchFieldColors(),
                onSubmit = onSubmit,
                modifier = Modifier.fillMaxWidth()
                    .focusRequester(fieldFocusRequester)
                    .testTag(UatTestTags.SEARCH_FIELD),
            )
            Text(
                "Select the box to type, then press Done. Results cover Movies, Shows and Music.",
                color = SwarmMuted,
                fontSize = 12.sp,
            )
        }
    }
}

/** Ranks [entries]' genres by how many entries in this specific kind carry each one (descending), keeps only genres whose *grouped* asset count reaches [MIN_GENRE_SHELF_SIZE] (a scraping gap, not a real category, otherwise), takes the top [MAX_GENRE_SHELVES] of those (or fewer, if fewer qualify), and groups each genre's matching subset via [group] — [ShowGroup]/[ArtistGroup] for Shows/Music, the identity function for the already-flat Movies list. */
private fun <T> topGenreShelves(entries: List<MergedEntry>, group: (List<MergedEntry>) -> List<T>): List<Pair<String, List<T>>> =
    entries.flatMap { it.entry.genres }
        .groupingBy { it }
        .eachCount()
        .entries
        .sortedByDescending { it.value }
        .map { (genre, _) -> genre to group(entries.filter { it.entry.genres.contains(genre) }) }
        .filter { (_, grouped) -> grouped.size >= MIN_GENRE_SHELF_SIZE }
        .take(MAX_GENRE_SHELVES)

/** Highest rated/reviewed first within each asset category — falls back to
 * `-1.0` for anything IntroDB/TMDb never scored, so unrated titles sink to
 * the end rather than interleaving with rated ones by coincidence of list
 * order. */
private fun MergedEntry.ratingScore(): Double = entry.communityRating ?: -1.0

private fun ShowGroup.ratingScore(): Double {
    val ratings = seasons.flatMap { it.episodes }.mapNotNull { it.entry.communityRating }
    return if (ratings.isEmpty()) -1.0 else ratings.average()
}

private fun ArtistGroup.ratingScore(): Double {
    val ratings = albums.flatMap { it.tracks }.mapNotNull { it.entry.communityRating }
    return if (ratings.isEmpty()) -1.0 else ratings.average()
}

private fun WatchState.progressFraction(): Float =
    if (durationSecs <= 0.0) 0f else (positionSecs / durationSecs).coerceIn(0.0, 1.0).toFloat()

private fun WatchState.percentComplete(): Int = (progressFraction() * 100).toInt()

private fun ShowGroup.isWatched(states: Map<String, WatchState>): Boolean {
    val episodes = CatalogGrouping.previewSeasons(this).flatMap { it.episodes }
    return episodes.isNotEmpty() && episodes.all { states[it.entry.fingerprint]?.watched == true }
}

/**
 * The genre-filtered view — the "Browse all" full-grid layout
 * ([MovieShelfScreen]/[ShowShelfScreen]/[ArtistShelfScreen]'s own visual
 * style), one section per kind, instead of the horizontal-shelf browsing
 * [CatalogScreen] otherwise uses. A single scrolling row is fine for "here's
 * a taste of Action movies"; picking a genre is asking to actually see
 * everything tagged with it, which wants a real grid. All three sections
 * share one [LazyVerticalGrid] (full-width header items via
 * `GridItemSpan(maxLineSpan)`, ordinary single-cell items otherwise) rather
 * than three separate grids, since a second scrollable nested inside
 * another of the same orientation doesn't work in Compose without a bounded
 * height — one grid with section headers sidesteps that entirely.
 */
@Composable
private fun GenreFilteredGrid(
    movies: List<MergedEntry>,
    shows: List<ShowGroup>,
    artists: List<ArtistGroup>,
    artworkUrl: (MergedEntry) -> String?,
    artistPhotoUrl: (MergedEntry) -> String?,
    onOpenMovie: (MergedEntry) -> Unit,
    onOpenShow: (ShowGroup) -> Unit,
    onOpenArtist: (ArtistGroup) -> Unit,
    isLiked: (MergedEntry) -> Boolean,
    firstFocusRequester: FocusRequester,
    firstEntryFocusRequester: FocusRequester,
    requestInitialFocus: Boolean,
    initialFocusMovieKey: String?,
    initialFocusShowKey: String?,
    initialFocusArtistKey: String?,
    hasHeader: Boolean,
    header: @Composable ((() -> Unit)?) -> Unit,
    gridState: LazyGridState,
) {
    val headerCount = if (hasHeader) 1 else 0
    val focusNavigationScope = rememberCoroutineScope()
    val firstSection = when {
        movies.isNotEmpty() -> "movies"
        shows.isNotEmpty() -> "shows"
        artists.isNotEmpty() -> "music"
        else -> null
    }
    var nextGridIndex = headerCount // Category row.
    var restoreGridIndex: Int? = null
    if (movies.isNotEmpty()) {
        nextGridIndex++ // Movies header.
        val selected = initialFocusMovieKey?.let { key -> movies.indexOfFirst { it.entry.entryKey == key } } ?: -1
        if (selected >= 0) restoreGridIndex = nextGridIndex + selected
        nextGridIndex += movies.size
    }
    if (shows.isNotEmpty()) {
        nextGridIndex++ // Shows header.
        val selected = initialFocusShowKey?.let { key -> shows.indexOfFirst { it.show == key } } ?: -1
        if (selected >= 0) restoreGridIndex = nextGridIndex + selected
        nextGridIndex += shows.size
    }
    if (artists.isNotEmpty()) {
        nextGridIndex++ // Music header.
        val selected = initialFocusArtistKey?.let { key -> artists.indexOfFirst { it.artist == key } } ?: -1
        if (selected >= 0) restoreGridIndex = nextGridIndex + selected
    }
    LaunchedEffect(restoreGridIndex, movies, shows, artists, requestInitialFocus) {
        if (requestInitialFocus) {
            if (restoreGridIndex != null) {
                gridState.scrollToItem(restoreGridIndex!!)
                withFrameNanos {}
            }
            runCatching { firstFocusRequester.requestFocus() }
        }
    }
    val navigateToFirstEntry = {
        focusNavigationScope.launch {
            // Item 0 is the category row and item 1 is the first full-width
            // section header, so item 2 is the first focusable card.
            runCatching { gridState.scrollToItem(headerCount + 1) }
            withFrameNanos {}
            runCatching { firstEntryFocusRequester.requestFocus() }
        }
        Unit
    }

    LazyVerticalGrid(
        state = gridState,
        columns = GridCells.Fixed(5),
        verticalArrangement = Arrangement.spacedBy(20.dp),
        horizontalArrangement = Arrangement.spacedBy(20.dp),
        // top = 32.dp, not the flat 12.dp every other edge gets — same
        // focus-scale headroom fix as MovieShelfScreen/ShowShelfScreen/
        // ArtistShelfScreen's identical grids, needed here too since this
        // is the same "browse all" full-grid style reached a different way
        // (picking a genre) rather than via those screens' own "Browse all"
        // button.
        contentPadding = PaddingValues(start = 12.dp, end = 12.dp, top = 32.dp, bottom = 12.dp),
    ) {
        if (hasHeader) {
            item(key = "catalog-categories", span = { GridItemSpan(maxLineSpan) }, contentType = "categories") {
                header(navigateToFirstEntry)
            }
        }
        if (movies.isNotEmpty()) {
            item(span = { GridItemSpan(maxLineSpan) }) { GridSectionHeader("Movies") }
            gridItemsIndexed(
                items = movies,
                key = { _, entry -> "movie-${entry.entry.entryKey}" },
                contentType = { _, _ -> "movie" },
            ) { index, entry ->
                CatalogCard(
                    entry,
                    artworkUrl(entry),
                    onClick = { onOpenMovie(entry) },
                    focusRequester = if (
                        entry.entry.entryKey == initialFocusMovieKey ||
                        (restoreGridIndex == null && firstSection == "movies" && index == 0)
                    ) firstFocusRequester else null,
                    additionalFocusRequester = firstEntryFocusRequester.takeIf { firstSection == "movies" && index == 0 },
                    widthModifier = Modifier.fillMaxWidth(),
                    isLiked = isLiked(entry),
                    testTag = UatTestTags.CARD_MOVIE_PREFIX + entry.entry.entryKey,
                )
            }
        }
        if (shows.isNotEmpty()) {
            item(span = { GridItemSpan(maxLineSpan) }) { GridSectionHeader("Shows") }
            gridItemsIndexed(
                items = shows,
                key = { _, show -> "show-${show.show}" },
                contentType = { _, _ -> "show" },
            ) { index, show ->
                val representative = show.seasons.firstOrNull()?.episodes?.firstOrNull()
                GroupCard(
                    title = show.show,
                    subtitle = "${show.seasons.size} season" + if (show.seasons.size == 1) "" else "s",
                    artworkUrl = representative?.let(artworkUrl),
                    onClick = { onOpenShow(show) },
                    focusRequester = if (
                        show.show == initialFocusShowKey ||
                        (restoreGridIndex == null && firstSection == "shows" && index == 0)
                    ) firstFocusRequester else null,
                    additionalFocusRequester = firstEntryFocusRequester.takeIf { firstSection == "shows" && index == 0 },
                    widthModifier = Modifier.fillMaxWidth(),
                    testTag = UatTestTags.CARD_SHOW_PREFIX + show.show,
                )
            }
        }
        if (artists.isNotEmpty()) {
            item(span = { GridItemSpan(maxLineSpan) }) { GridSectionHeader("Music") }
            gridItemsIndexed(
                items = artists,
                key = { _, artist -> "artist-${artist.artist}" },
                contentType = { _, _ -> "artist" },
            ) { index, artist ->
                val albumCount = artist.albums.size
                val artistArtwork = artist.artworkUrls(artworkUrl, artistPhotoUrl)
                GroupCard(
                    title = artist.artist,
                    subtitle = "$albumCount album" + if (albumCount == 1) "" else "s",
                    artworkUrl = artistArtwork.artistPhoto,
                    fallbackArtworkUrl = artistArtwork.albumCoverFallback,
                    artworkAspectRatio = 1f,
                    placeholderType = "Artist",
                    onClick = { onOpenArtist(artist) },
                    focusRequester = if (
                        artist.artist == initialFocusArtistKey ||
                        (restoreGridIndex == null && firstSection == "music" && index == 0)
                    ) firstFocusRequester else null,
                    additionalFocusRequester = firstEntryFocusRequester.takeIf { firstSection == "music" && index == 0 },
                    widthModifier = Modifier.fillMaxWidth(),
                    testTag = UatTestTags.CARD_ARTIST_PREFIX + artist.artist,
                )
            }
        }
    }
}

@Composable
private fun GridSectionHeader(label: String) {
    Text(label, color = SwarmMuted, fontSize = TOP_LEVEL_TITLE_SIZE, fontWeight = FontWeight.Black, modifier = Modifier.padding(bottom = 4.dp))
}

@Composable
private fun searchFieldColors() = OutlinedTextFieldDefaults.colors(
    focusedTextColor = SwarmText,
    unfocusedTextColor = SwarmText,
    focusedBorderColor = SwarmAccent,
    unfocusedBorderColor = SwarmBorder,
    cursorColor = SwarmAccent,
)

// Top-level shelf titles (Movies/Shows/Music) read noticeably larger/bolder
// than genre sub-shelf titles beneath them, so the row hierarchy is visible
// at a glance rather than every shelf title looking like the same kind of
// heading — real feedback from live use.
private val TOP_LEVEL_TITLE_SIZE = 20.sp
private val GENRE_TITLE_SIZE = 16.sp

/** No more header-level "Browse all" button — every row (root or genre) now
 * carries its own in-row [BrowseAllTile] at the end once it's over
 * [MAX_SHELF_ITEMS], so this is just the row's title. */
@Composable
private fun ShelfHeader(label: String, fontSize: TextUnit) {
    Text(label, color = SwarmMuted, fontSize = fontSize, fontWeight = FontWeight.Black)
}

/** Appended as the last card in a shelf row once it exceeds [MAX_SHELF_ITEMS]
 * — replaces the old header-level "Browse all" button with an in-row tile,
 * matching the same poster-sized footprint as every other card in the row. */
@Composable
private fun BrowseAllTile(onClick: () -> Unit, testTag: String, focusRequester: FocusRequester? = null) {
    Card(
        onClick = onClick,
        colors = CardDefaults.colors(containerColor = SwarmSurfaceMuted),
        scale = CardDefaults.scale(scale = 1f, focusedScale = 1f, pressedScale = 0.99f),
        modifier = Modifier.width(CARD_WIDTH)
            .then(if (focusRequester != null) Modifier.focusRequester(focusRequester) else Modifier)
            .testTag(testTag),
    ) {
        Box(
            modifier = Modifier.fillMaxWidth().height(CARD_MEDIA_HEIGHT).clip(RoundedCornerShape(4.dp)),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                "Browse\nAll  →",
                color = SwarmAccent,
                fontSize = 14.sp,
                fontWeight = FontWeight.Black,
                textAlign = androidx.compose.ui.text.style.TextAlign.Center,
            )
        }
    }
}

// Extra clearance below a genre sub-shelf's (smaller, tighter-packed) title
// specifically: real bug, found live — tv-material3's focus-scale animation
// on the first card in the row below grows upward too, and with shelves
// stacked as densely as the genre breakdown now does, the default spacing
// let a focused card's top edge cover its own row's title. Top-level shelf
// titles get a smaller bump for the same reason at a smaller scale (they're
// larger text needing a little more clearance than before, but nowhere near
// as many rows stacked close together).
private val TOP_LEVEL_TITLE_SPACING = 14.dp
private val GENRE_TITLE_SPACING = 22.dp

/**
 * Shared by [MovieRow]/[ShowShelfRow]/[ArtistShelfRow]: decides which index
 * (if any) this row should send initial D-pad focus to — [restoreFocusIndex]
 * when a remembered card is actually present in this row, else index 0 when
 * this is the [isDefaultFocusRow] (first non-empty top-level section, same
 * fallback as before this existed), else nothing. `scrollToItem` before
 * `requestFocus`, not just the latter alone: a restored index can be well
 * outside the row's initially-composed window, and Compose can only focus an
 * item that's actually been laid out — the `withFrameNanos` gives that
 * freshly-scrolled-to item one frame to actually compose before the focus
 * request, which would otherwise silently no-op against an item not there yet.
 */
@Composable
private fun rememberRowFocusTarget(
    itemCount: Int,
    restoreFocusIndex: Int?,
    isDefaultFocusRow: Boolean,
    listState: LazyListState,
    defaultFocusRequester: FocusRequester? = null,
    requestInitialFocus: Boolean = true,
): Pair<Int?, FocusRequester> {
    val localFocusRequester = remember { FocusRequester() }
    val focusRequester = defaultFocusRequester ?: localFocusRequester
    val targetIndex = restoreFocusIndex ?: (0.takeIf { isDefaultFocusRow })
    LaunchedEffect(targetIndex, itemCount, requestInitialFocus) {
        if (requestInitialFocus && targetIndex != null && targetIndex < itemCount) {
            listState.scrollToItem(targetIndex)
            withFrameNanos {}
            runCatching { focusRequester.requestFocus() }
        }
    }
    return targetIndex to focusRequester
}

@Composable
private fun QuickAccessRow(
    title: String,
    items: List<QuickAccessItem>,
    artworkUrl: (MergedEntry) -> String?,
    onClick: (QuickAccessItem) -> Unit,
    isLiked: (MergedEntry) -> Boolean,
    isDefaultFocusRow: Boolean,
    defaultFocusRequester: FocusRequester?,
    firstCardFocusRequester: FocusRequester?,
    onNavigateDown: (() -> Unit)? = null,
    requestInitialFocus: Boolean,
) {
    val listState = rememberLazyListState()
    val artworkUrls = remember(items, artworkUrl) { items.map { artworkUrl(it.representative) } }
    PrefetchArtworkRow(listState, artworkUrls)
    val (targetIndex, focusRequester) = rememberRowFocusTarget(
        items.size,
        restoreFocusIndex = null,
        isDefaultFocusRow = isDefaultFocusRow,
        listState = listState,
        defaultFocusRequester = defaultFocusRequester,
        requestInitialFocus = requestInitialFocus,
    )
    val rowTag = if (title == "Continue Watching") UatTestTags.ROW_CONTINUE_WATCHING else UatTestTags.ROW_WATCHLIST
    Column(modifier = Modifier.testTag(rowTag)) {
        ShelfHeader(title, fontSize = TOP_LEVEL_TITLE_SIZE)
        Spacer(Modifier.height(TOP_LEVEL_TITLE_SPACING))
        LazyRow(
            state = listState,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            contentPadding = PaddingValues(horizontal = 12.dp),
        ) {
            itemsIndexed(
                items = items,
                key = { _, item -> item.key },
                contentType = { _, item -> "quick-${item.kind}" },
            ) { index, item ->
                CatalogCard(
                    merged = item.representative,
                    artworkUrl = artworkUrls[index],
                    onClick = { onClick(item) },
                    focusRequester = focusRequester.takeIf { index == targetIndex },
                    additionalFocusRequester = firstCardFocusRequester.takeIf { index == 0 },
                    onNavigateDown = onNavigateDown,
                    isLiked = isLiked(item.representative),
                    titleOverride = item.title,
                    subtitle = item.subtitle,
                    progress = item.progress,
                    placeholderType = if (item.kind == QuickAccessKind.MOVIE) "Movie" else "Show",
                    testTag = UatTestTags.CARD_QUICK_ACCESS_PREFIX + item.key,
                )
            }
        }
    }
}

@Composable
private fun MovieRow(
    title: String,
    movies: List<MergedEntry>,
    artworkUrl: (MergedEntry) -> String?,
    onOpenMovie: (MergedEntry) -> Unit,
    onOpenShelf: (List<MergedEntry>) -> Unit,
    isTopLevel: Boolean,
    restoreFocusIndex: Int?,
    isDefaultFocusRow: Boolean,
    isLiked: (MergedEntry) -> Boolean,
    defaultFocusRequester: FocusRequester? = null,
    firstCardFocusRequester: FocusRequester? = null,
    requestInitialFocus: Boolean = true,
    preview: BrowsePreview?,
    expandedPreviewEntryKey: String?,
    onPreviewFocusChanged: (MergedEntry, Boolean) -> Unit,
    onPreviewFinished: (String) -> Unit,
) {
    val visibleMovies = remember(movies) { movies.take(MAX_SHELF_ITEMS) }
    val showBrowseAllTile = movies.size > MAX_SHELF_ITEMS
    val listState = rememberLazyListState()
    val artworkUrls = remember(visibleMovies, artworkUrl) { visibleMovies.map(artworkUrl) }
    PrefetchArtworkRow(listState, artworkUrls)
    val (targetIndex, focusRequester) = rememberRowFocusTarget(
        visibleMovies.size + if (showBrowseAllTile) 1 else 0,
        restoreFocusIndex,
        isDefaultFocusRow,
        listState,
        defaultFocusRequester,
        requestInitialFocus,
    )
    Column(modifier = if (isTopLevel) Modifier.testTag(UatTestTags.SHELF_MOVIES) else Modifier) {
        ShelfHeader(title, if (isTopLevel) TOP_LEVEL_TITLE_SIZE else GENRE_TITLE_SIZE)
        Spacer(Modifier.height(if (isTopLevel) TOP_LEVEL_TITLE_SPACING else GENRE_TITLE_SPACING))
        // contentPadding, not just the Column's own outer padding: tv-material3's
        // Card scales up in place when it gains focus, so the leftmost/topmost card
        // in an unpadded LazyRow/LazyColumn scales outward past the layout's own
        // bounds and gets clipped by the scrolling container itself — confirmed live
        // (left edge of the first card in each shelf renders off-screen when focused).
        // Reserving a little extra space inside the scrollable area gives the scale
        // animation room without moving any card's resting position.
        LazyRow(state = listState, horizontalArrangement = Arrangement.spacedBy(12.dp), contentPadding = PaddingValues(horizontal = 12.dp)) {
            itemsIndexed(
                items = visibleMovies,
                key = { _, entry -> entry.entry.entryKey },
                contentType = { _, _ -> "movie" },
            ) { index, entry ->
                CatalogCard(
                    entry,
                    artworkUrls[index],
                    onClick = { onOpenMovie(entry) },
                    focusRequester = if (index == targetIndex) focusRequester else null,
                    additionalFocusRequester = firstCardFocusRequester.takeIf { index == 0 },
                    isLiked = isLiked(entry),
                    preview = preview,
                    expandedPreviewEntryKey = expandedPreviewEntryKey,
                    onPreviewFocusChanged = onPreviewFocusChanged,
                    onPreviewFinished = onPreviewFinished,
                    testTag = UatTestTags.CARD_MOVIE_PREFIX + entry.entry.entryKey,
                )
            }
            if (showBrowseAllTile) {
                item(key = "browse-all", contentType = "browse-all") {
                    BrowseAllTile(
                        onClick = { onOpenShelf(movies) },
                        testTag = UatTestTags.BROWSE_ALL_MOVIES,
                        focusRequester = focusRequester.takeIf { targetIndex == visibleMovies.size },
                    )
                }
            }
        }
    }
}

@Composable
private fun ShowShelfRow(
    title: String,
    shows: List<ShowGroup>,
    artworkUrl: (MergedEntry) -> String?,
    onOpenShowShelf: (List<ShowGroup>) -> Unit,
    onOpenShow: (ShowGroup) -> Unit,
    isTopLevel: Boolean,
    restoreFocusIndex: Int?,
    isDefaultFocusRow: Boolean,
    defaultFocusRequester: FocusRequester? = null,
    firstCardFocusRequester: FocusRequester? = null,
    requestInitialFocus: Boolean = true,
    preview: BrowsePreview?,
    expandedPreviewEntryKey: String?,
    onPreviewFocusChanged: (MergedEntry, Boolean) -> Unit,
    onPreviewFinished: (String) -> Unit,
) {
    val visibleShows = remember(shows) { shows.take(MAX_SHELF_ITEMS) }
    val showBrowseAllTile = shows.size > MAX_SHELF_ITEMS
    val listState = rememberLazyListState()
    // Keep each card's random choice stable across recompositions/focus
    // animation. A refreshed show list produces a fresh season+episode pick.
    val previewEntries = remember(visibleShows) {
        visibleShows.map(CatalogGrouping::randomPreviewEpisode)
    }
    val artworkUrls = remember(previewEntries, artworkUrl) {
        previewEntries.map { it?.let(artworkUrl) }
    }
    val realSeasonCounts = remember(visibleShows) {
        visibleShows.map { CatalogGrouping.previewSeasons(it).size }
    }
    PrefetchArtworkRow(listState, artworkUrls)
    val (targetIndex, focusRequester) = rememberRowFocusTarget(
        visibleShows.size + if (showBrowseAllTile) 1 else 0,
        restoreFocusIndex,
        isDefaultFocusRow,
        listState,
        defaultFocusRequester,
        requestInitialFocus,
    )
    Column(modifier = if (isTopLevel) Modifier.testTag(UatTestTags.SHELF_SHOWS) else Modifier) {
        ShelfHeader(title, if (isTopLevel) TOP_LEVEL_TITLE_SIZE else GENRE_TITLE_SIZE)
        Spacer(Modifier.height(if (isTopLevel) TOP_LEVEL_TITLE_SPACING else GENRE_TITLE_SPACING))
        LazyRow(state = listState, horizontalArrangement = Arrangement.spacedBy(12.dp), contentPadding = PaddingValues(horizontal = 12.dp)) {
            itemsIndexed(
                items = visibleShows,
                key = { _, show -> show.show },
                contentType = { _, _ -> "show" },
            ) { index, show ->
                GroupCard(
                    title = show.show,
                    subtitle = "${realSeasonCounts[index]} season" + if (realSeasonCounts[index] == 1) "" else "s",
                    artworkUrl = artworkUrls[index],
                    onClick = { onOpenShow(show) },
                    focusRequester = if (index == targetIndex) focusRequester else null,
                    additionalFocusRequester = firstCardFocusRequester.takeIf { index == 0 },
                    previewEntry = previewEntries[index],
                    preview = preview,
                    expandedPreviewEntryKey = expandedPreviewEntryKey,
                    onPreviewFocusChanged = onPreviewFocusChanged,
                    onPreviewFinished = onPreviewFinished,
                    testTag = UatTestTags.CARD_SHOW_PREFIX + show.show,
                )
            }
            if (showBrowseAllTile) {
                item(key = "browse-all", contentType = "browse-all") {
                    BrowseAllTile(
                        onClick = { onOpenShowShelf(shows) },
                        testTag = UatTestTags.BROWSE_ALL_SHOWS,
                        focusRequester = focusRequester.takeIf { targetIndex == visibleShows.size },
                    )
                }
            }
        }
    }
}

@Composable
private fun ArtistShelfRow(
    title: String,
    artists: List<ArtistGroup>,
    artworkUrl: (MergedEntry) -> String?,
    artistPhotoUrl: (MergedEntry) -> String?,
    onOpenArtistShelf: (List<ArtistGroup>) -> Unit,
    onOpenArtist: (ArtistGroup) -> Unit,
    isTopLevel: Boolean,
    restoreFocusIndex: Int?,
    isDefaultFocusRow: Boolean,
    defaultFocusRequester: FocusRequester? = null,
    firstCardFocusRequester: FocusRequester? = null,
    requestInitialFocus: Boolean = true,
    preview: BrowsePreview?,
    expandedPreviewEntryKey: String?,
    onPreviewFocusChanged: (MergedEntry, Boolean) -> Unit,
    onPreviewFinished: (String) -> Unit,
) {
    val visibleArtists = remember(artists) { artists.take(MAX_SHELF_ITEMS) }
    val showBrowseAllTile = artists.size > MAX_SHELF_ITEMS
    val listState = rememberLazyListState()
    val artistArtwork = remember(visibleArtists, artworkUrl, artistPhotoUrl) {
        visibleArtists.map { it.artworkUrls(artworkUrl, artistPhotoUrl) }
    }
    // Same hover-preview rules as Shows: one stable representative track per
    // artist, kept across recompositions/focus animation so it doesn't
    // resample on every focus change.
    val previewEntries = remember(visibleArtists) {
        visibleArtists.map { it.randomPreviewTrack() }
    }
    PrefetchArtworkRow(listState, artistArtwork.map { it.artistPhoto ?: it.albumCoverFallback })
    val (targetIndex, focusRequester) = rememberRowFocusTarget(
        visibleArtists.size + if (showBrowseAllTile) 1 else 0,
        restoreFocusIndex,
        isDefaultFocusRow,
        listState,
        defaultFocusRequester,
        requestInitialFocus,
    )
    Column(modifier = if (isTopLevel) Modifier.testTag(UatTestTags.SHELF_MUSIC) else Modifier) {
        ShelfHeader(title, if (isTopLevel) TOP_LEVEL_TITLE_SIZE else GENRE_TITLE_SIZE)
        Spacer(Modifier.height(if (isTopLevel) TOP_LEVEL_TITLE_SPACING else GENRE_TITLE_SPACING))
        LazyRow(state = listState, horizontalArrangement = Arrangement.spacedBy(12.dp), contentPadding = PaddingValues(horizontal = 12.dp)) {
            itemsIndexed(
                items = visibleArtists,
                key = { _, artist -> artist.artist },
                contentType = { _, _ -> "artist" },
            ) { index, artist ->
                val albumCount = artist.albums.size
                val artwork = artistArtwork[index]
                GroupCard(
                    title = artist.artist,
                    subtitle = "$albumCount album" + if (albumCount == 1) "" else "s",
                    artworkUrl = artwork.artistPhoto,
                    fallbackArtworkUrl = artwork.albumCoverFallback,
                    artworkAspectRatio = 1f,
                    placeholderType = "Artist",
                    onClick = { onOpenArtist(artist) },
                    focusRequester = if (index == targetIndex) focusRequester else null,
                    additionalFocusRequester = firstCardFocusRequester.takeIf { index == 0 },
                    previewEntry = previewEntries[index],
                    preview = preview,
                    expandedPreviewEntryKey = expandedPreviewEntryKey,
                    onPreviewFocusChanged = onPreviewFocusChanged,
                    onPreviewFinished = onPreviewFinished,
                    testTag = UatTestTags.CARD_ARTIST_PREFIX + artist.artist,
                )
            }
            if (showBrowseAllTile) {
                item(key = "browse-all", contentType = "browse-all") {
                    BrowseAllTile(
                        onClick = { onOpenArtistShelf(artists) },
                        testTag = UatTestTags.BROWSE_ALL_MUSIC,
                        focusRequester = focusRequester.takeIf { targetIndex == visibleArtists.size },
                    )
                }
            }
        }
    }
}

/** Same "preview eligible" concept as [CatalogGrouping.randomPreviewEpisode],
 * for Music: one track, picked from a random album so a multi-album artist's
 * preview varies rather than always sampling album one. */
private fun ArtistGroup.randomPreviewTrack(): MergedEntry? =
    albums.randomOrNull()?.tracks?.randomOrNull()

// Card width: smaller than this screen used to be (was 160.dp) so more
// fit across one horizontal row at once — the same "see more per row"
// request the media server's browse grid already satisfies with its own,
// much smaller thumbnails.
private val CARD_WIDTH = 130.dp
private val CARD_MEDIA_HEIGHT = 195.dp
private val PREVIEW_CARD_WIDTH = 347.dp

@Composable
private fun CatalogCard(
    merged: MergedEntry,
    artworkUrl: String?,
    onClick: () -> Unit,
    focusRequester: FocusRequester?,
    additionalFocusRequester: FocusRequester? = null,
    widthModifier: Modifier = Modifier.width(CARD_WIDTH),
    isLiked: Boolean = false,
    preview: BrowsePreview? = null,
    expandedPreviewEntryKey: String? = null,
    onPreviewFocusChanged: ((MergedEntry, Boolean) -> Unit)? = null,
    onPreviewFinished: (String) -> Unit = {},
    titleOverride: String? = null,
    subtitle: String? = null,
    progress: Float? = null,
    placeholderType: String = "Movie",
    onNavigateDown: (() -> Unit)? = null,
    testTag: String? = null,
) {
    var isFocused by remember(merged.entry.entryKey) { mutableStateOf(false) }
    val isPreviewExpanded = isFocused && expandedPreviewEntryKey == merged.entry.entryKey
    val animatedWidth by animateDpAsState(if (isPreviewExpanded) PREVIEW_CARD_WIDTH else CARD_WIDTH)
    val previewAlpha by animateFloatAsState(if (isPreviewExpanded) 1f else 0f, label = "movie-preview-alpha")
    val focusModifier = Modifier
        .then(
            if (onNavigateDown != null) {
                Modifier.onPreviewKeyEvent { event ->
                    if (event.type == KeyEventType.KeyDown && event.key == Key.DirectionDown) {
                        onNavigateDown()
                        true
                    } else {
                        false
                    }
                }
            } else {
                Modifier
            },
        )
        .then(if (focusRequester != null) Modifier.focusRequester(focusRequester) else Modifier)
        .then(if (additionalFocusRequester != null) Modifier.focusRequester(additionalFocusRequester) else Modifier)
    val resolvedWidth = if (onPreviewFocusChanged != null) Modifier.width(animatedWidth) else widthModifier
    val showCardText = merged.entry.kind == MediaKind.TRACK || artworkUrl == null
    Card(
        onClick = onClick,
        colors = CardDefaults.colors(containerColor = SwarmSurface),
        scale = CardDefaults.scale(scale = 1f, focusedScale = 1f, pressedScale = 0.99f),
        modifier = focusModifier.then(resolvedWidth)
            .then(if (testTag != null) Modifier.testTag(testTag) else Modifier)
            .onFocusChanged { focusState ->
                if (isFocused != focusState.isFocused) {
                    isFocused = focusState.isFocused
                    onPreviewFocusChanged?.invoke(merged, focusState.isFocused)
                }
            },
    ) {
        Column {
            Box(modifier = Modifier.fillMaxWidth().height(CARD_MEDIA_HEIGHT).clip(RoundedCornerShape(4.dp))) {
                ArtworkImage(
                    label = merged.entry.displayTitle(),
                    placeholderType = placeholderType,
                    primaryUrl = artworkUrl,
                    modifier = Modifier.fillMaxSize(),
                )
                val activePreview = preview?.takeIf { isFocused && it.entryKey == merged.entry.entryKey }
                if (isPreviewExpanded && activePreview == null) {
                    PreviewLoadingIndicator(Modifier.fillMaxSize())
                }
                activePreview?.let {
                    BrowsePreviewPlayer(
                        preview = it,
                        shouldPlay = isPreviewExpanded,
                        onFinished = onPreviewFinished,
                        modifier = Modifier.fillMaxSize().alpha(previewAlpha),
                    )
                }
                if (isLiked) {
                    Text(
                        "♥",
                        color = SwarmLike,
                        fontSize = 16.sp,
                        fontWeight = FontWeight.Black,
                        modifier = Modifier.align(Alignment.TopEnd).padding(6.dp),
                    )
                }
                progress?.let { fraction ->
                    Box(
                        modifier = Modifier.align(Alignment.BottomStart).fillMaxWidth().height(4.dp)
                            .background(Color.Black.copy(alpha = 0.65f)),
                    ) {
                        Box(
                            modifier = Modifier.fillMaxWidth(fraction.coerceIn(0f, 1f)).height(4.dp)
                                .background(SwarmAccent),
                        )
                    }
                }
            }
            if (showCardText) {
                Column(modifier = Modifier.padding(10.dp)) {
                    Text(
                        titleOverride ?: merged.entry.displayTitle(),
                        color = SwarmText,
                        fontSize = 13.sp,
                        fontWeight = FontWeight.SemiBold,
                        minLines = 2,
                        maxLines = 2,
                    )
                    if (subtitle != null) {
                        Spacer(Modifier.height(4.dp))
                        Text(subtitle, color = SwarmMuted, fontSize = 10.sp, maxLines = 1)
                    }
                    if (merged.sources.size > 1) {
                        Spacer(Modifier.height(4.dp))
                        Text("${merged.sources.size} sources", color = SwarmAccent, fontSize = 10.sp)
                    }
                }
            }
        }
    }
}

/** Shared grouped-media card: shows use representative poster art; artists prefer a photo and then an album cover. */
@Composable
private fun GroupCard(
    title: String,
    subtitle: String,
    artworkUrl: String?,
    onClick: () -> Unit,
    focusRequester: FocusRequester?,
    additionalFocusRequester: FocusRequester? = null,
    widthModifier: Modifier = Modifier.width(CARD_WIDTH),
    fallbackArtworkUrl: String? = null,
    artworkAspectRatio: Float = 2f / 3f,
    placeholderType: String = "Show",
    previewEntry: MergedEntry? = null,
    preview: BrowsePreview? = null,
    expandedPreviewEntryKey: String? = null,
    onPreviewFocusChanged: ((MergedEntry, Boolean) -> Unit)? = null,
    onPreviewFinished: (String) -> Unit = {},
    testTag: String? = null,
) {
    var isFocused by remember(title, previewEntry?.entry?.entryKey) { mutableStateOf(false) }
    val previewEnabled = previewEntry != null && onPreviewFocusChanged != null
    val isPreviewExpanded = isFocused && previewEntry?.entry?.entryKey == expandedPreviewEntryKey
    val animatedWidth by animateDpAsState(if (isPreviewExpanded) PREVIEW_CARD_WIDTH else CARD_WIDTH)
    val previewAlpha by animateFloatAsState(if (isPreviewExpanded) 1f else 0f, label = "show-preview-alpha")
    val focusModifier = Modifier
        .then(if (focusRequester != null) Modifier.focusRequester(focusRequester) else Modifier)
        .then(if (additionalFocusRequester != null) Modifier.focusRequester(additionalFocusRequester) else Modifier)
    val resolvedWidth = if (previewEnabled) Modifier.width(animatedWidth) else widthModifier
    val showCardText = placeholderType == "Artist" || (artworkUrl == null && fallbackArtworkUrl == null)
    Card(
        onClick = onClick,
        colors = CardDefaults.colors(containerColor = SwarmSurface),
        scale = CardDefaults.scale(scale = 1f, focusedScale = 1f, pressedScale = 0.99f),
        modifier = focusModifier.then(resolvedWidth)
            .then(if (testTag != null) Modifier.testTag(testTag) else Modifier)
            .onFocusChanged { focusState ->
                if (isFocused != focusState.isFocused) {
                    isFocused = focusState.isFocused
                    previewEntry?.let { onPreviewFocusChanged?.invoke(it, focusState.isFocused) }
                }
            },
    ) {
        Column {
            if (previewEnabled) {
                Box(modifier = Modifier.fillMaxWidth().height(CARD_MEDIA_HEIGHT).clip(RoundedCornerShape(4.dp))) {
                    ArtworkImage(
                        label = title,
                        placeholderType = placeholderType,
                        primaryUrl = artworkUrl,
                        fallbackUrl = fallbackArtworkUrl,
                        modifier = Modifier.fillMaxSize(),
                    )
                    val activePreview = preview?.takeIf { isFocused && it.entryKey == previewEntry?.entry?.entryKey }
                    if (isPreviewExpanded && activePreview == null) {
                        PreviewLoadingIndicator(Modifier.fillMaxSize())
                    }
                    activePreview?.let {
                        BrowsePreviewPlayer(
                            preview = it,
                            shouldPlay = isPreviewExpanded,
                            onFinished = onPreviewFinished,
                            modifier = Modifier.fillMaxSize().alpha(previewAlpha),
                            hasVideo = previewEntry?.entry?.kind != MediaKind.TRACK,
                        )
                    }
                }
            } else {
                ArtworkImage(
                    label = title,
                    placeholderType = placeholderType,
                    primaryUrl = artworkUrl,
                    fallbackUrl = fallbackArtworkUrl,
                    modifier = Modifier.fillMaxWidth().aspectRatio(artworkAspectRatio).clip(RoundedCornerShape(4.dp)),
                )
            }
            if (showCardText) {
                Column(modifier = Modifier.padding(10.dp)) {
                    Text(title, color = SwarmText, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, minLines = 2, maxLines = 2)
                    Spacer(Modifier.height(4.dp))
                    Text(subtitle, color = SwarmMuted, fontSize = 10.sp)
                }
            }
        }
    }
}
