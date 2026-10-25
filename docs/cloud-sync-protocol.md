# GOG cloud save protocol (reverse-engineered notes)

Sources: Heroic `gogdl/saves.py`, live probes of the GOG endpoints, and comet's README.

## Endpoints

| Purpose | Endpoint |
|---|---|
| Per-game save locations | `GET https://remote-config.gog.com/components/galaxy_client/clients/{clientId}?component_version=2.0.45`. Read `content.Windows.cloudStorage.locations[]` (`name`, `location` with `<?INSTALL?>`, `<?SAVED_GAMES?>`, `<?APPLICATION_DATA_LOCAL?>` and similar placeholders). Also `.enabled`, `.quota`. |
| Per-game clientId / clientSecret | Build manifest from `content-system.gog.com/products/{id}/os/{os}/builds?generation=2` (zlib JSON: `clientId`, `clientSecret`) |
| Game token | `auth.gog.com/token` with `grant_type=refresh_token`, using the game's `client_id` / `client_secret` and the user's refresh token |
| List | `GET cloudstorage.gog.com/v1/{userId}/{clientId}` → `[{name, hash, last_modified}]` (404 = empty) |
| Download | `GET .../{clientId}/{locationName}/{relpath}` (header `X-Object-Meta-LocalLastModified`) |
| Upload | `PUT` same URL, body gzip(level 6, mtime=0), headers `Etag: md5(gzipped)`, `Content-Encoding: gzip`, `X-Object-Meta-LocalLastModified: <UTC ISO8601 seconds>` |
| Delete | `DELETE` same URL |
| Commit | `POST .../v1/{userId}/{clientId}` (finalise the session) |

Required headers: `Authorization: Bearer <game token>` and a `GOGGalaxyCommunicationService/2.0.13.27 ... dont_sync_marker/true` User-Agent.
The empty-file marker is md5 `aadd86936a80ee8a369579c3926f1b3c`, which means "deleted".

## Why Heroic loses progress

`SyncClassifier` compares each file's mtime to ONE global "last sync" timestamp, then applies a single directory-wide action:
- Files changed on both sides, or on neither → ambiguous. It falls to CONFLICT, which does *nothing*.
- Deletions aren't tracked. A file missing locally is just "not existing locally" and gets re-downloaded or removed.
- It uses only the file's own `last_modified`, never a per-file hash against a last-synced base.
- The sync timestamp is held per game by the caller, so a failed or aborted run advances it anyway.
- A single `dirname` is hard-coded, and the remote-config `locations[]` list isn't used for multi-location games. Under Wine the Windows path placeholders must be mapped into the prefix.

## Our approach

1. Resolve ALL `locations[]` (each is its own cloud dir) into the Wine prefix paths.
2. Per file, keep a persisted base snapshot `{relpath → (md5-of-gzip, mtime)}` written only after a successful commit.
3. Run the three-way plan per file (`src/sync.rs::plan`) instead of one action per directory.
4. Compare content hashes (gzip md5 as GOG does), using mtime only for conflict display.
5. On conflict, show a dialog like the official client (keep local or cloud, with timestamps).
6. Sync before launch (download) and after exit (upload), then POST commit.
