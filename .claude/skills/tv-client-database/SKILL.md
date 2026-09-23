---
name: tv-client-database
description: Use when adding, changing, or reasoning about anything stored on-device by the Fire TV client (clients/tv-android/app) — new tables/columns, schema migrations, or deciding whether new state belongs in Room at all. Covers the Room-as-SQLite-with-relational-schema decision, the yoyo-style versioned-migration convention, the singleton-row pattern used for one-row "current state" tables, and which kinds of on-device data deliberately stay OUT of this database (secrets, flat per-item KV state).
---

# TV client on-device database conventions

Established when the client gained real on-device persistence for the
first time (previously: `AndroidTokenStore`, encrypted secrets only, and
a single SharedPreferences JSON blob for swarm membership — nothing else
survived an app restart). Read this before adding any new on-device
state to `clients/tv-android/app`.

## The stack: Room, not raw SQLite, not SQLDelight

**Room** (`androidx.room`, already in `gradle/libs.versions.toml` before
this need arose — see the dependency comment history in
`app/build.gradle.kts`) is this project's answer to "SQLite with a real
relational schema (PKs/FKs) and a migration tool," the Kotlin/Android
equivalent of what a request phrased in Python terms would call "yoyo
for python":

- `@Entity`/`@PrimaryKey`/`@ForeignKey`/`@Index` annotations *are* the
  schema — Kotlin data classes in `app/src/main/kotlin/app/swarm/tv/app/data/db/Entities.kt`,
  not hand-written `CREATE TABLE` strings.
- `@Dao` interfaces (`Daos.kt`) are the query layer — every query is a
  real, checked-at-compile-time `@Query("...")` SQL string; a typo or a
  reference to a nonexistent column fails the KSP build, not a runtime
  crash.
- `androidx.room.migration.Migration` objects (`Migrations.kt`) are the
  literal migration-script equivalent of a yoyo migration file: one
  object per version bump, each wrapping raw `db.execSQL(...)` SQL,
  applied in order, and — this is the part that matters most — **never
  edited once shipped**, exactly like a yoyo file that's already run
  against a real install. A wrong step gets fixed by adding a new, later
  migration, not by rewriting history.
- Why not SQLDelight's `.sqm` files (the more *literal* yoyo analogue —
  numbered raw-SQL files, no Kotlin in the loop at all): decided against
  for the extra Gradle plugin/build-tooling surface it needs on top of
  what Room already gets from the KSP setup this app now has anyway; not
  revisited unless Room's migration model becomes a real limitation.

**The migration convention, step by step** (also documented at the top
of `Migrations.kt` — keep both in sync if either changes):
1. Bump `AppDatabase`'s `@Database(version = ...)` by exactly 1.
2. Add one `Migration(oldVersion, newVersion) { db -> db.execSQL("...") }`
   entry to `MIGRATIONS`.
3. Update the matching `@Entity`/`@Dao` Kotlin models in the *same*
   change, so the compiled schema matches what the migration produces.
4. `ksp { arg("room.schemaLocation", "$projectDir/schemas") }`
   (`app/build.gradle.kts`) writes a JSON snapshot of the schema at
   every version on every build — a diffable history, and what a future
   `MigrationTestHelper`-based test would check new migrations against
   if one gets added. A build where the entities and a migration
   disagree about the resulting schema fails at `kspDebugKotlin` time,
   not silently at runtime on a real device.
5. Never edit a shipped `Migration`. Add a new one instead.

No `Migration` exists yet as of the schema's introduction — version 1 is
Room deriving `CREATE TABLE` directly from the `@Entity` annotations for
a fresh install, so there's nothing to migrate *from* yet. The list
stays empty until version 1 needs to change under installs that already
exist.

## The singleton-row pattern

Two of the three tables today (`server_connection`, `app_settings`) hold
exactly one row, `id` always `SINGLETON_ROW_ID` (`= 1L`), written via
`@Insert(onConflict = OnConflictStrategy.REPLACE)`. This models "this
device's current X" — one STUN connection, one set of app settings —
without a nullable-everything sentinel row or a separate "is this
configured yet" flag; presence/absence of the row *is* the signal
(`dao.get() == null` means "nothing saved yet"). `swarm` is the
one-to-many side, FK'd to `server_connection.id` with `ON DELETE
CASCADE` — forgetting the saved connection correctly forgets every
swarm row with it in one statement, no separate cleanup query needed.

**Enforcing "at most one of these" (e.g. `swarm.is_active`)**: do it in
a DAO query that sweeps every row in one statement
(`SwarmDao.setActive`), not a DB constraint. SQLite's partial unique
indexes (`CREATE UNIQUE INDEX ... WHERE is_active = 1`) aren't
expressible through Room's `@Index` annotation, and hand-writing one
outside Room's own schema tracking desyncs its migration-checksum
validation — confirmed as a real footgun before it ever shipped, not
merely theoretical. Watch for SQL three-valued-logic bugs in these
sweep queries too: `id = :maybeNullParam` evaluates to `NULL`, not
`false`, when the parameter is null, which then tries to write `NULL`
into a `NOT NULL` column and fails the whole statement — use an explicit
`CASE WHEN :param IS NOT NULL AND ... THEN 1 ELSE 0 END` instead of a
bare comparison whenever the comparison value can be null.

## What deliberately stays OUT of this database

- **Secrets** (`AndroidTokenStore`, the STUN access token) — stays in
  `androidx.security.crypto` `EncryptedSharedPreferences`
  (Keystore-backed AES256-GCM), never moves into the plain SQLite file
  here. A rooted device's file explorer can read `swarm.db` directly; it
  cannot read past the Keystore-backed encryption without device
  compromise well beyond that. This isn't a soft preference — don't add
  a `token`/`password`/`secret` column to any table in this schema.
- **Flat per-item resume/watched state** (`AndroidWatchStateStore`,
  keyed by cross-server content fingerprint, with show/season/episode
  snapshotted for logical-episode recovery after a replacement encode) —
  deliberately *not* Room even though Room is now set up and would work fine for it. It's a
  genuinely flat key→value shape with no relations to model (no entity
  it needs a foreign key toward), so a plain SharedPreferences-backed
  store stays simpler with nothing lost. Writes are timestamp-ordered:
  heartbeat, lifecycle, and disposal callbacks can overlap, so the store
  must not let an older callback overwrite a newer position. Don't migrate
  it to Room "for consistency" without a real reason — see that store's own doc comment.

## WAL journal mode

`AppDatabase` is built with `.setJournalMode(JournalMode.WRITE_AHEAD_LOGGING)`.
Real concurrent access happens here: DAOs get read from Compose
recomposition/`Flow` collection and written from `ViewModel` coroutines
at the same time (e.g. the settings screen observing `server_connection`
while a join/leave call updates `swarm` rows). WAL lets reads and writes
overlap instead of serializing behind a single connection-wide lock —
don't drop back to the default `TRUNCATE` mode without a specific reason.

## Testing posture (why there's no migration test yet)

`:app` has no test source set, no JUnit, no Robolectric/instrumented
harness at all as of this schema's introduction — confirmed by checking
for one before reaching for `androidx.room:room-testing`'s
`MigrationTestHelper`. That API needs a real Android runtime
(instrumented `androidTest`) or Robolectric's shadow SQLite to actually
exercise `Room.inMemoryDatabaseBuilder`/a real migration path; a plain
JVM unit test can't, at this Room version (2.6.1 — the KMP-portable
bundled SQLite driver that would remove that requirement landed later).
Setting up either harness from scratch is real new infrastructure, out
of scope for adding a schema. Correctness signal today is: (1) KSP/Room
annotation processing at compile time — `kspDebugKotlin` fails on a
bad `@Entity`/`@Dao`/FK reference before it ever reaches a device — and
(2) real on-device verification: pull the actual `.db` file
(`adb shell run-as app.swarm.tv cat /data/data/app.swarm.tv/databases/swarm.db`,
same for `-wal`) and inspect it with a local `sqlite3 file.db ".schema"`
— there's no `sqlite3` binary on the device itself to run this in
place. Revisit real migration tests once there's an actual version-2
migration to test and the `:app` test harness gets built for some other
reason too.
