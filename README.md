# McOsu Importer (Rust + Slint)

A lightweight desktop app for Windows (also compatible with other platforms) that watches your Downloads folder and imports osu! beatmaps (`.osz`) into McOsu’s `Songs` folder. It shows beatmap metadata, thumbnails, audio preview, and an external beatmap viewer.

## Highlights

- Watches a Downloads folder for new `.osz` files and queues them automatically.
- Waits for **download stability** (size + mtime) before parsing/importing.
- Extracts `.osu` metadata and background to generate a thumbnail.
- Manual and bulk actions: **Import now**, per-card Import/Reimport/Ignore, clear completed, show/hide completed.
- Duplicate detection (BeatmapSetID preferred; fallback to `.osz` hash).
- Optional source cleanup: delete the original `.osz` from Downloads after successful import (with safety checks).
- Audio preview (one-at-a-time) with cache.
- Beatmap visual preview via a locally served viewer (`127.0.0.1`) opened in a new browser window.

## Requirements

- Stable Rust toolchain (tested with Rust 1.80+).
- Windows 10/11 recommended (Slint/notify also work on Linux/macOS depending on backend support).

## Build and Run

- Dev:
  ```bash
  cargo run
  ```

- Release:
  ```bash
  cargo build --release
  ```
  - Binary:
    - Windows: `target/release/mcosu-importer.exe`
    - Linux/macOS: `target/release/mcosu-importer`
  - Run release:
    - PowerShell: `\target\release\mcosu-importer.exe`
    - bash/zsh: `./target/release/mcosu-importer`

- Release profile is optimized for smaller binaries:
  - `lto = "thin"`, `codegen-units = 1`, `opt-level = "z"`, `panic = "abort"`

- Packaging:
  - Windows:
    ```bash
    pwsh scripts/package.ps1
    ```
  - Linux/macOS:
    ```bash
    sh scripts/package.sh
    ```
  - Output goes to `dist/` (binary + README/CHANGELOG/LICENSE + assets).

## Usage

1) Launch the app.

2) Top bar (two rows):
- **Row 1:** Downloads and McOsu `Songs` folders (read-only fields + **Choose** buttons). Path safety warnings show right below.
- **Row 2:** Actions (**Import now**, **Add .osz**, **Clear completed**) and toggles (**Auto-import**, **Auto-delete source**, **Show completed**).

3) Pipeline:
- When a `.osz` is detected, the app waits until it stabilizes.
- It parses `.osu` files, reads metadata, finds the background image, and generates a thumbnail.
- Import behavior:
  - Auto-import only applies to items that entered the queue **after** auto-import is enabled.
  - Otherwise use **Import now** or the per-card Import/Reimport buttons.

### Per-card actions

- Import / Reimport / Ignore
- Open source (file) / Open destination / Open in browser (uses BeatmapSetID when available)
- **Audio preview** (single active preview at a time; cached)
- **Beatmap preview** (opens the local viewer in a new browser window)
- **Delete source (.osz)** after completed import:
  - Deletes only the original `.osz` in the configured Downloads folder
  - Confirmation prompt + Recycle Bin when possible
  - Disabled when the file is outside Downloads or when Songs overlaps Downloads (safety)

Error messages appear summarized in an `Error:` row with a **Details** dialog for full text (zip extraction, Songs write errors, metadata parsing, or source deletion failures).

### Add `.osz` manually

Use **Add .osz** to enqueue a file from anywhere. (Source deletion stays disabled if the file isn’t inside the configured Downloads folder.)

## Beatmaps tab (search workflow)

The app can help you discover beatmaps by searching and opening results in your browser. After you download a `.osz` via the browser, the watcher will detect it in your Downloads folder and enqueue it automatically.

> Note: This avoids embedding downloading logic directly into the app; the app focuses on local import and preview.

## Audio and Beatmap Preview

### Audio preview
- Only one preview plays at a time (starting a new preview pauses the previous).
- Uses audio from the `.osz` or from the imported destination.
- Cache location: `%LOCALAPPDATA%/mcosu-importer/cache/audio/`

### Beatmap preview
- On **Preview beatmap**, the app prepares a temporary preview directory:
  - `%LOCALAPPDATA%/mcosu-importer/cache/preview/<hash>/`
- Serves a vendored static viewer from `assets/viewer/` over a local HTTP server on `127.0.0.1`.
- Opens a new browser window; on Windows it tries Edge/Chrome `--app=<URL>` when available, otherwise falls back to the default browser.
- Logs include the chosen port, cache path, and URL for debugging.

## Where is McOsu’s Songs folder?

Steam → Library → right-click **McOsu** → Manage → Browse local files → open the `Songs` folder.

## Download Stability Detection

Configurable in `config.json`:
- `consecutive_checks` (default: 3)
- `interval_ms` (default: 700ms)
- `timeout_secs` (default: 120s)

A file is considered stable after N consecutive checks with no size/mtime changes. If it exceeds the timeout, it fails with a clear status/error.

## Path Safety

- The app prevents selecting `Songs` inside (or equal to) the Downloads folder.
- Dangerous configs (Downloads and Songs overlapping) show a persistent warning and disable:
  - **Import now**
  - auto-import
  - source deletion (manual/automatic)
until the paths are fixed.

## Duplicates

- Primary key: BeatmapSetID
- Fallback: `.osz` hash
- Index stored in: `cache/cache.json`
- Duplicate state offers:
  - Open destination
  - Reimport (overwrite)
  - Ignore

## Source Deletion (.osz)

- Only removes the original `.osz` from the configured Downloads folder.
- Uses Recycle Bin when possible; falls back to `remove_file` if needed.
- Deletion failures do not mark the import as failed, but show a warning + details.

Auto-delete can be enabled globally with **Auto-delete source after import** (first-time confirmation with “Don’t ask again”).

## Data, Cache, and Logs

- Data directory:
  - Windows: `%LOCALAPPDATA%/mcosu-importer`
  - Other OS: XDG/Library equivalents via `directories`
- `config.json`: paths, toggles, stability params
- `cache/cache.json`: thumbnails, audio cache index, duplicate index
- Thumbnails: `cache/thumbnails/`
- Audio cache: `cache/audio/<hash>/`
- Preview cache: `cache/preview/<hash>/`
- Logs: `logs/app.log`  
  Use **Copy logs** to copy the current log panel content to clipboard.

Cleanup: close the app and remove the `mcosu-importer` data folder. On first run, the app migrates legacy `config.json/cache.json` from the working directory if present.

## Tests

```bash
cargo test
```

## Troubleshooting

- Stuck on “Waiting”: the download may still be writing; tweak `consecutive_checks` / `interval_ms`.
- “File did not stabilize”: the stability timeout expired; check permissions or download speed.
- No thumbnail: the `.osz` has no supported background reference.
- “Duplicate”: BeatmapSetID or hash is already indexed; use **Reimport** to overwrite.
- Destination looks empty: verify McOsu `Songs` path and write permissions.
- Logs missing: check `logs/app.log` under the data directory.
- UI won’t open on Linux/macOS: verify Slint backend dependencies for your system.
- Release build fails: run `cargo clean` and update the toolchain.

## Security / Privacy

- No login, cookies, or browser automation.
- The app primarily processes local files.
- Optional external actions include opening a beatmap page or preview window in your browser.
- Beatmap viewer runs locally on `127.0.0.1` and serves only cached/local assets.

## Known Issues / Limitations

- No star rating / pp calculation.
- Drag-and-drop depends on backend support; use **Add .osz** if needed.
- Duplicate detection uses BeatmapSetID/hash (does not compare file-by-file content).
- Thumbnails require a valid background reference inside the `.osz`.
- Beatmap preview depends on an available browser; “app window” mode only works if Edge/Chrome are available.
