# AGENTS.md

## Stack

- **macOS only** — tray-only app, no Dock icon (`skipTaskbar: true`, `macOSPrivateApi: true`)
- Tauri 2 · React 19 · TypeScript · Vite · Zustand
- Rust entry point: `src-tauri/src/main.rs` → all logic in `src-tauri/src/lib.rs`
- Frontend entry: `src/main.tsx` → `src/App.tsx` (UI) + `src/store.ts` (state) + `src/types.ts` (shared types)

## Dev Commands

```bash
npm run tauri:dev      # full dev (starts Vite + Tauri, hot-reload)
npm run build          # TypeScript check + Vite build only (no Tauri)
npm run tauri:build    # release .app bundle

cd src-tauri && cargo test   # Rust unit tests
```

## Architecture

```
Frontend (React/TS)          Backend (Rust)
──────────────────           ──────────────────────────
src/App.tsx                  src-tauri/src/lib.rs
  └─ useAppStore (Zustand)     ├─ AppState { projects: Mutex<Vec<Project>>, runtime: Mutex<RuntimeState> }
       └─ invoke(command)  →   ├─ UiState { auto_hide_suspended: Mutex<bool> }
                               ├─ Tauri commands (see table below)
                               └─ Persistence: projects.json, runtime.json, discovery.json (app data dir)
```

- All frontend state is in `useAppStore` — do not add local component state for server data.
- Types in `src/types.ts` must stay in sync with Rust structs in `lib.rs`.
- The frontend polls `refresh_status` and `discover_listeners` every **5 s** (`REFRESH_MS = 5000`) and also listens for the `refresh-requested` Tauri event emitted from Rust. Refresh cycles are coalesced to avoid overlapping scans.
- Listener discovery and status inspection run on blocking worker threads, not the UI thread. Detected listeners are transient; only an explicit save adds a project to persistence.

## Tauri Commands

All called via `invoke(name, args)` from the frontend.

| Command | Args | Returns |
|---|---|---|
| `list_projects` | — | `Project[]` |
| `add_project` | `{ input: AddProjectInput }` | `Project` |
| `update_project` | `{ input: UpdateProjectInput }` | `Project` |
| `remove_project` | `{ projectId: string }` | `void` |
| `refresh_status` | — | `ProjectStatus[]` |
| `discover_listeners` | — | `ListenerDiscoveryResult` |
| `get_discovery_settings` | — | `DiscoverySettings` |
| `set_discovery_folders` | `{ folders: string[] }` | `DiscoverySettings` |
| `open_project_url` | `{ projectId: string }` | `void` |
| `kill_project_port` | `{ projectId: string }` | `KillResult` |
| `get_settings` | — | `Settings` |
| `set_autostart` | `{ enabled: boolean }` | `Settings` |
| `detect_project_ports` | `{ path: string }` | `PortDetectionResult` |
| `hide_main_window` | — | `void` |
| `set_auto_hide_suspended` | `{ suspended: boolean }` | `void` |
| `quit_app` | — | `void` |

## Serialization

- Rust structs serialize to **camelCase** (`#[serde(rename_all = "camelCase")]`)
- Exception: `PortSource` enum uses **kebab-case** (`env`, `package-script`, `esbuild-config`, `vite-config`, `docker-compose`)

## Persistence

Three JSON files in the Tauri app data directory, managed entirely by Rust:

- `projects.json` — `Vec<Project>` (loaded at startup, written on every mutation)
- `runtime.json` — `RuntimeState { last_running_by_project: HashMap<String, ISO8601> }` (tracks last-seen-running timestamps)
- `discovery.json` — `DiscoverySettings { folders: string[] }`; empty by default. Writes are atomic. Discovery includes only listeners whose resolved working directory and identified project path are inside a selected folder. Saved project status remains independent of this filter.

The frontend never reads or writes these files directly.

## Capabilities

Defined in `src-tauri/capabilities/default.json`. Currently: `core:default` + `dialog:default`.

Adding a new Tauri plugin requires:
1. Add the plugin crate to `src-tauri/Cargo.toml`
2. Register it in `lib.rs` (`.plugin(...)` on the builder)
3. Add its permission to `capabilities/default.json`

## macOS Notes

- Process kill sequence: **SIGTERM** → wait up to 2 s → **SIGKILL** → wait up to 1 s
- Browser open uses the macOS `open` CLI: `Command::new("open").arg(url)`
- The tray popover uses logical points for both positioning and sizing. Monitor selection must stay tied to the tray anchor, with the pointer resolving mixed-DPI ambiguity; `POPOVER_SAFE_MARGIN` (24 logical points) keeps it inside the work area.
- `src-tauri/vendor/tray-icon` backports the upstream macOS 27 click fix. See its `PATCH.md` before updating Tauri/tray dependencies.
- `updater` plugin requires a real public key in `tauri.conf.json` before shipping (`REPLACE_WITH_TAURI_UPDATER_PUBLIC_KEY`)
