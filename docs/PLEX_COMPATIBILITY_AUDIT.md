# Plex Compatibility Audit — `batocera.local` library (2026-09-15)

A point-in-time audit of the live media library (`root@batocera.local`,
`/userdata/{movies,shows,music}` → `/media/roms_modern/{Movies,Shows,Music}`)
against Plex's documented directory/naming conventions. Read-only: nothing
was moved, renamed, or deleted to produce this report. Intended as the
checklist for the reorganization work that follows it.

## Method

Every finding below is checked two ways:

1. **Real Plex conventions** — Plex Support's "Naming and organizing your
   Movie/TV Show/Music files" and related articles.
2. **This codebase's own ground truth** — `crates/swarm-media/src/plex.rs`,
   the "centralized Plex media-organization compatibility layer" already in
   this repo (issue #247). Its doc comment states the design rule
   explicitly: *"Plex wins. Where existing SWARM behavior conflicts with a
   currently supported Plex convention, this module encodes the Plex
   behavior and the caller adopts it."* Every season-folder and
   extras-folder check below is a direct port of that module's
   `parse_season_dir` / `PlexExtraKind::from_dir_name` logic, not an
   approximation — so a "not recognized" verdict here is also what this
   server's own scanner would report.

## What Plex requires

| Kind | Required shape |
|---|---|
| Movie | `Movie Title (Year)/Movie Title (Year).ext`, or loose in the library root. Optional `{tmdb-id}` / `{imdb-id}` for exact matching, `{edition-Label}` for cuts. |
| TV episode | `Show Name (Year)/Season NN/Show Name - SxxEyy - Title.ext`. The season folder must literally start with the word "Season" (`Season 1`, `Season 01`, …), or be exactly `Specials` for season 0. |
| Extras | One of 8 named folders (`Behind The Scenes`, `Deleted Scenes`, `Featurettes`, `Interviews`, `Scenes`, `Shorts`, `Trailers`, `Other`) or a matching filename suffix (`-behindthescenes`, `-deleted`, …). |
| Music track | `Artist/Album/Track.ext` — exactly two directory levels between the library root and the file. |
| Subtitles | Sidecar file beside the video, or an adjacent `Subs`/`Subtitles` folder. |

## TV Shows — 39 shows

**7 shows use a season-folder name Plex does not recognize.** All fail the
same check: `parse_season_dir` strips a leading `"season"` and requires the
*entire remainder* to be a plain integer (or the literal word
`"specials"`). None of these qualify.

| Show | Folders found |
|---|---|
| Daria | `S01`–`S05` |
| Dexter | `Dexter (2006) S01`–`S08` |
| Law & Order - SVU | `S01`–`S26` (all 26 seasons) |
| Rick and Morty | `S01`–`S07` |
| Star Trek Enterprise | `S01`–`S04` |
| Dragon Ball | `Season 1 - Emperor Pilaf Saga`, `Season 2 - Tournament Saga`, … |
| Dragon Ball Z | `Season 1 - Saiyan Saga`, `Season 2 - Namek and Captain Ginyu Sagas`, … |

**11 shows have episodes sitting loose directly in the show's root folder —
no season folder at all.** Worse than a naming mismatch: there is nothing
for Plex to group these under.

| Show | Loose episode files |
|---|---|
| Tales from the Crypt | 130 (the entire show) |
| The CENTURIONS | 99 |
| Mobile Suit Gundam Wing | 73 |
| Cowboy Bebop | 32 |
| WandaVision | 13 |
| Secret Invasion | 9 |
| Rick and Morty | 6 (in addition to its `S01`-style folders above) |
| Dragon Ball | 1 |
| House | 1 |
| Sealab 2021 | 1 |
| The Cuphead Show! | 1 |

**Unrecognized subfolders** — not a season folder, not one of Plex's 8
extras categories, not `Subs`/`Subtitles`:

- `DOUG/Movie`
- `House/Sample`
- `Mobile Suit Gundam Wing/Frozen Teardrop`
- `Dragon Ball/Movies`, `Dragon Ball Z/Movies`, `Dragon Ball Z/OVAs`, `Dragon Ball Z/R1 Dragon Books`
- `The CENTURIONS/Other Sci-Fi ANIME, HERE`, `.../Other Sci-Fi CARTOONS, HERE`, `.../Other SCI-FI Movies and TV Shows, HERE`

**Misplaced non-show content:** `Shows/Batocera-music/` contains actual
music artist folders (`ATB`, `Gorillaz`, `Kyau & Albert - Discography`,
`Mudvayne`) — real audio content sitting inside the TV library, not a show.

## Movies — 449 real movie folders

- **107 of 449 (24%) have no year in the folder name** — `Alien`, `Blade`,
  `Predator`, `Scream`, `The Incredibles`, `John Wick`, and more. Not a hard
  Plex failure (Plex accepts an unyeared folder), but it materially hurts
  match accuracy, especially for franchises where several entries share a
  bare title (`Blade`, `Blade 2`, `Blade Trinity`; `Scream` through
  `Scream 4`; four unyeared `John Wick` entries, etc).
- **12 orphaned files loose in the Movies root** — stray `.vtt`/`.sub`
  subtitles and `.jpg` artwork left over from an old `"Title (1080p)"`
  naming scheme, now superseded by a properly organized `"Title (Year)/"`
  folder elsewhere for the same movie (`The Matrix Resurrections`,
  `Real Steel`, `Home Alone 1`–`5`, `Black Widow`, others). Same shape as
  the database duplicates already cleaned up in the media server, on disk
  instead of in the DB.
- **`Movies/_cleanup_leftovers/` — 28 entries**, apparently a holding
  folder from an unfinished prior reorganization pass. Most names
  (`Kill Bill Vol 1/2 (…) [1080p]`, `Deadpool`, `Batman (1989) [1080p]`,
  `Escape From LA`, `Edward Scissorhands (1990) [1080p]`,
  `Elf.2003.1080p…`, `Logan`, `A Quiet Place (2018) (…)`, `300`, …) are
  scene-release-named **duplicates of movies already organized properly
  elsewhere in the library** — the on-disk analog of the database-row
  duplicates already cleaned up, just not yet cleaned from disk. A handful
  do **not** appear to have a duplicate anywhere else in the library and
  may be genuinely new/unprocessed: `No Country for Old Men`,
  `Training Day (2001) RM4K …`, `The Super Mario Bros Movie`,
  `The Revenant`, `The Departed (2006) [1080p]`, `The Avengers`,
  `Sin City …` (both), `Ocean's Eleven (2001) …`, `Ocean's Twelve (2004) …`,
  and a 9-movie `STAR.WARS.THE.SKYWALKER.SAGA.1977-2019…COMPLETE.BOXSET…REMUX…`
  release.
- **`Movies/Shows/` contains a whole TV series bundle:**
  `DRAGON BALL SUPER (2015-2018) - Complete TV Series and 3 Movies -
  1080p DUAL AUDIO x264`, sitting unextracted in the Movies root — not
  cataloged in the Shows library at all.

## Music — 20 artists

- **6 artists have a non-standard 3-level structure** — an extra grouping
  folder between the artist and the real album folder, which Plex's
  `Artist/Album/Track` convention does not have:

  | Artist | Grouping folders found |
  |---|---|
  | ATB | `Album`, `Compilation` |
  | KoRn | `Album`, `Live & Compilation` |
  | Kyau & Albert | `Albums`, `Compilations`, `Remixes`, `Singles` |
  | Mudvayne | `Albums`, `Compilation` |
  | Staind | `Albums`, `Compilations`, `Live`, `Singles, EPs, Fan Club & Promo` |
  | Tiesto | `albums`, `Compilation`, `Singles` |

- **3 artists have tracks loose directly under the artist folder, with no
  Album folder at all:**

  | Artist | Loose files |
  |---|---|
  | Armin Van Buuren | 52 individual FLAC tracks, no album grouping whatsoever |
  | Paul Oakenfold | 5 full DJ-mix files |
  | Gorillaz | 1 (`cover.jpg` — harmless, but oddly placed) |

## Summary

| Area | Clean | Findings |
|---|---|---|
| TV Shows (39) | 22 shows | 7 unrecognized season-folder naming · 11 with ungrouped loose episodes (~366 files total) · 5 shows with unrecognized subfolders · 1 misplaced music folder |
| Movies (449 folders) | Structurally sound overall | 107 missing a year (24%) · 12 orphaned root files · a 28-item unfinished cleanup-staging folder (mostly dupes, some new content) · 1 unextracted TV bundle in the wrong root |
| Music (20 artists) | 14 artists | 6 with a non-standard grouping layer · 3 with loose/un-albumed tracks (58 files total) |
