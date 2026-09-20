# Changelog

All notable changes to this project are documented in this file.

## [0.2.0] - 2026-09-20

### Added

- Automatic TCP listener discovery with project identification and a reviewable Track action.
- Configurable discovery folders with persistent settings and filtering of unrelated processes.
- Green activity indicators for discovered projects, with newly detected servers shown first.
- Review and update a tracked project's port when its server moves to another port.
- Explicit Show Port Scout tray action and reopening the popover from Finder.

### Fixed

- macOS 27 tray click handling, using the upstream tray-icon fix.
- Popover positioning and sizing across displays with different pixel densities.
- Port inspection blocking the UI and overlapping frontend refreshes.
- Saved projects disappearing from the interface when another startup request fails.

## [0.1.0] - 2026-03-03

### Added

- First public release of Port Scout for macOS menu bar.
- Project list with per-project path + port configuration.
- Periodic status refresh, branch detection, and PID visibility.
- One-click localhost open and guarded port-kill workflow.
- Start-at-login setting and tray controls.
- In-app updater integration with download/install flow.
