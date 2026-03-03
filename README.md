# Port Scout

Port Scout is a lightweight menu bar app for macOS that helps you keep track of all your local development servers in one place. Instead of remembering which project runs on which port, you can quickly see each project’s status, open it in your browser, and stop the processes without touching the terminal. It is designed to stay out of your way while giving you instant visibility into what is running and where.

## Features

- Monitor all your local projects and ports from the menu bar.
- See at a glance whether each project is running or stopped.
- Open any project instantly in your browser (localhost:<port>).
- View useful context like git branch, active process, and last running time.
- Stop a running port safely from the app.
- Keep your list organized with editable projects and quick refresh.
- Optional start at login so Port Scout is always ready when you start coding.

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
