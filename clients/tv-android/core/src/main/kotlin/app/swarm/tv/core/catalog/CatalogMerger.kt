/**
 * Merges the catalog manifests of every server in a swarm into one list:
 * the same [app.swarm.tv.core.peer.CatalogEntry.fingerprint] on two servers
 * collapses into a single [MergedEntry] with multiple sources, per
 * `docs/PROTOCOL.md`'s catalog-identity section. STUN never sees any of
 * this — manifests are fetched peer-to-peer, so merging happens entirely
 * client-side.
 */
package app.swarm.tv.core.catalog

import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.CatalogManifest
import app.swarm.tv.core.peer.MediaKind

fun CatalogEntry.displayTitle(): String = when (kind) {
    MediaKind.EPISODE -> episodeTitle?.takeIf { it.isNotBlank() } ?: title
    else -> scrapedTitle?.takeIf { it.isNotBlank() } ?: title
}

data class MergedEntry(
    val fingerprint: String,
    /** Server ids holding this fingerprint, sorted for determinism. */
    val sources: List<String>,
    /** The richest [CatalogEntry] seen for this fingerprint — see [isRicher]. */
    val entry: CatalogEntry,
)

object CatalogMerger {
    /** `serverId -> that server's current manifest`. */
    fun merge(manifestsByServer: Map<String, CatalogManifest>): List<MergedEntry> {
        val sourcesByFingerprint = linkedMapOf<String, MutableList<String>>()
        val bestEntryByFingerprint = linkedMapOf<String, CatalogEntry>()
        val bestServerByFingerprint = linkedMapOf<String, String>()
        val fingerprintByServerEntryKey = mutableMapOf<Pair<String, String>, String>()

        for (serverId in manifestsByServer.keys.sorted()) {
            val manifest = manifestsByServer.getValue(serverId)
            for (entry in manifest.entries) {
                fingerprintByServerEntryKey[serverId to entry.entryKey] = entry.fingerprint
                sourcesByFingerprint.getOrPut(entry.fingerprint) { mutableListOf() }.add(serverId)
                val existing = bestEntryByFingerprint[entry.fingerprint]
                if (existing == null || isRicher(entry, existing)) {
                    bestEntryByFingerprint[entry.fingerprint] = entry
                    bestServerByFingerprint[entry.fingerprint] = serverId
                }
            }
        }

        return bestEntryByFingerprint.entries
            .map { (fingerprint, entry) ->
                val selectedServer = bestServerByFingerprint.getValue(fingerprint)
                val parentFingerprint = entry.parentEntryKey?.let {
                    fingerprintByServerEntryKey[selectedServer to it]
                }
                val canonicalParentKey = parentFingerprint?.let { bestEntryByFingerprint[it]?.entryKey }
                MergedEntry(
                    fingerprint,
                    sourcesByFingerprint.getValue(fingerprint).toList(),
                    if (canonicalParentKey != null) entry.copy(parentEntryKey = canonicalParentKey) else entry,
                )
            }
            .sortedWith(
                compareBy<MergedEntry>(
                    { it.entry.catalogSortTitle() },
                    { it.entry.displayTitle().lowercase() },
                ),
            )
    }

    /**
     * A scraped title or downloaded artwork makes one server's copy of an
     * entry more useful to display than another's — deterministic tie-break
     * (sorted server-id iteration above) when neither has an edge, so the
     * merge is reproducible given the same inputs regardless of map order.
     */
    private fun isRicher(candidate: CatalogEntry, existing: CatalogEntry): Boolean {
        fun score(e: CatalogEntry) =
            (if (e.scrapedTitle != null) 1 else 0) +
                (if (e.episodeTitle != null) 1 else 0) +
                (if (e.artworkEtag != null) 1 else 0) +
                (if (e.skipSegments.isNotEmpty()) 1 else 0)
        return score(candidate) > score(existing)
    }

    /**
     * Movies are alphabetized like a media library: a leading standalone
     * "The" does not separate a title from the rest of its series. The
     * displayed title is untouched; this value is used only for ordering.
     */
    private fun CatalogEntry.catalogSortTitle(): String {
        val title = displayTitle().trim()
        val withoutLeadingArticle = if (
            kind == MediaKind.MOVIE && title.startsWith("The ", ignoreCase = true)
        ) {
            title.substring(4).trimStart()
        } else {
            title
        }
        return withoutLeadingArticle.lowercase()
    }

}
