package app.swarm.tv.app.ui

/**
 * Compose `Modifier.testTag(...)` constants for the instrumented UAT suite
 * under `androidTest/.../uat`. Additive only — these exist purely so that
 * suite can find real on-screen elements reliably; they carry no runtime
 * behavior. See the `swarm-tv-uat-suite` skill for how they're used, and
 * `swarm-e2e-suite-lockdown` for the change policy on the suite itself
 * (this file is ordinary product/testability code, not suite test logic,
 * so it isn't covered by that lockdown).
 */
object UatTestTags {
    // CatalogScreen: top-level shelves.
    const val SHELF_MOVIES = "uat_shelf_movies"
    const val SHELF_SHOWS = "uat_shelf_shows"
    const val SHELF_MUSIC = "uat_shelf_music"
    const val ROW_CONTINUE_WATCHING = "uat_row_continue_watching"
    const val ROW_WATCHLIST = "uat_row_watchlist"

    // CatalogScreen: card tiles (suffix with the entry/show/artist key at the call site).
    const val CARD_MOVIE_PREFIX = "uat_card_movie_"
    const val CARD_SHOW_PREFIX = "uat_card_show_"
    const val CARD_ARTIST_PREFIX = "uat_card_artist_"
    const val CARD_QUICK_ACCESS_PREFIX = "uat_card_quick_access_"

    // CatalogScreen: filter rail.
    const val FILTER_RAIL = "uat_filter_rail"
    const val FILTER_KIND_PREFIX = "uat_filter_kind_" // + KindFilter.name (ALL/MOVIES/SHOWS/MUSIC)
    const val FILTER_LIKED_ONLY = "uat_filter_liked_only"
    const val FILTER_GENRE_PREFIX = "uat_filter_genre_" // + genre name
    const val FILTER_RATING_PREFIX = "uat_filter_rating_" // + rating value
    const val SEARCH_FIELD = "uat_search_field"
    const val SEARCH_CLEAR_BUTTON = "uat_search_clear_button"
    const val SEARCH_NO_MATCHES = "uat_search_no_matches"
    const val BROWSE_PREVIEW_PREFIX = "uat_browse_preview_" // + playback session id
    const val BROWSE_ALL_MOVIES = "uat_browse_all_movies"
    const val BROWSE_ALL_SHOWS = "uat_browse_all_shows"
    const val BROWSE_ALL_MUSIC = "uat_browse_all_music"
    const val GRID_MOVIE_PREFIX = "uat_grid_movie_"
    const val GRID_SHOW_PREFIX = "uat_grid_show_"
    const val GRID_ARTIST_PREFIX = "uat_grid_artist_"

    // CatalogScreen: top bar.
    const val OPEN_SWARM_BUTTON = "uat_open_swarm_button"
    const val DASHBOARD_ADD_SERVER_BUTTON = "uat_dashboard_add_server_button"
    const val DASHBOARD_BROWSE_BUTTON = "uat_dashboard_browse_button"
    const val DASHBOARD_SETTINGS_BUTTON = "uat_dashboard_settings_button"

    // SwarmDashboardScreen: LAN server action modal.
    const val LAN_SERVER_MODAL = "uat_lan_server_modal"
    const val LAN_SERVER_CONNECT_BUTTON = "uat_lan_server_connect_button"
    const val LAN_SERVER_DISCONNECT_BUTTON = "uat_lan_server_disconnect_button"
    const val LAN_SERVER_FORGET_BUTTON = "uat_lan_server_forget_button"

    // STUN "Add Server" activation flow (launched from SwarmDashboardScreen).
    const val ADD_SERVER_START_BUTTON = "uat_add_server_start_button"
    const val ACTIVATION_CODE = "uat_activation_code"
    const val ACTIVATION_CANCEL_BUTTON = "uat_activation_cancel_button"

    // MovieDetailScreen.
    const val MOVIE_DETAIL_ARTWORK = "uat_movie_detail_artwork"
    const val MOVIE_DETAIL_YEAR = "uat_movie_detail_year"
    const val MOVIE_DETAIL_GENRES = "uat_movie_detail_genres"
    const val MOVIE_DETAIL_CAST = "uat_movie_detail_cast"
    const val MOVIE_DETAIL_DESCRIPTION = "uat_movie_detail_description"
    const val MOVIE_DETAIL_PLAY_BUTTON = "uat_movie_detail_play_button"
    const val MOVIE_DETAIL_LIKE_BUTTON = "uat_movie_detail_like_button"
    const val MOVIE_DETAIL_WATCHLIST_BUTTON = "uat_movie_detail_watchlist_button"
    const val MOVIE_DETAIL_REPORT_PROBLEM_BUTTON = "uat_movie_detail_report_problem_button"

    // SeasonScreen.
    const val SEASON_SCREEN_SHOW_TITLE = "uat_season_screen_show_title"
    const val SEASON_SCREEN_WATCHLIST_BUTTON = "uat_season_screen_watchlist_button"
    const val SEASON_SCREEN_RESUME_BUTTON = "uat_season_screen_resume_button"
    const val SEASON_CARD_PREFIX = "uat_season_card_" // + season number
    const val EPISODE_ITEM_PREFIX = "uat_episode_item_" // + episode entryKey

    // AlbumScreen.
    const val ALBUM_CARD_PREFIX = "uat_album_card_" // + album key
    const val TRACK_ROW_PREFIX = "uat_track_row_" // + track entryKey

    // PreparingPlaybackScreen: the instant cover shown while a fresh play is negotiated.
    const val PREPARING_PLAYBACK = "uat_preparing_playback"
    const val PREPARING_PLAYBACK_RESUME_BUTTON = "uat_preparing_playback_resume"

    // PlayerScreen / PauseOverlay.
    const val PLAYER_SURFACE = "uat_player_surface"
    const val PAUSE_LABEL = "uat_pause_label"
    const val PAUSE_TITLE = "uat_pause_title"
    // NOTE: year/duration/content-rating/community-rating+votes/resolution
    // are rendered as ONE joined Text line (`metadata.joinToString(" • ")`),
    // not separate elements — this single tag covers all of them; assert by
    // substring match, not by separately-tagged fields.
    const val PAUSE_METADATA = "uat_pause_metadata"
    const val PAUSE_GENRES = "uat_pause_genres"
    const val PAUSE_CAST = "uat_pause_cast"
    const val PAUSE_DESCRIPTION = "uat_pause_description"
    const val PAUSE_AUDIO_TRACK_PICKER = "uat_pause_audio_track_picker"
    const val PAUSE_SUBTITLE_TRACK_PICKER = "uat_pause_subtitle_track_picker"
    const val PAUSE_RESUME_BUTTON = "uat_pause_resume_button"
    const val PAUSE_NEXT_EPISODE_BUTTON = "uat_pause_next_episode_button"
    const val PAUSE_AUDIO_OPTION_PREFIX = "uat_pause_audio_option_"
    const val PAUSE_SUBTITLE_OPTION_PREFIX = "uat_pause_subtitle_option_"
    const val CONTINUE_OVERLAY = "uat_continue_overlay"
    const val CONTINUE_PLAY_NOW_BUTTON = "uat_continue_play_now_button"
    const val CONTINUE_CANCEL_BUTTON = "uat_continue_cancel_button"
    const val PLAYBACK_RELEASED_PREFIX = "uat_playback_released_" // + session id

    // MusicPlayerScreen.
    const val MUSIC_PLAYER_COVER = "uat_music_player_cover"
    const val MUSIC_PLAYER_TITLE = "uat_music_player_title"
    const val MUSIC_PLAYER_UP_NEXT = "uat_music_player_up_next"
    const val MUSIC_PLAYER_SHUFFLE_BUTTON = "uat_music_player_shuffle_button"
    const val MUSIC_PLAYER_PREVIOUS_BUTTON = "uat_music_player_previous_button"
    const val MUSIC_PLAYER_PLAY_PAUSE_BUTTON = "uat_music_player_play_pause_button"
    const val MUSIC_PLAYER_SKIP_BUTTON = "uat_music_player_skip_button"
    const val MUSIC_PLAYER_LIKE_BUTTON = "uat_music_player_like_button"
    const val MUSIC_PLAYER_CLOSE_BUTTON = "uat_music_player_close_button"

    // MiniPlayerBar.
    const val MINI_PLAYER_REOPEN = "uat_mini_player_reopen"
    const val MINI_PLAYER_CLOSE_BUTTON = "uat_mini_player_close_button"

    // SwarmSettingsScreen / NotificationInbox. Reached via OPEN_SWARM_BUTTON
    // above, then this in-screen "Notifications" tab (there is no
    // separately-labeled "Settings" tab inside this screen — its tab row is
    // General/Family/Notifications/Testing).
    const val NOTIFICATIONS_TAB_BUTTON = "uat_notifications_tab_button"
    const val NOTIFICATION_ROW_PREFIX = "uat_notification_row_" // + notification key
    const val NOTIFICATION_DISMISS_BUTTON = "uat_notification_dismiss_button"

    // SwarmSettingsScreen / Kid Mode.
    const val FAMILY_TAB_BUTTON = "uat_family_tab_button"
    const val KID_MODE_STATUS = "uat_kid_mode_status"
    const val KID_MODE_MANAGE_BUTTON = "uat_kid_mode_manage_button"
    const val KID_MODE_PIN_PROMPT = "uat_kid_mode_pin_prompt"
    const val KID_MODE_PIN_ERROR = "uat_kid_mode_pin_error"
    const val KID_MODE_KIND_MOVIES = "uat_kid_mode_kind_movies"
    const val KID_MODE_KIND_SHOWS = "uat_kid_mode_kind_shows"
    const val KID_MODE_KIND_MUSIC = "uat_kid_mode_kind_music"
    const val KID_MODE_SAVE_BUTTON = "uat_kid_mode_save_button"
    const val KID_MODE_DISABLE_BUTTON = "uat_kid_mode_disable_button"
    const val NUMBER_PAD_KEY_PREFIX = "uat_number_pad_key_" // + digit/backspace

    // Debug-only testing-mode lifecycle marker rendered by MainActivity.
    const val TRANSPORT_RECOVERY_PREFIX = "uat_transport_recovery_" // + generation
}
