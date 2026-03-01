# Server Ports

Menu bar macOS utility built with Tauri + React to monitor local dev servers by configured port.

## Features

- Add projects with name, folder path, and port.
- Auto-refresh status every 5 seconds.
- Show git branch, running PID, and last seen running time.
- Open `http://localhost:<port>` directly.
- Kill the listener process on that port with `SIGTERM` then `SIGKILL` fallback.
- Start-at-login toggle via macOS LaunchAgent (`tauri-plugin-autostart`).
- Launch hidden when started by autostart (`--minimized`).
- Tray controls: show window, refresh, quit.
- Updater plugin wired with signed updater artifact generation.
- In-app `Check Updates` action (download + install flow).

## Prerequisites

- macOS
- Node.js 20+
- Rust toolchain (`cargo`, `rustc`)
- Xcode Command Line Tools (`xcode-select --install`)
- For public distribution: full Xcode + Apple Developer account (signing/notarization)

## Development

```bash
npm install
npm run tauri:dev
```

The frontend runs on `http://localhost:5173` and Tauri launches against that dev URL.

## Build

```bash
npm run tauri:build
```

Artifacts are emitted under `src-tauri/target/release/bundle`.

## Data Storage

Project and runtime state are stored in:

- `~/Library/Application Support/com.guille.serverports/projects.json`
- `~/Library/Application Support/com.guille.serverports/runtime.json`

## Backend Command API

- `list_projects() -> Project[]`
- `add_project(input: AddProjectInput) -> Project`
- `remove_project(project_id: string) -> void`
- `refresh_status() -> ProjectStatus[]`
- `open_project_url(project_id: string) -> void`
- `kill_project_port(project_id: string) -> KillResult`
- `get_settings() -> Settings`
- `set_autostart(enabled: bool) -> Settings`
- `quit_app() -> void`

## Updater Setup

Before shipping auto-updates, replace placeholder updater values in `src-tauri/tauri.conf.json`:

- `plugins.updater.pubkey`: your Tauri updater public key
- `plugins.updater.endpoints`: your hosted update JSON URL(s)

## Testing

Frontend:

```bash
npm run typecheck
npm run build
```

Rust (from `src-tauri`):

```bash
cargo fmt --all
cargo test
```
