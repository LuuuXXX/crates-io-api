# crates_io_api — Sparse Index Implementation

A drop-in replacement for the `crates_io_api` crate that fetches metadata
from the **crates.io sparse registry index** (`https://index.crates.io/`)
instead of the crates.io JSON web API.

---

## Background

The [crates.io sparse registry index](https://doc.rust-lang.org/cargo/reference/registry-index.html)
is the same data source Cargo uses internally to resolve dependencies.
It stores per-crate metadata in **newline-delimited JSON (ndjson)** files —
one JSON object per published version — served as plain HTTP over a CDN.

This implementation queries those files directly, which means:

- **No crates.io web API dependency** (avoids rate-limits, authentication, etc.)
- **CDN-backed**, highly available, and cacheable
- The same source of truth Cargo itself uses for version and feature resolution

---

## Architecture

```
src/
  lib.rs          — public re-exports (identical to base/; drop-in compatible)
  error.rs        — Error enum (identical to base/)
  types.rs        — public data types (identical to base/)
  index.rs        — sparse index URL computation, ndjson parser, type converters
  sync_client.rs  — SyncClient (blocking) backed by the sparse index
  async_client.rs — AsyncClient (async) backed by the sparse index
```

---

## Implementation approach

### 1. Sparse index URL computation

The index uses a deterministic path layout based on crate name length:

| Name length | URL path                    |
|-------------|------------------------------|
| 1           | `1/{name}`                  |
| 2           | `2/{name}`                  |
| 3           | `3/{first_char}/{name}`     |
| ≥ 4         | `{first2}/{next2}/{name}`   |

The name is **lowercased** before computing the path (matching Cargo's
behavior).  For example, `serde` → `https://index.crates.io/se/rd/serde`.

### 2. Parsing the ndjson response

Each file is an append-only log with one JSON object per line:

```json
{"name":"serde","vers":"1.0.0","deps":[...],"features":{},"yanked":false}
{"name":"serde","vers":"1.0.1","deps":[...],"features":{},"yanked":false}
...
```

The parser reads line-by-line and skips lines that are empty or that fail
to deserialise, making it **forward-compatible** with future index format
extensions.

### 3. Type conversion

Sparse index entries contain a subset of the data available through the
web API.  The conversion fills unavailable fields with safe defaults:

**`CrateResponse` / `Crate`**

| Target field        | Source                                                  |
|---------------------|---------------------------------------------------------|
| `id` / `name`       | `IndexEntry::name` (first entry, preserves casing)     |
| `max_version`       | Highest non-yanked semver across all entries            |
| `max_stable_version`| Highest non-yanked stable (no pre-release) semver       |
| `versions` (ids)    | Sequential 1-based position in the ndjson file          |
| `description` / `license` / `downloads` | Not in index → `None` / `0`    |
| `created_at` / `updated_at` | Not in index → current UTC time               |
| `links`             | Synthesised from crate name                             |
| `exact_match`       | Always `true` (queried by exact name)                  |

**`Version`**

| Target field        | Source                                                  |
|---------------------|---------------------------------------------------------|
| `num`               | `IndexEntry::vers`                                      |
| `yanked`            | `IndexEntry::yanked`                                    |
| `features`          | `IndexEntry::features` merged with `features2` (v2)    |
| `dl_path`           | Synthesised standard crates.io download path            |
| `id`                | 1-based position in ndjson                              |
| `license` / `downloads` / timestamps | Not in index → `None` / `0` / epoch  |

**`Dependency`**

| Target field        | Source                                                  |
|---------------------|---------------------------------------------------------|
| `crate_id`          | `IndexDep::package` if set (renamed dep), else `name`  |
| `req` / `features` / `optional` / `default_features` / `target` / `kind` | `IndexDep` fields |
| `downloads` / `id` / `version_id` | Not in index → `0`                      |

> **Renamed dependencies**: when a dependency is declared as
> `dep_alias = { package = "real_crate" }`, the sparse index sets
> `name` to the alias and `package` to the real crate name.  The
> converter always uses `package` (the real name) as `crate_id`.

> **Format v2 features**: entries may carry a `features2` field with
> extended feature data; these are merged into `features` transparently.

### 4. `max_version` / `max_stable_version` selection

- Non-yanked entries are preferred; if **all** entries are yanked, the
  fallback considers yanked entries.
- `max_version` is the highest semver overall (including pre-releases).
- `max_stable_version` is the highest semver with an empty pre-release
  segment (i.e., no `-alpha`, `-beta`, etc.).

### 5. Rate limiting

Both `SyncClient` and `AsyncClient` honour the `rate_limit` duration passed
to `::new()`, enforcing at most one HTTP request per interval — identical
behaviour to the original crate.

---

## Method availability

| Method                              | Status                                 |
|-------------------------------------|----------------------------------------|
| `SyncClient::new` / `AsyncClient::new` | ✅ Full                             |
| `get_crate`                         | ✅ Full — core use-case               |
| `crate_dependencies`                | ✅ Full                               |
| `full_crate`                        | ✅ Partial (no downloads/owners/rev-deps) |
| `crate_downloads`                   | ⚠️ Returns empty (not in sparse index) |
| `crate_owners`                      | ⚠️ Returns empty (not in sparse index) |
| `crate_authors`                     | ⚠️ Returns empty (not in sparse index) |
| `crate_reverse_dependencies`        | ⚠️ Returns empty (not in sparse index) |
| `summary` / `crates` / `user`       | ❌ Returns `Error::Api`               |

---

## Quick start

```rust
use crates_io_api::{SyncClient, Error};
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = SyncClient::new(
        "my-bot (contact@example.com)",
        Duration::from_millis(1000),
    )?;

    // Check whether a crate exists and find its latest version.
    match client.get_crate("serde") {
        Ok(resp) => {
            println!("name:        {}", resp.crate_data.name);
            println!("max_version: {}", resp.crate_data.max_version);
            println!("versions:    {}", resp.versions.len());
        }
        Err(Error::NotFound(_)) => println!("crate not published"),
        Err(e) => return Err(e.into()),
    }

    // Fetch dependencies of a specific version.
    let deps = client.crate_dependencies("serde", "1.0.0")?;
    for dep in &deps {
        println!("  dep: {} {}", dep.crate_id, dep.req);
    }

    Ok(())
}
```

---

## Running the tests

```bash
# Unit tests (no network required)
cargo test

# Integration tests (require network access to index.crates.io)
cargo test -- --ignored
```

---

## Comparison with `base/`

| Aspect              | `base/` (web API)          | `src/` (sparse index)        |
|---------------------|----------------------------|------------------------------|
| Data source         | `https://crates.io/api/v1/`| `https://index.crates.io/`   |
| Protocol            | REST JSON API              | HTTP + ndjson files           |
| Crate metadata      | Full (desc, license, …)    | Partial (versions, deps, yanked) |
| Download stats      | ✅                         | ❌ (not in index)             |
| Owner / author info | ✅                         | ❌ (not in index)             |
| Reverse deps        | ✅                         | ❌ (not in index)             |
| Public API          | Same                       | Same (drop-in compatible)     |
