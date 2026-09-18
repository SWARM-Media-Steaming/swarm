//! Metadata scraping: TMDb for movies/TV (user-supplied free key), keyless
//! MusicBrainz + Cover Art Archive + Wikimedia Commons for music. Same
//! source stack as Batocera.Drone, chosen there (and here) for being
//! license-clean and signup-free beyond the one TMDb key.

pub mod artwork;
pub mod coverart;
pub mod introdb;
pub mod lrclib;
pub mod musicbrainz;
pub mod runner;
pub mod tmdb;
pub mod wikimedia;

pub use runner::{
    run_bulk_scrape, scrape_one_album, scrape_one_track, scrape_one_video, BulkScrapeReport,
    ScrapeConfig, ScrapeIssue, ScrapeOneError, ScrapeOutcome, ScrapeProgress, ScrapeProgressEvent,
};
pub use tmdb::{TmdbOverride, TmdbOverrideError};
