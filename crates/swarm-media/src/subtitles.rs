//! Side-loaded subtitle sidecars: the `.srt`/`.vtt` files that sit next to a
//! movie or episode on disk — either directly beside it, or gathered into a
//! `Subs`/`Subtitles` folder (the DVD-rip convention). The library scan
//! discovers them (see [`crate::scan`]) and registers each against its owning
//! catalog entry as a `source = "external"` row in `subtitle_tracks`, so the
//! existing peer playback protocol offers them exactly like a Whisper- or
//! OpenSubtitles-sourced track and the device UI can toggle them on or off.
//!
//! Everything here is pure path/text logic with no filesystem or database
//! access, so it stays unit-testable in isolation.

use swarm_core::peer::MediaKind;

/// Recognized subtitle sidecar extensions. Deliberately limited to the two
/// text formats the peer subtitle route can hand a client as WebVTT — `.srt`
/// is converted on the way out (see [`srt_to_webvtt`]), `.vtt` passes
/// through untouched. Image-based VobSub (`.sub`/`.idx`) and the
/// styling-heavy `.ass`/`.ssa` formats are skipped rather than mis-served as
/// something they are not.
pub const SUBTITLE_EXTS: &[&str] = &["srt", "vtt"];

/// The canonical lowercase extension for a subtitle sidecar path, or `None`
/// for anything that is not one — same allowlist discipline as
/// [`crate::classify::media_extension`].
pub fn subtitle_extension(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?.to_lowercase();
    SUBTITLE_EXTS.iter().copied().find(|known| *known == ext)
}

/// Folder names (case-insensitive) that gather subtitles away from the media
/// they belong to. When a sidecar lives inside one of these, its owning
/// video is looked for in the directory the folder itself sits in, not the
/// folder.
const SUBS_FOLDER_NAMES: &[&str] = &["subs", "sub", "subtitles", "subtitle"];

fn is_subs_folder(name: &str) -> bool {
    SUBS_FOLDER_NAMES.contains(&name.to_lowercase().as_str())
}

/// `(token, ISO-ish code, display name)`. Both the 2-letter code, the
/// 3-letter code, and the English name are accepted as a trailing filename
/// token (`Movie.en.srt`, `Movie.eng.srt`, `Movie.english.srt`, or a bare
/// `English.srt` inside a `Subs` folder).
const LANGUAGES: &[(&str, &str, &str)] = &[
    ("en", "en", "English"),
    ("eng", "en", "English"),
    ("english", "en", "English"),
    ("es", "es", "Spanish"),
    ("spa", "es", "Spanish"),
    ("spanish", "es", "Spanish"),
    ("fr", "fr", "French"),
    ("fre", "fr", "French"),
    ("fra", "fr", "French"),
    ("french", "fr", "French"),
    ("de", "de", "German"),
    ("ger", "de", "German"),
    ("deu", "de", "German"),
    ("german", "de", "German"),
    ("it", "it", "Italian"),
    ("ita", "it", "Italian"),
    ("italian", "it", "Italian"),
    ("pt", "pt", "Portuguese"),
    ("por", "pt", "Portuguese"),
    ("portuguese", "pt", "Portuguese"),
    ("nl", "nl", "Dutch"),
    ("dut", "nl", "Dutch"),
    ("nld", "nl", "Dutch"),
    ("dutch", "nl", "Dutch"),
    ("sv", "sv", "Swedish"),
    ("swe", "sv", "Swedish"),
    ("swedish", "sv", "Swedish"),
    ("no", "no", "Norwegian"),
    ("nor", "no", "Norwegian"),
    ("norwegian", "no", "Norwegian"),
    ("da", "da", "Danish"),
    ("dan", "da", "Danish"),
    ("danish", "da", "Danish"),
    ("fi", "fi", "Finnish"),
    ("fin", "fi", "Finnish"),
    ("finnish", "fi", "Finnish"),
    ("pl", "pl", "Polish"),
    ("pol", "pl", "Polish"),
    ("polish", "pl", "Polish"),
    ("ru", "ru", "Russian"),
    ("rus", "ru", "Russian"),
    ("russian", "ru", "Russian"),
    ("ja", "ja", "Japanese"),
    ("jpn", "ja", "Japanese"),
    ("japanese", "ja", "Japanese"),
    ("ko", "ko", "Korean"),
    ("kor", "ko", "Korean"),
    ("korean", "ko", "Korean"),
    ("zh", "zh", "Chinese"),
    ("chi", "zh", "Chinese"),
    ("zho", "zh", "Chinese"),
    ("chinese", "zh", "Chinese"),
    ("ar", "ar", "Arabic"),
    ("ara", "ar", "Arabic"),
    ("arabic", "ar", "Arabic"),
    ("tr", "tr", "Turkish"),
    ("tur", "tr", "Turkish"),
    ("turkish", "tr", "Turkish"),
    ("cs", "cs", "Czech"),
    ("cze", "cs", "Czech"),
    ("czech", "cs", "Czech"),
    ("el", "el", "Greek"),
    ("gre", "el", "Greek"),
    ("greek", "el", "Greek"),
    ("he", "he", "Hebrew"),
    ("heb", "he", "Hebrew"),
    ("hebrew", "he", "Hebrew"),
    ("hi", "hi", "Hindi"),
    ("hin", "hi", "Hindi"),
    ("hindi", "hi", "Hindi"),
    ("hu", "hu", "Hungarian"),
    ("hun", "hu", "Hungarian"),
    ("hungarian", "hu", "Hungarian"),
    ("ro", "ro", "Romanian"),
    ("ron", "ro", "Romanian"),
    ("romanian", "ro", "Romanian"),
    ("uk", "uk", "Ukrainian"),
    ("ukr", "uk", "Ukrainian"),
    ("ukrainian", "uk", "Ukrainian"),
    ("th", "th", "Thai"),
    ("tha", "th", "Thai"),
    ("thai", "th", "Thai"),
];

/// Trailing tokens that qualify a subtitle rather than name its language —
/// `Movie.en.forced.srt`, `Movie.eng.sdh.srt`. `whisper` marks a track this
/// server generated itself (see `transcription::whisper_subtitle_path`) as
/// opposed to a downloaded one, which is worth surfacing in the label.
const MODIFIER_TOKENS: &[(&str, &str)] = &[
    ("forced", "Forced"),
    ("sdh", "SDH"),
    ("cc", "CC"),
    ("hearingimpaired", "SDH"),
    ("whisper", "Whisper"),
];

/// Trailing tokens that are pure filler — they peel like a modifier so
/// parsing can continue past them to a real language/modifier token behind
/// them, but contribute nothing to the label themselves. `whisper_
/// subtitle_path` names every generated file `<video-stem>-whisper-
/// english-subtitles.vtt`; without peeling `subtitles` here, parsing halted
/// on that generic word before ever reaching `english`, and the file could
/// never be recognized as belonging to its own video.
const NOISE_TOKENS: &[&str] = &["subtitle", "subtitles"];

fn lookup_language(token: &str) -> Option<(&'static str, &'static str)> {
    LANGUAGES
        .iter()
        .find(|(name, _, _)| *name == token)
        .map(|(_, code, label)| (*code, *label))
}

fn lookup_modifier(token: &str) -> Option<&'static str> {
    MODIFIER_TOKENS
        .iter()
        .find(|(name, _)| *name == token)
        .map(|(_, label)| *label)
}

fn is_noise_token(token: &str) -> bool {
    NOISE_TOKENS.contains(&token)
}

const SEPARATORS: [char; 4] = ['.', '_', ' ', '-'];

/// A subtitle filename stem split into the part that identifies the video it
/// belongs to and the language/qualifier tokens trailing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSubtitleName {
    /// The stem with recognized trailing language/modifier tokens removed —
    /// what gets matched against a sibling video's own stem.
    pub base_stem: String,
    /// ISO-ish language code when a language token was recognized.
    pub language: Option<String>,
    /// Human-facing track label. Empty only when neither a language nor a
    /// modifier could be derived (the caller then falls back to a folder
    /// hint or a generic label).
    pub label: String,
}

/// Peel recognized language and modifier tokens off the end of a subtitle
/// filename stem. `Movie (2019).en.forced` → base `Movie (2019)`, language
/// `en`, label `English (Forced)`; a bare `English` → base ``, language
/// `en`, label `English`.
pub fn parse_subtitle_name(stem: &str) -> ParsedSubtitleName {
    let mut end = stem.len();
    let mut language: Option<(&str, &str)> = None;
    let mut modifiers: Vec<&str> = Vec::new();

    loop {
        let slice = &stem[..end];
        let (token_start, token) = match slice.rfind(SEPARATORS) {
            Some(idx) => (idx + 1, &slice[idx + 1..]),
            None => (0, slice),
        };
        if token.is_empty() {
            break;
        }
        let lower = token.to_lowercase();
        let consumed = if language.is_none() && lookup_language(&lower).is_some() {
            language = lookup_language(&lower);
            true
        } else if let Some(modifier) = lookup_modifier(&lower) {
            modifiers.push(modifier);
            true
        } else {
            is_noise_token(&lower)
        };
        if !consumed {
            break;
        }
        if token_start == 0 {
            end = 0;
            break;
        }
        end = token_start - 1;
    }

    let base_stem = stem[..end].trim_end_matches(SEPARATORS).to_string();
    modifiers.reverse();
    let label = match (language, modifiers.is_empty()) {
        (Some((_, name)), true) => name.to_string(),
        (Some((_, name)), false) => format!("{name} ({})", modifiers.join(", ")),
        (None, false) => modifiers.join(", "),
        (None, true) => String::new(),
    };
    ParsedSubtitleName {
        base_stem,
        language: language.map(|(code, _)| code.to_string()),
        label,
    }
}

fn strip_ext(name: &str) -> &str {
    name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name)
}

/// Collapse separators and case so two spellings of the same name compare
/// equal (`The.Movie.2019` vs `The Movie 2019`).
fn normalize_stem(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_space = true;
    for ch in raw.chars() {
        let mapped = if SEPARATORS.contains(&ch) { ' ' } else { ch };
        if mapped == ' ' {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            for lower in mapped.to_lowercase() {
                out.push(lower);
            }
            last_space = false;
        }
    }
    out.trim().to_string()
}

/// The directory a subtitle sidecar's owning video should be looked for in,
/// plus any extra folder names between a `Subs` folder and the file that can
/// serve as name hints (`Subs/Inception (2019)/eng.srt` → hint
/// `Inception (2019)`). `subtitle_relative_path` uses forward slashes and is
/// in the same stored form as a catalog entry's `relative_path` (so it
/// carries the `{label}/` prefix in a multi-root install, exactly like the
/// entries it is compared against).
pub fn subtitle_media_dir(subtitle_relative_path: &str) -> (String, Vec<String>) {
    let segments: Vec<&str> = subtitle_relative_path
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if segments.len() < 2 {
        return (String::new(), Vec::new());
    }
    let dirs = &segments[..segments.len() - 1];
    if let Some(subs_idx) = dirs.iter().rposition(|d| is_subs_folder(d)) {
        let media_dir = dirs[..subs_idx].join("/");
        let hints = dirs[subs_idx + 1..]
            .iter()
            .map(|hint| hint.to_string())
            .collect();
        (media_dir, hints)
    } else {
        (dirs.join("/"), Vec::new())
    }
}

/// A candidate video entry the subtitle might belong to, reduced to just the
/// fields the match needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleCandidate {
    pub entry_key: String,
    pub relative_path: String,
    pub kind: MediaKind,
    pub season: Option<u32>,
    pub episode: Option<u32>,
}

fn parent_dir(relative_path: &str) -> &str {
    relative_path
        .rsplit_once('/')
        .map(|(dir, _)| dir)
        .unwrap_or("")
}

/// Choose which of `candidates` (every video entry under the subtitle's
/// [`subtitle_media_dir`]) a sidecar belongs to. Returns `None` when the
/// choice is ambiguous — a subtitle silently attached to the wrong film is
/// worse than one that simply isn't offered.
pub fn match_subtitle_to_video<'a>(
    subtitle_relative_path: &str,
    candidates: &'a [SubtitleCandidate],
) -> Option<&'a SubtitleCandidate> {
    let file_name = subtitle_relative_path.rsplit('/').next()?;
    let stem = strip_ext(file_name);
    let (media_dir, hints) = subtitle_media_dir(subtitle_relative_path);
    let parsed = parse_subtitle_name(stem);

    // Prefer entries sitting directly in the media directory over ones
    // nested deeper (a movie's own extras subfolders, say).
    let direct: Vec<&SubtitleCandidate> = candidates
        .iter()
        .filter(|c| parent_dir(&c.relative_path) == media_dir)
        .collect();
    let pool: Vec<&SubtitleCandidate> = if direct.is_empty() {
        candidates.iter().collect()
    } else {
        direct
    };
    if pool.is_empty() {
        return None;
    }
    if pool.len() == 1 {
        return pool.into_iter().next();
    }

    // An episode marker in the subtitle name pins it to one episode.
    if let Some((season, episode)) = crate::classify::episode_marker(&parsed.base_stem) {
        let matches: Vec<&SubtitleCandidate> = pool
            .iter()
            .copied()
            .filter(|c| {
                c.kind == MediaKind::Episode
                    && c.season == Some(season)
                    && c.episode == Some(episode)
            })
            .collect();
        if matches.len() == 1 {
            return Some(matches[0]);
        }
    }

    // Otherwise match the subtitle's base name (or a Subs-folder hint)
    // against each candidate video's own stem.
    let mut wanted = hints.clone();
    if !parsed.base_stem.is_empty() {
        wanted.push(parsed.base_stem.clone());
    }
    let wanted: Vec<String> = wanted.iter().map(|w| normalize_stem(w)).collect();
    let exact: Vec<&SubtitleCandidate> = pool
        .iter()
        .copied()
        .filter(|c| {
            let candidate_stem = normalize_stem(strip_ext(
                c.relative_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&c.relative_path),
            ));
            wanted.iter().any(|w| {
                !w.is_empty()
                    && (*w == candidate_stem
                        || w.starts_with(&format!("{candidate_stem} "))
                        || candidate_stem.starts_with(&format!("{w} ")))
            })
        })
        .collect();
    if exact.len() == 1 {
        return Some(exact[0]);
    }
    None
}

/// Convert SubRip (`.srt`) text to WebVTT. Idempotent on input that is
/// already WebVTT. Mirrors the converter the OpenSubtitles download path
/// uses, so a side-loaded `.srt` reaches a client identically to a
/// downloaded one.
pub fn srt_to_webvtt(input: &str) -> String {
    let normalized = input.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    if normalized.trim_start().starts_with("WEBVTT") {
        return normalized;
    }
    let mut output = String::from("WEBVTT\n\n");
    for line in normalized.lines() {
        if line.contains(" --> ") {
            output.push_str(&line.replace(',', "."));
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_text_subtitle_extensions() {
        assert_eq!(subtitle_extension("Movie.en.srt"), Some("srt"));
        assert_eq!(subtitle_extension("Movie.VTT"), Some("vtt"));
        assert_eq!(subtitle_extension("Movie.sub"), None);
        assert_eq!(subtitle_extension("Movie.ass"), None);
        assert_eq!(subtitle_extension("Movie.mkv"), None);
    }

    #[test]
    fn parses_trailing_language_token() {
        let parsed = parse_subtitle_name("10.Cloverfield.Lane.2016.1080p.en");
        assert_eq!(parsed.base_stem, "10.Cloverfield.Lane.2016.1080p");
        assert_eq!(parsed.language.as_deref(), Some("en"));
        assert_eq!(parsed.label, "English");
    }

    #[test]
    fn parses_language_and_modifier_tokens() {
        let parsed = parse_subtitle_name("Inception (2010).eng.forced");
        assert_eq!(parsed.base_stem, "Inception (2010)");
        assert_eq!(parsed.language.as_deref(), Some("en"));
        assert_eq!(parsed.label, "English (Forced)");
    }

    #[test]
    fn parses_bare_language_name() {
        let parsed = parse_subtitle_name("English");
        assert_eq!(parsed.base_stem, "");
        assert_eq!(parsed.language.as_deref(), Some("en"));
        assert_eq!(parsed.label, "English");
    }

    #[test]
    fn parses_a_whisper_generated_subtitle_name() {
        // Exact shape `transcription::whisper_subtitle_path` produces:
        // `<video-stem>-whisper-english-subtitles.vtt`. The generic
        // trailing `subtitles` token must not block peeling through to the
        // real language token behind it, or the file can never be matched
        // back to its own video.
        let parsed = parse_subtitle_name("28.Days.Later.2002.1080p-whisper-english-subtitles");
        assert_eq!(parsed.base_stem, "28.Days.Later.2002.1080p");
        assert_eq!(parsed.language.as_deref(), Some("en"));
        assert_eq!(parsed.label, "English (Whisper)");
    }

    #[test]
    fn leaves_a_plain_stem_untouched() {
        let parsed = parse_subtitle_name("Inception (2010)");
        assert_eq!(parsed.base_stem, "Inception (2010)");
        assert_eq!(parsed.language, None);
        assert_eq!(parsed.label, "");
    }

    #[test]
    fn subs_folder_resolves_to_the_directory_it_sits_in() {
        let (dir, hints) = subtitle_media_dir("movies/Inception (2010)/Subs/2_English.srt");
        assert_eq!(dir, "movies/Inception (2010)");
        assert!(hints.is_empty());
    }

    #[test]
    fn subs_folder_with_a_per_movie_subfolder_keeps_it_as_a_hint() {
        let (dir, hints) = subtitle_media_dir("movies/Subs/Inception (2010)/English.srt");
        assert_eq!(dir, "movies");
        assert_eq!(hints, vec!["Inception (2010)".to_string()]);
    }

    #[test]
    fn plain_sidecar_resolves_to_its_own_directory() {
        let (dir, hints) = subtitle_media_dir("movies/Inception (2010)/Inception.en.srt");
        assert_eq!(dir, "movies/Inception (2010)");
        assert!(hints.is_empty());
    }

    fn movie(key: &str, path: &str) -> SubtitleCandidate {
        SubtitleCandidate {
            entry_key: key.into(),
            relative_path: path.into(),
            kind: MediaKind::Movie,
            season: None,
            episode: None,
        }
    }

    fn episode(key: &str, path: &str, season: u32, ep: u32) -> SubtitleCandidate {
        SubtitleCandidate {
            entry_key: key.into(),
            relative_path: path.into(),
            kind: MediaKind::Episode,
            season: Some(season),
            episode: Some(ep),
        }
    }

    #[test]
    fn subs_folder_single_movie_in_directory_is_matched() {
        let candidates = vec![movie(
            "a",
            "movies/Inception (2010)/Inception.2010.1080p.mkv",
        )];
        let chosen =
            match_subtitle_to_video("movies/Inception (2010)/Subs/2_English.srt", &candidates)
                .unwrap();
        assert_eq!(chosen.entry_key, "a");
    }

    #[test]
    fn plain_sidecar_matches_the_video_with_the_same_stem_among_many() {
        let candidates = vec![
            movie("a", "movies/Inception.2010.1080p.mkv"),
            movie("b", "movies/Interstellar.2014.1080p.mkv"),
        ];
        let chosen =
            match_subtitle_to_video("movies/Inception.2010.1080p.en.srt", &candidates).unwrap();
        assert_eq!(chosen.entry_key, "a");
    }

    #[test]
    fn episode_marker_pins_the_subtitle_to_one_episode() {
        let candidates = vec![
            episode("a", "tv/The Expanse/Season 2/The.Expanse.S02E05.mkv", 2, 5),
            episode("b", "tv/The Expanse/Season 2/The.Expanse.S02E06.mkv", 2, 6),
        ];
        let chosen = match_subtitle_to_video(
            "tv/The Expanse/Season 2/Subs/The.Expanse.S02E06.en.srt",
            &candidates,
        )
        .unwrap();
        assert_eq!(chosen.entry_key, "b");
    }

    #[test]
    fn ambiguous_match_returns_none_rather_than_guessing() {
        let candidates = vec![
            movie("a", "movies/Inception.2010.1080p.mkv"),
            movie("b", "movies/Interstellar.2014.1080p.mkv"),
        ];
        assert!(match_subtitle_to_video("movies/random-notes.en.srt", &candidates).is_none());
    }

    #[test]
    fn srt_is_converted_to_webvtt() {
        let out = srt_to_webvtt("1\r\n00:00:01,250 --> 00:00:03,000\r\nHello\r\n");
        assert!(out.starts_with("WEBVTT\n\n"));
        assert!(out.contains("00:00:01.250 --> 00:00:03.000"));
        assert!(out.contains("Hello"));
    }

    #[test]
    fn already_webvtt_input_is_left_alone() {
        let out = srt_to_webvtt("WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHi\n");
        assert_eq!(out, "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHi\n");
    }
}
