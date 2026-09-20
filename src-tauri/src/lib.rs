use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, LogicalSize, Manager, Position, Rect, Size, State, Window, WindowEvent,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};

const PROJECTS_FILE: &str = "projects.json";
const RUNTIME_FILE: &str = "runtime.json";
const DISCOVERY_FILE: &str = "discovery.json";
const POPOVER_BASE_WIDTH: f64 = 320.0;
const POPOVER_BASE_HEIGHT: f64 = 420.0;
const POPOVER_MIN_WIDTH: f64 = 300.0;
const POPOVER_MIN_HEIGHT: f64 = 380.0;
const POPOVER_SAFE_MARGIN: f64 = 24.0;
const POPOVER_OFFSET_Y_LOGICAL: f64 = 8.0;
const TRAY_ID: &str = "port-scout-tray";
const TRAY_ICON: tauri::image::Image<'_> = tauri::include_image!("./icons/TrayIcon.png");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    id: String,
    name: String,
    path: String,
    port: u16,
    #[serde(default = "default_start_command")]
    start_command: String,
    created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddProjectInput {
    name: String,
    path: String,
    port: u16,
    #[serde(default = "default_start_command")]
    start_command: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateProjectInput {
    id: String,
    name: String,
    path: String,
    port: u16,
    #[serde(default = "default_start_command")]
    start_command: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectStatus {
    project_id: String,
    branch: String,
    is_running: bool,
    pid: Option<i32>,
    port_active: bool,
    run_state: RunState,
    owner_project_id: Option<String>,
    last_running_at: Option<String>,
    checked_at: String,
    error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum RunState {
    Stopped,
    Owned,
    OwnedByOther,
    Ambiguous,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum KillBlockedReason {
    NotRunning,
    OwnedByOther,
    Ambiguous,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct KillResult {
    project_id: String,
    attempted_pid: Option<i32>,
    terminated: bool,
    signal_used: String,
    blocked_reason: Option<KillBlockedReason>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    autostart_enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct DiscoverySettings {
    #[serde(default)]
    folders: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum PortSource {
    Env,
    PackageScript,
    EsbuildConfig,
    ViteConfig,
    DockerCompose,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PortCandidate {
    port: u16,
    source: PortSource,
    detail: String,
    confidence: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PortDetectionResult {
    best_port: Option<u16>,
    candidates: Vec<PortCandidate>,
    errors: Vec<String>,
    suggested_start_command: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiscoveredListener {
    port: u16,
    pid: i32,
    process_name: String,
    name: String,
    path: Option<String>,
    project_id: Option<String>,
    suggested_start_command: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ListenerDiscoveryResult {
    listeners: Vec<DiscoveredListener>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TcpListenerInfo {
    port: u16,
    pid: i32,
    process_name: String,
}

struct ListenerSnapshot {
    listeners: Vec<TcpListenerInfo>,
    cwd_by_pid: HashMap<i32, PathBuf>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct DetectedProjectMetadata {
    path: PathBuf,
    name: String,
    suggested_start_command: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum StartBlockedReason {
    AlreadyRunning,
    OwnedByOther,
    Ambiguous,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StartResult {
    project_id: String,
    attempted_command: String,
    launched: bool,
    spawned_pid: Option<i32>,
    blocked_reason: Option<StartBlockedReason>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeState {
    last_running_by_project: HashMap<String, String>,
}

struct AppState {
    projects: Mutex<Vec<Project>>,
    runtime: Mutex<RuntimeState>,
}

struct UiState {
    auto_hide_suspended: Mutex<bool>,
}

#[derive(Debug, Clone)]
struct PortRunInfo {
    run_state: RunState,
    owner_project_id: Option<String>,
    owner_pid: Option<i32>,
    port_active: bool,
}

#[tauri::command]
fn list_projects(state: State<'_, AppState>) -> Vec<Project> {
    state
        .projects
        .lock()
        .expect("projects lock poisoned")
        .clone()
}

#[tauri::command]
fn add_project(
    app: AppHandle,
    state: State<'_, AppState>,
    input: AddProjectInput,
) -> Result<Project, String> {
    let mut projects = state
        .projects
        .lock()
        .map_err(|_| "projects lock poisoned")?;

    let validated = validate_project_candidate(
        &input.name,
        &input.path,
        input.port,
        &input.start_command,
        &projects,
        None,
    )?;

    let project = Project {
        id: uuid::Uuid::new_v4().to_string(),
        name: validated.name,
        path: validated.path,
        port: validated.port,
        start_command: validated.start_command,
        created_at: now_iso(),
    };

    projects.push(project.clone());
    save_projects(&app, &projects)?;

    Ok(project)
}

#[tauri::command]
fn update_project(
    app: AppHandle,
    state: State<'_, AppState>,
    input: UpdateProjectInput,
) -> Result<Project, String> {
    let mut projects = state
        .projects
        .lock()
        .map_err(|_| "projects lock poisoned")?;

    let index = projects
        .iter()
        .position(|project| project.id == input.id)
        .ok_or_else(|| "Project not found".to_string())?;

    let validated = validate_project_candidate(
        &input.name,
        &input.path,
        input.port,
        &input.start_command,
        &projects,
        Some(&input.id),
    )?;

    projects[index].name = validated.name;
    projects[index].path = validated.path;
    projects[index].port = validated.port;
    projects[index].start_command = validated.start_command;

    save_projects(&app, &projects)?;

    Ok(projects[index].clone())
}

#[tauri::command]
fn remove_project(
    app: AppHandle,
    state: State<'_, AppState>,
    project_id: String,
) -> Result<(), String> {
    let mut projects = state
        .projects
        .lock()
        .map_err(|_| "projects lock poisoned")?;
    let old_len = projects.len();
    projects.retain(|project| project.id != project_id);

    if projects.len() == old_len {
        return Err("Project not found".to_string());
    }

    save_projects(&app, &projects)?;

    let mut runtime = state.runtime.lock().map_err(|_| "runtime lock poisoned")?;
    runtime.last_running_by_project.remove(&project_id);
    save_runtime(&app, &runtime)?;

    Ok(())
}

#[tauri::command]
fn reorder_projects(
    app: AppHandle,
    state: State<'_, AppState>,
    project_ids: Vec<String>,
) -> Result<Vec<Project>, String> {
    let mut projects = state
        .projects
        .lock()
        .map_err(|_| "projects lock poisoned")?;

    // Drain existing projects into an index-addressable vec to preserve original order.
    let mut all_projects: Vec<Option<Project>> = projects.drain(..).map(Some).collect();
    let mut index_by_id: HashMap<String, usize> = HashMap::with_capacity(all_projects.len());
    for (idx, maybe) in all_projects.iter().enumerate() {
        if let Some(p) = maybe {
            index_by_id.insert(p.id.clone(), idx);
        }
    }

    let mut reordered: Vec<Project> = Vec::with_capacity(all_projects.len());

    // First, push projects in the explicit order given by project_ids.
    for id in &project_ids {
        if let Some(&idx) = index_by_id.get(id) {
            if let Some(project) = all_projects[idx].take() {
                reordered.push(project);
            }
        }
    }

    // Append any projects not mentioned in project_ids, preserving original order.
    for maybe in all_projects {
        if let Some(project) = maybe {
            reordered.push(project);
        }
    }

    *projects = reordered;
    save_projects(&app, &projects)?;

    Ok(projects.clone())
}

#[tauri::command]
async fn refresh_status(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<ProjectStatus>, String> {
    let projects = state
        .projects
        .lock()
        .map_err(|_| "projects lock poisoned")?
        .clone();

    tauri::async_runtime::spawn_blocking(move || refresh_project_statuses(&app, projects))
        .await
        .map_err(|error| format!("Failed to refresh project status: {error}"))?
}

fn refresh_project_statuses(
    app: &AppHandle,
    projects: Vec<Project>,
) -> Result<Vec<ProjectStatus>, String> {
    let ports: HashSet<u16> = projects.iter().map(|project| project.port).collect();
    let snapshot = collect_listener_snapshot(Some(&ports))?;
    if let Some(warning) = snapshot
        .warnings
        .iter()
        .find(|warning| warning.starts_with("Port scan may be incomplete:"))
    {
        return Err(warning.clone());
    }

    let mut projects_by_port: HashMap<u16, Vec<Project>> = HashMap::new();
    for project in &projects {
        projects_by_port
            .entry(project.port)
            .or_default()
            .push(project.clone());
    }

    let mut run_info_by_project: HashMap<String, PortRunInfo> =
        HashMap::with_capacity(projects.len());
    for (port, projects_on_port) in projects_by_port {
        let listening_pids: Vec<i32> = snapshot
            .listeners
            .iter()
            .filter(|listener| listener.port == port)
            .map(|listener| listener.pid)
            .collect();
        let states = resolve_port_run_info_with_cwds(
            &projects_on_port,
            &listening_pids,
            &snapshot.cwd_by_pid,
        );
        run_info_by_project.extend(states);
    }

    // Run subprocesses before taking a shared state lock: mutations should never wait for git.
    let project_details: Vec<_> = projects
        .into_iter()
        .map(|project| {
            let path = Path::new(&project.path);
            let path_exists = path.is_dir();
            let branch = if path_exists {
                detect_branch(path)
            } else {
                "not-a-git-repo".to_string()
            };
            (project, path_exists, branch)
        })
        .collect();

    let state = app.state::<AppState>();
    let mut runtime = state.runtime.lock().map_err(|_| "runtime lock poisoned")?;
    let mut changed_runtime = false;
    let checked_at = now_iso();
    let mut statuses = Vec::with_capacity(project_details.len());
    for (project, path_exists, branch) in project_details {
        let run_info = run_info_by_project
            .get(&project.id)
            .cloned()
            .unwrap_or(PortRunInfo {
                run_state: RunState::Stopped,
                owner_project_id: None,
                owner_pid: None,
                port_active: false,
            });
        let is_running = run_info.run_state == RunState::Owned;
        let pid = if is_running { run_info.owner_pid } else { None };

        if is_running {
            runtime
                .last_running_by_project
                .insert(project.id.clone(), checked_at.clone());
            changed_runtime = true;
        }

        let last_running_at = runtime.last_running_by_project.get(&project.id).cloned();
        let error = if path_exists {
            None
        } else {
            Some("Path is missing or no longer a directory".to_string())
        };

        statuses.push(ProjectStatus {
            project_id: project.id,
            branch,
            is_running,
            pid,
            port_active: run_info.port_active,
            run_state: run_info.run_state,
            owner_project_id: run_info.owner_project_id,
            last_running_at,
            checked_at: checked_at.clone(),
            error,
        });
    }

    if changed_runtime {
        save_runtime(app, &runtime)?;
    }
    drop(runtime);

    sync_tray_active_count(app, &statuses);

    Ok(statuses)
}

#[tauri::command]
async fn discover_listeners(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ListenerDiscoveryResult, String> {
    let projects = state
        .projects
        .lock()
        .map_err(|_| "projects lock poisoned")?
        .clone();
    tauri::async_runtime::spawn_blocking(move || {
        let settings = get_discovery_settings(app)?;
        discover_in_folders(&settings, &projects)
    })
    .await
    .map_err(|error| format!("Failed to discover listening ports: {error}"))?
}

fn active_project_count(statuses: &[ProjectStatus]) -> usize {
    statuses.iter().filter(|status| status.is_running).count()
}

fn tray_title_for_active_count(count: usize) -> String {
    if count == 0 {
        String::new()
    } else {
        count.to_string()
    }
}

fn sync_tray_active_count(app: &AppHandle, statuses: &[ProjectStatus]) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };

    let title = tray_title_for_active_count(active_project_count(statuses));
    let _ = tray.set_title(Some(title.as_str()));
}

#[tauri::command]
fn open_project_url(state: State<'_, AppState>, project_id: String) -> Result<(), String> {
    let projects = state
        .projects
        .lock()
        .map_err(|_| "projects lock poisoned")?;
    let project = projects
        .iter()
        .find(|candidate| candidate.id == project_id)
        .ok_or_else(|| "Project not found".to_string())?;

    let url = build_localhost_url(project.port);
    let status = Command::new("open")
        .arg(url)
        .status()
        .map_err(|error| format!("Failed to open project URL: {error}"))?;

    if !status.success() {
        return Err("Failed to open URL in browser".to_string());
    }

    Ok(())
}

#[tauri::command]
fn kill_project_port(state: State<'_, AppState>, project_id: String) -> Result<KillResult, String> {
    let projects = state
        .projects
        .lock()
        .map_err(|_| "projects lock poisoned")?;
    let project = projects
        .iter()
        .find(|candidate| candidate.id == project_id)
        .ok_or_else(|| "Project not found".to_string())?
        .clone();
    let projects_on_port: Vec<Project> = projects
        .iter()
        .filter(|candidate| candidate.port == project.port)
        .cloned()
        .collect();
    drop(projects);

    let listening_pids = detect_listening_pids(project.port)?;
    let run_info_by_project = resolve_port_run_info(&projects_on_port, &listening_pids)?;
    let run_info = run_info_by_project
        .get(&project_id)
        .ok_or_else(|| "Project not found".to_string())?;

    let blocked_reason = blocked_reason_for_run_state(run_info.run_state);

    if let Some(reason) = blocked_reason {
        return Ok(KillResult {
            project_id,
            attempted_pid: None,
            terminated: false,
            signal_used: "none".to_string(),
            blocked_reason: Some(reason),
        });
    }

    let Some(pid) = run_info.owner_pid else {
        return Ok(KillResult {
            project_id,
            attempted_pid: None,
            terminated: false,
            signal_used: "none".to_string(),
            blocked_reason: Some(KillBlockedReason::Ambiguous),
        });
    };

    send_signal(pid, "-TERM")?;
    let mut signal_used = "SIGTERM".to_string();

    let terminated_after_term = wait_until_pid_dead(pid, Duration::from_secs(2))?;
    let terminated = if terminated_after_term {
        true
    } else {
        signal_used = "SIGKILL".to_string();
        send_signal(pid, "-KILL")?;
        wait_until_pid_dead(pid, Duration::from_secs(1))?
    };

    Ok(KillResult {
        project_id,
        attempted_pid: Some(pid),
        terminated,
        signal_used,
        blocked_reason: None,
    })
}

#[tauri::command]
fn start_project_server(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<StartResult, String> {
    let projects = state
        .projects
        .lock()
        .map_err(|_| "projects lock poisoned")?;
    let project = projects
        .iter()
        .find(|candidate| candidate.id == project_id)
        .ok_or_else(|| "Project not found".to_string())?
        .clone();
    let projects_on_port: Vec<Project> = projects
        .iter()
        .filter(|candidate| candidate.port == project.port)
        .cloned()
        .collect();
    drop(projects);

    let listening_pids = detect_listening_pids(project.port)?;
    let run_info_by_project = resolve_port_run_info(&projects_on_port, &listening_pids)?;
    let run_info = run_info_by_project
        .get(&project_id)
        .ok_or_else(|| "Project not found".to_string())?;

    let attempted_command = project.start_command.clone();
    let blocked_reason = start_blocked_reason_for_run_state(run_info.run_state);

    if let Some(reason) = blocked_reason {
        return Ok(StartResult {
            project_id,
            attempted_command,
            launched: false,
            spawned_pid: None,
            blocked_reason: Some(reason),
        });
    }

    let path = Path::new(&project.path);
    if !path.is_dir() {
        return Err("Project path is missing or no longer a directory".to_string());
    }

    let command = project.start_command.trim().to_string();
    if command.is_empty() {
        return Err("Start command cannot be empty".to_string());
    }

    let child = Command::new("/bin/zsh")
        .arg("-lc")
        .arg(&command)
        .current_dir(path)
        .env("PORT", project.port.to_string())
        .env("PATH", launch_shell_path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("Failed to start project server: {error}"))?;

    let spawned_pid = i32::try_from(child.id()).ok();

    Ok(StartResult {
        project_id,
        attempted_command,
        launched: true,
        spawned_pid,
        blocked_reason: None,
    })
}

#[tauri::command]
fn get_settings(app: AppHandle) -> Result<Settings, String> {
    let enabled = app
        .autolaunch()
        .is_enabled()
        .map_err(|error| format!("Failed to read autostart settings: {error}"))?;

    Ok(Settings {
        autostart_enabled: enabled,
    })
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<Settings, String> {
    if enabled {
        app.autolaunch()
            .enable()
            .map_err(|error| format!("Failed to enable autostart: {error}"))?;
    } else {
        app.autolaunch()
            .disable()
            .map_err(|error| format!("Failed to disable autostart: {error}"))?;
    }

    get_settings(app)
}

#[tauri::command]
fn get_discovery_settings(app: AppHandle) -> Result<DiscoverySettings, String> {
    load_json(&data_dir(&app)?.join(DISCOVERY_FILE))
}

#[tauri::command]
fn set_discovery_folders(
    app: AppHandle,
    folders: Vec<String>,
) -> Result<DiscoverySettings, String> {
    update_discovery_folders(&data_dir(&app)?.join(DISCOVERY_FILE), folders)
}

fn normalize_discovery_folders(
    folders: Vec<String>,
    previous: &[String],
) -> Result<Vec<String>, String> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for folder in folders {
        if folder.is_empty() || !Path::new(&folder).is_absolute() {
            return Err("Choose an existing folder using its absolute path".to_string());
        }
        let path = match normalize_path(&folder) {
            Ok(path) if Path::new(&path).is_dir() => path,
            // A disconnected/moved folder must not prevent removal of another
            // saved folder. New selections still require a usable directory.
            _ if previous.contains(&folder) => folder,
            Ok(_) => return Err(format!("Discovery folder is not a directory: {folder}")),
            Err(error) => return Err(error),
        };
        if seen.insert(path.clone()) {
            normalized.push(path);
        }
    }
    Ok(normalized)
}

fn update_discovery_folders(
    path: &Path,
    folders: Vec<String>,
) -> Result<DiscoverySettings, String> {
    let previous = load_json::<DiscoverySettings>(path).unwrap_or_default();
    let settings = DiscoverySettings {
        folders: normalize_discovery_folders(folders, &previous.folders)?,
    };
    // Readers can scan concurrently; never expose a partially written settings file.
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = save_json(&temporary, &settings).and_then(|()| {
        fs::rename(&temporary, path)
            .map_err(|error| format!("Failed to save discovery folders: {error}"))
    });
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(settings)
}

#[tauri::command]
fn detect_project_ports(path: String) -> Result<PortDetectionResult, String> {
    detect_project_ports_for_path(&path)
}

#[tauri::command]
fn hide_main_window(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        window
            .hide()
            .map_err(|error| format!("Failed to hide window: {error}"))?;
    }

    Ok(())
}

#[tauri::command]
fn set_auto_hide_suspended(state: State<'_, UiState>, suspended: bool) -> Result<(), String> {
    let mut guard = state
        .auto_hide_suspended
        .lock()
        .map_err(|_| "ui state lock poisoned")?;
    *guard = suspended;
    Ok(())
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

fn default_start_command() -> String {
    "npm run dev".to_string()
}

fn launch_shell_path() -> String {
    let mut path_entries: Vec<String> = env::var("PATH")
        .ok()
        .into_iter()
        .flat_map(|value| {
            value
                .split(':')
                .map(|entry| entry.to_string())
                .collect::<Vec<_>>()
        })
        .filter(|entry| !entry.trim().is_empty())
        .collect();

    // Finder-launched macOS apps often miss Homebrew/user bins.
    let mut common_entries = vec![
        "/opt/homebrew/bin".to_string(),
        "/opt/homebrew/sbin".to_string(),
        "/usr/local/bin".to_string(),
        "/usr/local/sbin".to_string(),
        "/usr/bin".to_string(),
        "/bin".to_string(),
        "/usr/sbin".to_string(),
        "/sbin".to_string(),
    ];

    if let Ok(home) = env::var("HOME") {
        common_entries.push(format!("{home}/.local/bin"));
        common_entries.push(format!("{home}/.bun/bin"));
        common_entries.push(format!("{home}/.cargo/bin"));
    }

    for entry in common_entries {
        if !path_entries.iter().any(|existing| existing == &entry) {
            path_entries.push(entry);
        }
    }

    path_entries.join(":")
}

fn build_localhost_url(port: u16) -> String {
    format!("http://localhost:{port}")
}

fn normalize_path(path: &str) -> Result<String, String> {
    let canonical = fs::canonicalize(path).map_err(|error| format!("Invalid path: {error}"))?;
    canonical
        .to_str()
        .map(|value| value.to_string())
        .ok_or_else(|| "Path contains invalid UTF-8".to_string())
}

struct ValidatedProjectInput {
    name: String,
    path: String,
    port: u16,
    start_command: String,
}

fn validate_project_candidate(
    name: &str,
    path: &str,
    port: u16,
    start_command: &str,
    projects: &[Project],
    ignore_project_id: Option<&str>,
) -> Result<ValidatedProjectInput, String> {
    let trimmed_name = name.trim();
    if trimmed_name.is_empty() {
        return Err("Project name cannot be empty".to_string());
    }

    if port == 0 {
        return Err("Port must be in the range 1..65535".to_string());
    }

    let trimmed_start_command = start_command.trim();
    if trimmed_start_command.is_empty() {
        return Err("Start command cannot be empty".to_string());
    }

    let normalized_path = normalize_path(path)?;
    let as_path = Path::new(&normalized_path);
    if !as_path.is_dir() {
        return Err("Project path must be an existing directory".to_string());
    }

    for project in projects {
        if ignore_project_id.is_some_and(|id| project.id == id) {
            continue;
        }

        if project.path == normalized_path {
            return Err(format!(
                "Path '{}' is already assigned to project '{}'",
                normalized_path, project.name
            ));
        }
    }

    Ok(ValidatedProjectInput {
        name: trimmed_name.to_string(),
        path: normalized_path,
        port,
        start_command: trimmed_start_command.to_string(),
    })
}

fn detect_project_ports_for_path(path: &str) -> Result<PortDetectionResult, String> {
    let normalized_path = normalize_path(path)?;
    let base_path = PathBuf::from(&normalized_path);
    if !base_path.is_dir() {
        return Err("Project path must be an existing directory".to_string());
    }

    let mut errors = Vec::new();
    let mut candidates = Vec::new();
    candidates.extend(detect_from_env_files(&base_path, &mut errors));
    candidates.extend(detect_from_package_json(&base_path, &mut errors));
    candidates.extend(detect_from_esbuild_config_files(&base_path, &mut errors));
    candidates.extend(detect_from_vite_config_files(&base_path, &mut errors));
    candidates.extend(detect_from_docker_compose_files(&base_path, &mut errors));
    let suggested_start_command = detect_project_start_command_for_path(&base_path);

    let ranked_candidates = rank_and_dedupe_candidates(candidates);
    let best_port = ranked_candidates.first().map(|candidate| candidate.port);

    Ok(PortDetectionResult {
        best_port,
        candidates: ranked_candidates,
        errors,
        suggested_start_command,
    })
}

fn detect_from_env_files(base_path: &Path, errors: &mut Vec<String>) -> Vec<PortCandidate> {
    let mut candidates = Vec::new();
    for name in [
        ".env.local",
        ".env.development.local",
        ".env.development",
        ".env",
    ] {
        let file_path = base_path.join(name);
        if !file_path.exists() {
            continue;
        }

        let contents = match fs::read_to_string(&file_path) {
            Ok(contents) => contents,
            Err(error) => {
                errors.push(format!("Failed to read {}: {error}", file_path.display()));
                continue;
            }
        };

        for (line_index, line) in contents.lines().enumerate() {
            if let Some(port) = extract_port_from_env_line(line) {
                candidates.push(PortCandidate {
                    port,
                    source: PortSource::Env,
                    detail: format!("{name}:{}", line_index + 1),
                    confidence: 0.95,
                });
            }
        }
    }

    candidates
}

fn extract_port_from_env_line(line: &str) -> Option<u16> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let without_export = trimmed
        .strip_prefix("export ")
        .unwrap_or(trimmed)
        .trim_start();
    if !without_export.starts_with("PORT") {
        return None;
    }

    let rest = &without_export["PORT".len()..];
    let rest = rest.trim_start();
    if !rest.starts_with('=') {
        return None;
    }

    parse_port_value(&rest[1..])
}

fn detect_from_package_json(base_path: &Path, errors: &mut Vec<String>) -> Vec<PortCandidate> {
    let file_path = base_path.join("package.json");
    if !file_path.exists() {
        return Vec::new();
    }

    let contents = match fs::read_to_string(&file_path) {
        Ok(contents) => contents,
        Err(error) => {
            errors.push(format!("Failed to read {}: {error}", file_path.display()));
            return Vec::new();
        }
    };

    let parsed = match serde_json::from_str::<serde_json::Value>(&contents) {
        Ok(parsed) => parsed,
        Err(error) => {
            errors.push(format!("Failed to parse {}: {error}", file_path.display()));
            return Vec::new();
        }
    };

    let mut candidates = Vec::new();
    let Some(scripts) = parsed.get("scripts").and_then(|value| value.as_object()) else {
        return candidates;
    };

    for key in ["dev", "start", "serve"] {
        let Some(script) = scripts.get(key).and_then(|value| value.as_str()) else {
            continue;
        };

        let ports = detect_ports_in_script(script);
        for port in ports {
            candidates.push(PortCandidate {
                port,
                source: PortSource::PackageScript,
                detail: format!("package.json:scripts.{key}"),
                confidence: 0.85,
            });
        }
    }

    candidates
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageManager {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

fn detect_project_start_command_for_path(base_path: &Path) -> Option<String> {
    let file_path = base_path.join("package.json");
    if !file_path.exists() {
        return None;
    }

    let contents = match fs::read_to_string(&file_path) {
        Ok(contents) => contents,
        Err(_) => return None,
    };

    let parsed = match serde_json::from_str::<serde_json::Value>(&contents) {
        Ok(parsed) => parsed,
        Err(_) => return None,
    };

    let scripts = parsed.get("scripts").and_then(|value| value.as_object())?;
    let script_key = preferred_start_script_key(scripts)?;

    let package_manager = detect_package_manager(base_path);
    Some(command_for_package_manager(package_manager, script_key))
}

fn preferred_start_script_key(
    scripts: &serde_json::Map<String, serde_json::Value>,
) -> Option<&'static str> {
    for key in ["dev", "start", "serve"] {
        let is_valid = scripts
            .get(key)
            .and_then(|value| value.as_str())
            .is_some_and(|value| !value.trim().is_empty());
        if is_valid {
            return Some(key);
        }
    }

    None
}

fn detect_package_manager(base_path: &Path) -> PackageManager {
    if base_path.join("pnpm-lock.yaml").is_file() {
        return PackageManager::Pnpm;
    }

    if base_path.join("yarn.lock").is_file() {
        return PackageManager::Yarn;
    }

    if base_path.join("bun.lockb").is_file() || base_path.join("bun.lock").is_file() {
        return PackageManager::Bun;
    }

    PackageManager::Npm
}

fn command_for_package_manager(package_manager: PackageManager, script_key: &str) -> String {
    match package_manager {
        PackageManager::Npm => format!("npm run {script_key}"),
        PackageManager::Pnpm => format!("pnpm run {script_key}"),
        PackageManager::Yarn => format!("yarn {script_key}"),
        PackageManager::Bun => format!("bun run {script_key}"),
    }
}

fn detect_ports_in_script(script: &str) -> Vec<u16> {
    let raw_tokens: Vec<&str> = script.split_whitespace().collect();
    let mut ports = Vec::new();

    let mut index = 0usize;
    while index < raw_tokens.len() {
        let token = normalize_script_token(raw_tokens[index]);

        if let Some(port) = token.strip_prefix("PORT=").and_then(parse_port_value) {
            push_unique_port(&mut ports, port);
        }

        if let Some(port) = token.strip_prefix("--port=").and_then(parse_port_value) {
            push_unique_port(&mut ports, port);
        }

        if (token == "--port" || token == "-p") && index + 1 < raw_tokens.len() {
            let next_token = normalize_script_token(raw_tokens[index + 1]);
            if let Some(port) = parse_port_value(&next_token) {
                push_unique_port(&mut ports, port);
            }
        }

        index += 1;
    }

    ports
}

fn normalize_script_token(token: &str) -> String {
    token
        .trim_matches(|character: char| {
            matches!(
                character,
                '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}'
            )
        })
        .to_string()
}

fn detect_from_docker_compose_files(
    base_path: &Path,
    errors: &mut Vec<String>,
) -> Vec<PortCandidate> {
    let mut candidates = Vec::new();
    for name in [
        "docker-compose.yml",
        "docker-compose.yaml",
        "compose.yml",
        "compose.yaml",
    ] {
        let file_path = base_path.join(name);
        if !file_path.exists() {
            continue;
        }

        let contents = match fs::read_to_string(&file_path) {
            Ok(contents) => contents,
            Err(error) => {
                errors.push(format!("Failed to read {}: {error}", file_path.display()));
                continue;
            }
        };

        for (line_index, line) in contents.lines().enumerate() {
            let ports = detect_ports_in_compose_line(line);
            for port in ports {
                candidates.push(PortCandidate {
                    port,
                    source: PortSource::DockerCompose,
                    detail: format!("{name}:{}", line_index + 1),
                    confidence: 0.75,
                });
            }
        }
    }

    candidates
}

fn detect_from_esbuild_config_files(
    base_path: &Path,
    errors: &mut Vec<String>,
) -> Vec<PortCandidate> {
    let mut candidates = Vec::new();
    for name in [
        "esbuild.config.js",
        "esbuild.config.mjs",
        "esbuild.config.cjs",
        "esbuild.config.ts",
        "esbuild.config.mts",
        "esbuild.config.cts",
    ] {
        let file_path = base_path.join(name);
        if !file_path.exists() {
            continue;
        }

        let contents = match fs::read_to_string(&file_path) {
            Ok(contents) => contents,
            Err(error) => {
                errors.push(format!("Failed to read {}: {error}", file_path.display()));
                continue;
            }
        };

        for (port, line) in detect_ports_in_esbuild_config(&contents) {
            candidates.push(PortCandidate {
                port,
                source: PortSource::EsbuildConfig,
                detail: format!("{name}:{line}"),
                confidence: 0.82,
            });
        }
    }

    candidates
}

fn detect_ports_in_esbuild_config(contents: &str) -> Vec<(u16, usize)> {
    let mut matches = Vec::new();
    let mut seen_ports = HashSet::new();
    for (line_index, line) in contents.lines().enumerate() {
        let line = strip_js_line_comment(line);
        if let Some(port) = extract_port_assignment_from_line(line) {
            if seen_ports.insert(port) {
                matches.push((port, line_index + 1));
            }
        }
    }

    matches
}

fn detect_from_vite_config_files(base_path: &Path, errors: &mut Vec<String>) -> Vec<PortCandidate> {
    let mut candidates = Vec::new();
    for name in [
        "vite.config.js",
        "vite.config.mjs",
        "vite.config.cjs",
        "vite.config.ts",
        "vite.config.mts",
        "vite.config.cts",
    ] {
        let file_path = base_path.join(name);
        if !file_path.exists() {
            continue;
        }

        let contents = match fs::read_to_string(&file_path) {
            Ok(contents) => contents,
            Err(error) => {
                errors.push(format!("Failed to read {}: {error}", file_path.display()));
                continue;
            }
        };

        for (port, line) in detect_ports_in_vite_config(&contents) {
            candidates.push(PortCandidate {
                port,
                source: PortSource::ViteConfig,
                detail: format!("{name}:{line}"),
                confidence: 0.80,
            });
        }
    }

    candidates
}

fn detect_ports_in_vite_config(contents: &str) -> Vec<(u16, usize)> {
    let mut matches = Vec::new();
    let mut seen_ports = HashSet::new();
    let mut in_server_block = false;
    let mut server_block_depth = 0i32;

    for (line_index, raw_line) in contents.lines().enumerate() {
        let line = strip_js_line_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }

        if let Some(port) = extract_port_from_server_dot_assignment(line) {
            if seen_ports.insert(port) {
                matches.push((port, line_index + 1));
            }
        }

        if in_server_block {
            if let Some(port) = extract_port_assignment_from_line(line) {
                if seen_ports.insert(port) {
                    matches.push((port, line_index + 1));
                }
            }

            server_block_depth += brace_depth_delta(line);
            if server_block_depth <= 0 {
                in_server_block = false;
                server_block_depth = 0;
            }
            continue;
        }

        if is_vite_server_block_start(line) {
            in_server_block = true;
            server_block_depth = brace_depth_delta(line);

            if let Some(port) = extract_port_assignment_from_line(line) {
                if seen_ports.insert(port) {
                    matches.push((port, line_index + 1));
                }
            }

            if server_block_depth <= 0 {
                in_server_block = false;
                server_block_depth = 0;
            }
        }
    }

    matches
}

fn strip_js_line_comment(line: &str) -> &str {
    line.split("//").next().unwrap_or(line)
}

fn extract_port_assignment_from_line(line: &str) -> Option<u16> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    for (index, _) in line.match_indices("port") {
        let before = line[..index].chars().next_back();
        if before.is_some_and(is_identifier_char) {
            continue;
        }

        let after_word = &line[index + "port".len()..];
        let first_after = after_word.chars().next();
        if first_after.is_some_and(is_identifier_char) {
            continue;
        }

        let after_word = after_word.trim_start();
        if !(after_word.starts_with(':') || after_word.starts_with('=')) {
            continue;
        }

        let expression = after_word[1..].trim_start();
        if let Some(port) = parse_port_expression(expression) {
            return Some(port);
        }
    }

    None
}

fn extract_port_from_server_dot_assignment(line: &str) -> Option<u16> {
    let Some(index) = line.find("server.port") else {
        return None;
    };

    let after_assignment = line[index + "server.port".len()..].trim_start();
    if !(after_assignment.starts_with(':') || after_assignment.starts_with('=')) {
        return None;
    }

    parse_port_expression(after_assignment[1..].trim_start())
}

fn parse_port_expression(value: &str) -> Option<u16> {
    if let Some(port) = parse_port_value(value) {
        return Some(port);
    }

    for separator in ["||", "??"] {
        if let Some(index) = value.rfind(separator) {
            if let Some(port) = parse_port_value(&value[index + separator.len()..]) {
                return Some(port);
            }
        }
    }

    None
}

fn is_vite_server_block_start(line: &str) -> bool {
    for (index, _) in line.match_indices("server") {
        let before = line[..index].chars().next_back();
        if before.is_some_and(is_identifier_char) {
            continue;
        }

        let after_word = &line[index + "server".len()..];
        let first_after = after_word.chars().next();
        if first_after.is_some_and(is_identifier_char) {
            continue;
        }

        let after_word = after_word.trim_start();
        if (after_word.starts_with(':') || after_word.starts_with('=')) && after_word.contains('{')
        {
            return true;
        }
    }

    false
}

fn brace_depth_delta(line: &str) -> i32 {
    let opens = line.chars().filter(|character| *character == '{').count() as i32;
    let closes = line.chars().filter(|character| *character == '}').count() as i32;
    opens - closes
}

fn is_identifier_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn detect_ports_in_compose_line(line: &str) -> Vec<u16> {
    let content = line.split('#').next().unwrap_or("").trim();
    if content.is_empty() {
        return Vec::new();
    }

    let mut ports = Vec::new();
    for raw_token in content.split_whitespace() {
        let token = raw_token
            .trim_matches(|character: char| {
                matches!(
                    character,
                    '"' | '\'' | ',' | ';' | '[' | ']' | '(' | ')' | '{' | '}'
                )
            })
            .trim_start_matches('-')
            .trim();

        if let Some(port) = extract_host_port_from_mapping(token) {
            push_unique_port(&mut ports, port);
        }
    }

    ports
}

fn extract_host_port_from_mapping(token: &str) -> Option<u16> {
    if !token.contains(':') {
        return None;
    }

    let token = token.split('/').next().unwrap_or(token);
    let parts: Vec<&str> = token.split(':').collect();
    if parts.len() < 2 {
        return None;
    }

    let host_part = if parts.len() == 2 {
        parts[0]
    } else {
        parts[parts.len() - 2]
    };

    parse_port_value(host_part)
}

fn parse_port_value(value: &str) -> Option<u16> {
    let value = value
        .trim()
        .trim_start_matches(|character: char| matches!(character, '"' | '\'' | '(' | '[' | '{'))
        .trim_matches(|character: char| character == '"' || character == '\'');
    let digits: String = value
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }

    let parsed = digits.parse::<u16>().ok()?;
    (parsed > 0).then_some(parsed)
}

fn push_unique_port(ports: &mut Vec<u16>, port: u16) {
    if !ports.contains(&port) {
        ports.push(port);
    }
}

fn port_source_priority(source: &PortSource) -> u8 {
    match source {
        PortSource::Env => 0,
        PortSource::PackageScript => 1,
        PortSource::EsbuildConfig => 2,
        PortSource::ViteConfig => 3,
        PortSource::DockerCompose => 4,
    }
}

fn rank_and_dedupe_candidates(mut candidates: Vec<PortCandidate>) -> Vec<PortCandidate> {
    candidates.sort_by(|left, right| {
        right
            .confidence
            .partial_cmp(&left.confidence)
            .unwrap_or(Ordering::Equal)
            .then_with(|| {
                port_source_priority(&left.source).cmp(&port_source_priority(&right.source))
            })
            .then_with(|| left.port.cmp(&right.port))
    });

    let mut deduped = Vec::new();
    let mut seen_ports = HashSet::new();
    for candidate in candidates {
        if seen_ports.insert(candidate.port) {
            deduped.push(candidate);
        }
    }

    deduped
}

fn detect_branch(path: &Path) -> String {
    let branch_output = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("rev-parse")
        .arg("--abbrev-ref")
        .arg("HEAD")
        .output();

    let Ok(branch_output) = branch_output else {
        return "not-a-git-repo".to_string();
    };

    if !branch_output.status.success() {
        return "not-a-git-repo".to_string();
    }

    let branch_name = String::from_utf8_lossy(&branch_output.stdout)
        .trim()
        .to_string();
    if branch_name.is_empty() {
        return "not-a-git-repo".to_string();
    }

    if branch_name != "HEAD" {
        return branch_name;
    }

    let detached_output = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("rev-parse")
        .arg("--short")
        .arg("HEAD")
        .output();

    match detached_output {
        Ok(output) if output.status.success() => {
            let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if sha.is_empty() {
                "detached@unknown".to_string()
            } else {
                format!("detached@{sha}")
            }
        }
        _ => "detached@unknown".to_string(),
    }
}

// NUL-delimited lsof fields preserve process names and paths containing spaces.
fn lsof_fields(stdout: &str) -> impl Iterator<Item = &str> {
    stdout
        .split('\0')
        .map(|field| field.trim_start_matches('\n'))
}

fn parse_tcp_listeners(stdout: &str) -> Vec<TcpListenerInfo> {
    let mut listeners = Vec::new();
    let mut seen = HashSet::new();
    let mut pid = None;
    let mut process_name = String::new();
    for field in lsof_fields(stdout) {
        if let Some(value) = field.strip_prefix('p') {
            pid = value.parse::<i32>().ok().filter(|pid| *pid > 0);
            process_name.clear();
        } else if let Some(value) = field.strip_prefix('c') {
            process_name = value.to_string();
        } else if let Some(endpoint) = field.strip_prefix('n') {
            let Some(pid) = pid else { continue };
            // LISTEN has one local endpoint; never interpret a remote connection as a listener.
            if endpoint.contains("->") {
                continue;
            }
            let Some(port) = endpoint
                .rsplit_once(':')
                .and_then(|(_, port)| port.parse::<u16>().ok())
                .filter(|port| *port > 0)
            else {
                continue;
            };
            if seen.insert((pid, port)) {
                listeners.push(TcpListenerInfo {
                    port,
                    pid,
                    process_name: if process_name.is_empty() {
                        format!("PID {pid}")
                    } else {
                        process_name.clone()
                    },
                });
            }
        }
    }
    listeners.sort_by_key(|listener| (listener.port, listener.pid));
    listeners
}

fn parse_pid_cwds(stdout: &str) -> HashMap<i32, PathBuf> {
    let mut cwd_by_pid = HashMap::new();
    let mut pid = None;
    for field in lsof_fields(stdout) {
        if let Some(value) = field.strip_prefix('p') {
            pid = value.parse::<i32>().ok().filter(|pid| *pid > 0);
        } else if let Some(path) = field.strip_prefix('n') {
            if let Some(pid) = pid {
                let path = Path::new(path);
                if path.is_absolute() {
                    cwd_by_pid.insert(pid, path.to_path_buf());
                }
            }
        }
    }
    cwd_by_pid
}

fn collect_listener_snapshot(ports: Option<&HashSet<u16>>) -> Result<ListenerSnapshot, String> {
    let output = Command::new("/usr/sbin/lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-F0pcn"])
        .output()
        .map_err(|error| format!("Failed to discover listening ports: {error}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    // lsof returns 1 for an empty result set; other failures must not look like free ports.
    if !output.status.success()
        && (output.status.code() != Some(1) || !stderr.is_empty())
        && output.stdout.is_empty()
    {
        return Err(format!(
            "Failed to discover listening ports: {}",
            if stderr.is_empty() {
                output.status.to_string()
            } else {
                stderr
            }
        ));
    }
    let listeners = parse_tcp_listeners(&String::from_utf8_lossy(&output.stdout));
    let mut warnings = Vec::new();
    if !stderr.is_empty() || (!output.status.success() && !output.stdout.is_empty()) {
        warnings.push(format!(
            "Port scan may be incomplete: {}",
            if stderr.is_empty() {
                output.status.to_string()
            } else {
                stderr
            }
        ));
    }
    let pids: HashSet<i32> = listeners
        .iter()
        .filter(|listener| ports.map_or(true, |ports| ports.contains(&listener.port)))
        .map(|listener| listener.pid)
        .collect();
    let mut cwd_by_pid = HashMap::new();
    if !pids.is_empty() {
        let mut sorted_pids: Vec<i32> = pids.iter().copied().collect();
        sorted_pids.sort_unstable();
        match detect_pid_cwds(&sorted_pids) {
            Ok(cwds) => cwd_by_pid = cwds,
            Err(error) => warnings.push(error),
        }
        let missing = pids
            .iter()
            .filter(|pid| !cwd_by_pid.contains_key(pid))
            .count();
        if missing > 0 {
            warnings.push(format!(
                "Could not identify the working directory of {missing} listening process(es). They may have exited or require additional permissions."
            ));
        }
    }
    Ok(ListenerSnapshot {
        listeners,
        cwd_by_pid,
        warnings,
    })
}

fn detect_pid_cwds(pids: &[i32]) -> Result<HashMap<i32, PathBuf>, String> {
    if pids.is_empty() {
        return Ok(HashMap::new());
    }
    let pid_list = pids
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let output = Command::new("/usr/sbin/lsof")
        .args(["-a", "-p", &pid_list, "-d", "cwd", "-F0pn"])
        .output()
        .map_err(|error| format!("Failed to inspect process directories: {error}"))?;
    if !output.status.success() && output.stdout.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if !stderr.is_empty() || output.status.code() != Some(1) {
            return Err(format!(
                "Failed to inspect process directories: {}",
                if stderr.is_empty() {
                    output.status.to_string()
                } else {
                    stderr
                }
            ));
        }
    }
    Ok(parse_pid_cwds(&String::from_utf8_lossy(&output.stdout))
        .into_iter()
        .filter_map(|(pid, path)| fs::canonicalize(path).ok().map(|path| (pid, path)))
        .collect())
}

fn is_project_directory_candidate(path: &Path, home: Option<&Path>) -> bool {
    if !path.is_absolute() || path.parent().is_none() || home == Some(path) {
        return false;
    }
    if [
        "/Users",
        "/Volumes",
        "/private",
        "/private/var",
        "/var",
        "/tmp",
        "/private/tmp",
        "/opt",
    ]
    .iter()
    .any(|excluded| path == Path::new(excluded))
    {
        return false;
    }
    if [
        "/Applications",
        "/System",
        "/Library",
        "/usr",
        "/bin",
        "/sbin",
        "/opt/homebrew",
    ]
    .iter()
    .any(|excluded| path.starts_with(excluded))
        || home.is_some_and(|home| {
            [
                "Library",
                ".vscode/extensions",
                ".vscode-insiders/extensions",
                ".cursor/extensions",
            ]
            .iter()
            .any(|excluded| path.starts_with(home.join(excluded)))
        })
    {
        return false;
    }
    !path.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        name.ends_with(".app") || name == "node_modules" || name == ".git" || name == ".venv"
    })
}

fn detect_listener_project(cwd: &Path, home: Option<&Path>) -> Option<DetectedProjectMetadata> {
    for path in cwd.ancestors() {
        if home == Some(path) {
            break;
        }
        if !is_project_directory_candidate(path, home) {
            continue;
        }
        let has_manifest = ["package.json", "Cargo.toml", "pyproject.toml"]
            .iter()
            .any(|marker| path.join(marker).is_file());
        if !has_manifest && !path.join(".git").exists() {
            continue;
        }
        let package_name = fs::read_to_string(path.join("package.json"))
            .ok()
            .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok())
            .and_then(|package| {
                package
                    .get("name")
                    .and_then(|name| name.as_str())
                    .map(str::to_string)
            })
            .map(|name| name.trim().to_string())
            .filter(|name| {
                !name.is_empty() && name.len() <= 214 && !name.chars().any(char::is_control)
            });
        let name = package_name.or_else(|| {
            path.file_name()
                .map(|name| name.to_string_lossy().to_string())
        })?;
        return Some(DetectedProjectMetadata {
            path: path.to_path_buf(),
            name,
            suggested_start_command: detect_project_start_command_for_path(path),
        });
    }
    None
}

fn matching_saved_listener_project<'a>(
    projects: &'a [Project],
    port: u16,
    project_path: Option<&Path>,
    cwd: Option<&Path>,
) -> Option<&'a Project> {
    let mut matches = projects.iter().filter(|project| {
        if project.port != port {
            return false;
        }
        let saved_path =
            fs::canonicalize(&project.path).unwrap_or_else(|_| PathBuf::from(&project.path));
        project_path == Some(saved_path.as_path()) || cwd == Some(saved_path.as_path())
    });
    let project = matches.next()?;
    if matches.next().is_some() {
        None
    } else {
        Some(project)
    }
}

fn path_in_discovery_folders(path: &Path, folders: &[PathBuf]) -> bool {
    folders.iter().any(|folder| path.starts_with(folder))
}

fn discover_in_folders(
    settings: &DiscoverySettings,
    projects: &[Project],
) -> Result<ListenerDiscoveryResult, String> {
    let mut folders = Vec::new();
    let mut warnings = Vec::new();
    for folder in &settings.folders {
        match fs::canonicalize(folder) {
            Ok(path) if path.is_dir() => folders.push(path),
            _ => warnings.push(format!("Discovery folder is unavailable: {folder}")),
        }
    }
    // Discovery is opt-in. No configured/available folders means no process scan.
    if folders.is_empty() {
        return Ok(ListenerDiscoveryResult {
            listeners: Vec::new(),
            warnings,
        });
    }
    let mut snapshot = collect_listener_snapshot(None)?;
    snapshot.warnings.extend(warnings);
    Ok(build_listener_discovery(snapshot, projects, &folders))
}

fn build_listener_discovery(
    snapshot: ListenerSnapshot,
    projects: &[Project],
    folders: &[PathBuf],
) -> ListenerDiscoveryResult {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| fs::canonicalize(&home).unwrap_or(home));
    let mut metadata_by_cwd: HashMap<PathBuf, Option<DetectedProjectMetadata>> = HashMap::new();
    let listeners = snapshot
        .listeners
        .into_iter()
        .filter_map(|listener| {
            let cwd = snapshot.cwd_by_pid.get(&listener.pid)?;
            if !path_in_discovery_folders(cwd, folders) {
                return None;
            }
            let metadata = metadata_by_cwd
                .entry(cwd.clone())
                .or_insert_with(|| detect_listener_project(cwd, home.as_deref()))
                .as_ref();
            let saved = matching_saved_listener_project(
                projects,
                listener.port,
                metadata.map(|metadata| metadata.path.as_path()),
                Some(cwd.as_path()),
            );
            let project_path = saved
                .and_then(|project| fs::canonicalize(&project.path).ok())
                .or_else(|| metadata.map(|metadata| metadata.path.clone()))?;
            // A manifest above a chosen folder must not expand its scope.
            if !path_in_discovery_folders(&project_path, folders) {
                return None;
            }
            Some(DiscoveredListener {
                port: listener.port,
                pid: listener.pid,
                process_name: listener.process_name.clone(),
                name: saved
                    .map(|project| project.name.clone())
                    .or_else(|| metadata.map(|metadata| metadata.name.clone()))
                    .unwrap_or(listener.process_name),
                path: Some(project_path.to_string_lossy().to_string()),
                project_id: saved.map(|project| project.id.clone()),
                suggested_start_command: saved
                    .map(|project| project.start_command.clone())
                    .or_else(|| {
                        metadata.and_then(|metadata| metadata.suggested_start_command.clone())
                    }),
            })
        })
        .collect();
    ListenerDiscoveryResult {
        listeners,
        warnings: snapshot.warnings,
    }
}

fn parse_pids(stdout: &str) -> Vec<i32> {
    let mut pids = Vec::new();
    let mut seen = HashSet::new();
    for line in stdout.lines() {
        let Ok(pid) = line.trim().parse::<i32>() else {
            continue;
        };
        if seen.insert(pid) {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids
}

fn detect_listening_pids(port: u16) -> Result<Vec<i32>, String> {
    let output = Command::new("/usr/sbin/lsof")
        .arg("-nP")
        .arg(format!("-iTCP:{port}"))
        .arg("-sTCP:LISTEN")
        .arg("-t")
        .output()
        .map_err(|error| format!("Failed to check port status: {error}"))?;

    if output.stdout.is_empty() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_pids(&stdout))
}

#[cfg(test)]
fn detect_listening_pid(port: u16) -> Result<Option<i32>, String> {
    Ok(detect_listening_pids(port)?.into_iter().next())
}

fn project_matches_cwd(project_path: &str, cwd: &Path) -> bool {
    cwd.starts_with(Path::new(project_path))
}

fn classify_port_run_info(
    projects_on_port: &[Project],
    port_active: bool,
    matched_pids_by_project: HashMap<String, Vec<i32>>,
) -> HashMap<String, PortRunInfo> {
    let mut info_by_project = HashMap::with_capacity(projects_on_port.len());
    for project in projects_on_port {
        info_by_project.insert(
            project.id.clone(),
            PortRunInfo {
                run_state: RunState::Stopped,
                owner_project_id: None,
                owner_pid: None,
                port_active,
            },
        );
    }

    if !port_active {
        return info_by_project;
    }

    if matched_pids_by_project.len() == 1 {
        let (owner_project_id, owner_pids) = matched_pids_by_project
            .into_iter()
            .next()
            .expect("single-entry map should have one element");
        let owner_pid = owner_pids.into_iter().min();

        for project in projects_on_port {
            let Some(info) = info_by_project.get_mut(&project.id) else {
                continue;
            };
            if project.id == owner_project_id {
                info.run_state = RunState::Owned;
                info.owner_project_id = Some(owner_project_id.clone());
                info.owner_pid = owner_pid;
            } else {
                info.run_state = RunState::OwnedByOther;
                info.owner_project_id = Some(owner_project_id.clone());
            }
        }
    } else {
        for info in info_by_project.values_mut() {
            info.run_state = RunState::Ambiguous;
        }
    }

    info_by_project
}

fn resolve_port_run_info(
    projects_on_port: &[Project],
    listening_pids: &[i32],
) -> Result<HashMap<String, PortRunInfo>, String> {
    let cwd_by_pid = detect_pid_cwds(listening_pids)?;
    Ok(resolve_port_run_info_with_cwds(
        projects_on_port,
        listening_pids,
        &cwd_by_pid,
    ))
}

fn resolve_port_run_info_with_cwds(
    projects_on_port: &[Project],
    listening_pids: &[i32],
    cwd_by_pid: &HashMap<i32, PathBuf>,
) -> HashMap<String, PortRunInfo> {
    let mut matched_pids_by_project: HashMap<String, Vec<i32>> = HashMap::new();
    for pid in listening_pids {
        let Some(cwd) = cwd_by_pid.get(pid) else {
            continue;
        };

        for project in projects_on_port {
            if project_matches_cwd(&project.path, &cwd) {
                matched_pids_by_project
                    .entry(project.id.clone())
                    .or_default()
                    .push(*pid);
            }
        }
    }

    classify_port_run_info(
        projects_on_port,
        !listening_pids.is_empty(),
        matched_pids_by_project,
    )
}

fn blocked_reason_for_run_state(run_state: RunState) -> Option<KillBlockedReason> {
    match run_state {
        RunState::Stopped => Some(KillBlockedReason::NotRunning),
        RunState::OwnedByOther => Some(KillBlockedReason::OwnedByOther),
        RunState::Ambiguous => Some(KillBlockedReason::Ambiguous),
        RunState::Owned => None,
    }
}

fn start_blocked_reason_for_run_state(run_state: RunState) -> Option<StartBlockedReason> {
    match run_state {
        RunState::Stopped => None,
        RunState::Owned => Some(StartBlockedReason::AlreadyRunning),
        RunState::OwnedByOther => Some(StartBlockedReason::OwnedByOther),
        RunState::Ambiguous => Some(StartBlockedReason::Ambiguous),
    }
}

fn send_signal(pid: i32, signal: &str) -> Result<(), String> {
    let status = Command::new("kill")
        .arg(signal)
        .arg(pid.to_string())
        .status()
        .map_err(|error| format!("Failed to send signal {signal} to PID {pid}: {error}"))?;

    if !status.success() && is_pid_alive(pid)? {
        return Err(format!("Signal {signal} to PID {pid} failed"));
    }

    Ok(())
}

fn is_pid_alive(pid: i32) -> Result<bool, String> {
    let status = Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map_err(|error| format!("Failed to check PID status: {error}"))?;

    Ok(status.success())
}

#[cfg(test)]
fn wait_until_port_free(port: u16, timeout: Duration) -> Result<bool, String> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if detect_listening_pid(port)?.is_none() {
            return Ok(true);
        }
        thread::sleep(Duration::from_millis(100));
    }

    Ok(detect_listening_pid(port)?.is_none())
}

fn wait_until_pid_dead(pid: i32, timeout: Duration) -> Result<bool, String> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if !is_pid_alive(pid)? {
            return Ok(true);
        }
        thread::sleep(Duration::from_millis(100));
    }

    Ok(!is_pid_alive(pid)?)
}

fn data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Failed to resolve app data directory: {error}"))?;

    fs::create_dir_all(&dir)
        .map_err(|error| format!("Failed to create app data directory: {error}"))?;
    Ok(dir)
}

fn projects_file_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(data_dir(app)?.join(PROJECTS_FILE))
}

fn runtime_file_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(data_dir(app)?.join(RUNTIME_FILE))
}

fn load_projects(app: &AppHandle) -> Result<Vec<Project>, String> {
    let path = projects_file_path(app)?;
    load_json(&path)
}

fn save_projects(app: &AppHandle, projects: &[Project]) -> Result<(), String> {
    let path = projects_file_path(app)?;
    save_json(&path, projects)
}

fn load_runtime(app: &AppHandle) -> Result<RuntimeState, String> {
    let path = runtime_file_path(app)?;
    load_json(&path)
}

fn save_runtime(app: &AppHandle, runtime: &RuntimeState) -> Result<(), String> {
    let path = runtime_file_path(app)?;
    save_json(&path, runtime)
}

fn load_json<T>(path: &Path) -> Result<T, String>
where
    T: for<'de> Deserialize<'de> + Default,
{
    if !path.exists() {
        return Ok(T::default());
    }

    let contents = fs::read_to_string(path)
        .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
    serde_json::from_str(&contents)
        .map_err(|error| format!("Failed to parse {}: {error}", path.display()))
}

fn save_json<T>(path: &Path, value: &T) -> Result<(), String>
where
    T: Serialize + ?Sized,
{
    let payload = serde_json::to_string_pretty(value)
        .map_err(|error| format!("Failed to serialize JSON: {error}"))?;
    fs::write(path, payload).map_err(|error| format!("Failed to write {}: {error}", path.display()))
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct LogicalRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl LogicalRect {
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

#[derive(Debug, Clone, Copy)]
struct PopoverMonitor {
    scale_factor: f64,
    bounds: LogicalRect,
    work_area: LogicalRect,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PopoverLayout {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

fn logical_tray_rect(rect: &Rect, scale_factor: f64) -> LogicalRect {
    let position = rect.position.to_logical::<f64>(scale_factor);
    let size = rect.size.to_logical::<f64>(scale_factor);
    LogicalRect {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
    }
}

fn popover_monitors(app: &AppHandle) -> Vec<PopoverMonitor> {
    app.available_monitors()
        .unwrap_or_else(|error| {
            log::warn!("Could not read displays for popover: {error}");
            Vec::new()
        })
        .into_iter()
        .map(|monitor| {
            let scale_factor = monitor.scale_factor();
            let position = monitor.position().to_logical::<f64>(scale_factor);
            let size = monitor.size().to_logical::<f64>(scale_factor);
            let work_area = monitor.work_area();
            let work_position = work_area.position.to_logical::<f64>(scale_factor);
            let work_size = work_area.size.to_logical::<f64>(scale_factor);
            PopoverMonitor {
                scale_factor,
                bounds: LogicalRect {
                    x: position.x,
                    y: position.y,
                    width: size.width,
                    height: size.height,
                },
                work_area: LogicalRect {
                    x: work_position.x,
                    y: work_position.y,
                    width: work_size.width,
                    height: work_size.height,
                },
            }
        })
        .collect()
}

fn logical_cursor_position(app: &AppHandle) -> Option<(f64, f64)> {
    // Tao on macOS reports the cursor in the primary display's pixel scale,
    // whereas tray rectangles use the status item's own display scale.
    let scale = app.primary_monitor().ok()??.scale_factor();
    let cursor = app.cursor_position().ok()?.to_logical::<f64>(scale);
    Some((cursor.x, cursor.y))
}

fn tray_monitor<'a>(
    rect: &Rect,
    monitors: &'a [PopoverMonitor],
    cursor: Option<(f64, f64)>,
) -> Option<(&'a PopoverMonitor, LogicalRect)> {
    // Physical global rectangles can overlap on macOS when displays have
    // different scales. Normalize each candidate with its own scale and use
    // the pointer to disambiguate the display on which the tray was clicked.
    monitors
        .iter()
        .filter_map(|monitor| {
            let tray = logical_tray_rect(rect, monitor.scale_factor);
            let center_x = tray.x + tray.width / 2.0;
            let center_y = tray.y + tray.height / 2.0;
            monitor
                .bounds
                .contains(center_x, center_y)
                .then_some((monitor, tray))
        })
        .min_by(|(_, left), (_, right)| {
            let distance = |tray: &LogicalRect| {
                cursor.map_or(0.0, |(x, y)| {
                    (tray.x + tray.width / 2.0 - x).powi(2)
                        + (tray.y + tray.height / 2.0 - y).powi(2)
                })
            };
            distance(left).total_cmp(&distance(right))
        })
}

fn clamp_with_soft_min(base: f64, min: f64, max: f64) -> f64 {
    if max < min {
        return max.max(1.0);
    }
    base.clamp(min, max)
}

fn popover_layout(work_area: LogicalRect, tray: Option<LogicalRect>) -> PopoverLayout {
    let margin_x = POPOVER_SAFE_MARGIN.min(work_area.width / 4.0);
    let margin_y = POPOVER_SAFE_MARGIN.min(work_area.height / 4.0);
    let width = clamp_with_soft_min(
        POPOVER_BASE_WIDTH,
        POPOVER_MIN_WIDTH,
        work_area.width - margin_x * 2.0,
    );
    let height = clamp_with_soft_min(
        POPOVER_BASE_HEIGHT,
        POPOVER_MIN_HEIGHT,
        work_area.height - margin_y * 2.0,
    );
    let (desired_x, desired_y) = if let Some(tray) = tray {
        (
            tray.x + (tray.width - width) / 2.0,
            tray.y + tray.height + POPOVER_OFFSET_Y_LOGICAL,
        )
    } else {
        (
            work_area.x + (work_area.width - width) / 2.0,
            work_area.y + (work_area.height - height) / 2.0,
        )
    };
    let min_x = work_area.x + margin_x;
    let min_y = work_area.y + margin_y;
    PopoverLayout {
        x: desired_x.clamp(
            min_x,
            (work_area.x + work_area.width - margin_x - width).max(min_x),
        ),
        y: desired_y.clamp(
            min_y,
            (work_area.y + work_area.height - margin_y - height).max(min_y),
        ),
        width,
        height,
    }
}

fn log_window_result(action: &str, result: tauri::Result<()>) {
    if let Err(error) = result {
        log::warn!("Could not {action} Port Scout window: {error}");
    }
}

fn show_popover_window(app: &AppHandle, tray_rect: Option<&Rect>) {
    let Some(window) = app.get_webview_window("main") else {
        log::warn!("Cannot show Port Scout: main window is missing");
        return;
    };
    let monitors = popover_monitors(app);
    let cursor = logical_cursor_position(app);
    let layout = tray_rect
        .and_then(|rect| tray_monitor(rect, &monitors, cursor))
        .map(|(monitor, tray)| popover_layout(monitor.work_area, Some(tray)))
        .or_else(|| {
            monitors
                .iter()
                .find(|monitor| cursor.is_some_and(|(x, y)| monitor.bounds.contains(x, y)))
                .or_else(|| monitors.first())
                .map(|monitor| popover_layout(monitor.work_area, None))
        });

    if let Some(layout) = layout {
        // Use logical points throughout: Physical positions are converted by
        // Tao with the window's OLD display scale before crossing displays.
        log_window_result(
            "set minimum size for",
            window.set_min_size(Some(Size::Logical(LogicalSize::new(
                POPOVER_MIN_WIDTH.min(layout.width),
                POPOVER_MIN_HEIGHT.min(layout.height),
            )))),
        );
        log_window_result(
            "size",
            window.set_size(Size::Logical(LogicalSize::new(layout.width, layout.height))),
        );
        log_window_result(
            "position",
            window.set_position(Position::Logical(tauri::LogicalPosition::new(
                layout.x, layout.y,
            ))),
        );
    } else {
        log_window_result("center", window.center());
    }
    #[cfg(target_os = "macos")]
    log_window_result("unhide", app.show());
    log_window_result("show", window.show());
    log_window_result("focus", window.set_focus());
    let _ = app.emit("refresh-requested", ());
}

fn show_main_window(app: &AppHandle) {
    let rect = app
        .tray_by_id(TRAY_ID)
        .and_then(|tray| tray.rect().ok().flatten());
    show_popover_window(app, rect.as_ref());
}

fn toggle_popover_window(app: &AppHandle, tray_rect: Option<&Rect>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        log_window_result("hide", window.hide());
    } else {
        show_popover_window(app, tray_rect);
    }
}

fn pointer_is_over_tray(app: &AppHandle) -> bool {
    let Some(cursor) = logical_cursor_position(app) else {
        return false;
    };
    let Some(rect) = app
        .tray_by_id(TRAY_ID)
        .and_then(|tray| tray.rect().ok().flatten())
    else {
        return false;
    };
    tray_monitor(&rect, &popover_monitors(app), Some(cursor))
        .is_some_and(|(_, tray)| tray.contains(cursor.0, cursor.1))
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show Port Scout", true, None::<&str>)?;
    let refresh = MenuItem::with_id(app, "refresh", "Refresh", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show, &refresh, &separator, &quit])?;

    TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .icon(TRAY_ICON)
        .icon_as_template(true)
        .tooltip("Port Scout — click to open")
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main_window(app),
            "refresh" => {
                let _ = app.emit("refresh-requested", ());
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button,
                button_state,
                rect,
                ..
            } = event
            {
                if button == MouseButton::Left && button_state == MouseButtonState::Up {
                    toggle_popover_window(tray.app_handle(), Some(&rect));
                }
            }
        })
        .build(app)?;

    Ok(())
}

fn handle_window_focus(window: &Window, event: &WindowEvent) {
    if window.label() != "main" {
        return;
    }

    if matches!(event, WindowEvent::Focused(false)) {
        let suspended = window
            .app_handle()
            .try_state::<UiState>()
            .and_then(|state| state.auto_hide_suspended.lock().ok().map(|flag| *flag))
            .unwrap_or(false);
        // macOS 27 gives the status item focus on mouse-down. Leave that
        // case to the tray toggle; hiding here would reopen on mouse-up.
        if suspended || pointer_is_over_tray(window.app_handle()) {
            return;
        }
        log_window_result("hide after focus loss", window.hide());
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .plugin(tauri_plugin_log::Builder::default().build())
        .enable_macos_default_menu(false)
        .on_window_event(handle_window_focus)
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let projects = load_projects(app.handle())?;
            let runtime = load_runtime(app.handle())?;

            app.manage(AppState {
                projects: Mutex::new(projects),
                runtime: Mutex::new(runtime),
            });
            app.manage(UiState {
                auto_hide_suspended: Mutex::new(false),
            });

            build_tray(app.handle())?;

            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_always_on_top(true);
                let _ = window.set_skip_taskbar(true);
                let _ = window.hide();
            }
            if !env::args().any(|argument| argument == "--minimized") {
                show_main_window(app.handle());
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_projects,
            add_project,
            update_project,
            remove_project,
            reorder_projects,
            refresh_status,
            discover_listeners,
            open_project_url,
            kill_project_port,
            start_project_server,
            get_settings,
            set_autostart,
            get_discovery_settings,
            set_discovery_folders,
            detect_project_ports,
            hide_main_window,
            set_auto_hide_suspended,
            quit_app,
        ]);

    builder
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            #[cfg(target_os = "macos")]
            if matches!(event, tauri::RunEvent::Reopen { .. }) {
                show_main_window(app);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        net::TcpListener,
        process::{Command, Stdio},
        thread,
    };
    use tempfile::TempDir;

    fn mixed_scale_displays() -> [PopoverMonitor; 2] {
        [
            PopoverMonitor {
                scale_factor: 2.0,
                bounds: LogicalRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1440.0,
                    height: 932.0,
                },
                work_area: LogicalRect {
                    x: 0.0,
                    y: 32.0,
                    width: 1440.0,
                    height: 875.0,
                },
            },
            PopoverMonitor {
                scale_factor: 1.0,
                bounds: LogicalRect {
                    x: 1440.0,
                    y: 0.0,
                    width: 1920.0,
                    height: 1080.0,
                },
                work_area: LogicalRect {
                    x: 1440.0,
                    y: 24.0,
                    width: 1920.0,
                    height: 1032.0,
                },
            },
        ]
    }

    #[test]
    fn popover_retina_tray_uses_its_display_scale_when_pixel_bounds_overlap() {
        let displays = mixed_scale_displays();
        let rect = Rect {
            position: Position::Physical(tauri::PhysicalPosition::new(2600, 0)),
            size: Size::Physical(tauri::PhysicalSize::new(60, 64)),
        };
        let (monitor, tray) = tray_monitor(&rect, &displays, Some((1315.0, 16.0))).unwrap();
        assert_eq!(monitor.scale_factor, 2.0);
        assert_eq!(tray.x, 1300.0);
        assert_eq!(tray.width, 30.0);
        let layout = popover_layout(monitor.work_area, Some(tray));
        assert_eq!((layout.width, layout.height), (320.0, 420.0));
        assert!(layout.x + layout.width <= 1440.0 - POPOVER_SAFE_MARGIN);
    }

    #[test]
    fn popover_external_display_keeps_logical_size_and_clamps_to_same_display() {
        let displays = mixed_scale_displays();
        // Its desired left edge extends onto the Retina screen. The anchor's
        // display must remain the clamping target, rather than that left edge.
        let rect = Rect {
            position: Position::Physical(tauri::PhysicalPosition::new(1445, 0)),
            size: Size::Physical(tauri::PhysicalSize::new(30, 24)),
        };
        let (monitor, tray) = tray_monitor(&rect, &displays, Some((1460.0, 12.0))).unwrap();
        assert_eq!(monitor.scale_factor, 1.0);
        let layout = popover_layout(monitor.work_area, Some(tray));
        assert_eq!((layout.width, layout.height), (320.0, 420.0));
        assert_eq!(layout.x, 1440.0 + POPOVER_SAFE_MARGIN);
        assert!(layout.y >= monitor.work_area.y);
    }

    #[test]
    fn popover_supports_displays_left_of_and_above_the_main_display() {
        let work_area = LogicalRect {
            x: -1920.0,
            y: -500.0,
            width: 1920.0,
            height: 1040.0,
        };
        let tray = LogicalRect {
            x: -30.0,
            y: -524.0,
            width: 30.0,
            height: 24.0,
        };
        let layout = popover_layout(work_area, Some(tray));
        assert_eq!(layout.x, -344.0);
        assert_eq!(layout.y, -476.0);
        assert!(layout.x + layout.width < 0.0);
    }

    #[test]
    fn popover_shrinks_below_soft_minimum_when_work_area_is_too_small() {
        let work_area = LogicalRect {
            x: 100.0,
            y: 50.0,
            width: 250.0,
            height: 200.0,
        };
        let layout = popover_layout(work_area, None);
        assert_eq!((layout.width, layout.height), (202.0, 152.0));
        assert!(layout.x >= work_area.x);
        assert!(layout.y >= work_area.y);
        assert!(layout.x + layout.width <= work_area.x + work_area.width);
        assert!(layout.y + layout.height <= work_area.y + work_area.height);
    }

    #[test]
    fn popover_logical_rect_is_not_scaled_a_second_time() {
        let rect = Rect {
            position: Position::Logical(tauri::LogicalPosition::new(-1800.0, -24.0)),
            size: Size::Logical(LogicalSize::new(30.0, 24.0)),
        };
        let tray = logical_tray_rect(&rect, 2.0);
        assert_eq!(
            tray,
            LogicalRect {
                x: -1800.0,
                y: -24.0,
                width: 30.0,
                height: 24.0
            }
        );
    }

    #[test]
    fn parses_listening_pids() {
        assert_eq!(parse_pids("456\n123\n456\n"), vec![123, 456]);
        assert_eq!(parse_pids("\n"), Vec::<i32>::new());
        assert_eq!(parse_pids("abc\n999\n"), vec![999]);
    }

    #[test]
    fn parses_listeners_and_deduplicates_ipv4_and_ipv6() {
        let listeners = parse_tcp_listeners(concat!(
            "p123\0cnode\0\nf9\0n127.0.0.1:5173\0\nf10\0n[::1]:5173\0\n",
            "p456\0cCode Helper (Plugin)\0\nf8\0n*:3000\0\nf9\0n*:3001\0\n",
            "p789\0cpython3\0\nf7\0n*:5173\0\n"
        ));
        assert_eq!(listeners.len(), 4);
        assert_eq!(
            listeners[0],
            TcpListenerInfo {
                port: 3000,
                pid: 456,
                process_name: "Code Helper (Plugin)".to_string(),
            }
        );
        assert_eq!(listeners[2].pid, 123);
        assert_eq!(listeners[3].pid, 789);
    }

    #[test]
    fn ignores_malformed_listener_fields_and_connections() {
        let listeners = parse_tcp_listeners(concat!(
            "n*:80\0pnot-a-pid\0cnode\0n*:3000\0",
            "p123\0cnode\0n*:0\0n*:65536\0n*:http\0",
            "n127.0.0.1:5000->127.0.0.1:3000\0n*:5001\0",
            "p-5\0n*:3001\0p456\0n*:3002\0"
        ));
        assert_eq!(listeners.len(), 2);
        assert_eq!(listeners[0].port, 3002);
        assert_eq!(listeners[0].process_name, "PID 456");
        assert_eq!(listeners[1].port, 5001);
    }

    #[test]
    fn parses_process_directories_with_spaces_and_newlines() {
        let cwds = parse_pid_cwds("p123\0\nfcwd\0n/Users/a/My project\0\np456\0\nfcwd\0n/tmp/with\nnewline\0\np789\0nrelative\0");
        assert_eq!(cwds[&123], Path::new("/Users/a/My project"));
        assert_eq!(cwds[&456], Path::new("/tmp/with\nnewline"));
        assert!(!cwds.contains_key(&789));
    }

    #[test]
    fn detects_nearest_project_ancestor_and_package_metadata() {
        let temp = TempDir::new().expect("temporary project");
        let outer = temp.path().join("workspace");
        let inner = outer.join("packages/api");
        let cwd = inner.join("src/server");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir(outer.join(".git")).unwrap();
        fs::write(
            inner.join("package.json"),
            r#"{"name":"@example/api","scripts":{"dev":"vite"}}"#,
        )
        .unwrap();
        fs::write(inner.join("pnpm-lock.yaml"), "").unwrap();
        let metadata = detect_listener_project(&cwd, None).expect("detect inner project");
        assert_eq!(metadata.path, inner);
        assert_eq!(metadata.name, "@example/api");
        assert_eq!(
            metadata.suggested_start_command.as_deref(),
            Some("pnpm run dev")
        );
    }

    #[test]
    fn detects_rust_python_git_and_invalid_package_names_by_directory() {
        let temp = TempDir::new().unwrap();
        for (name, marker, contents) in [
            ("rust-service", "Cargo.toml", "[package]"),
            ("python-service", "pyproject.toml", "[project]"),
            ("worktree", ".git", "gitdir: elsewhere"),
            ("invalid-json", "package.json", "{"),
            ("blank-name", "package.json", r#"{"name":" "}"#),
            ("invalid-name", "package.json", r#"{"name":42}"#),
        ] {
            let path = temp.path().join(name);
            fs::create_dir(&path).unwrap();
            fs::write(path.join(marker), contents).unwrap();
            let metadata = detect_listener_project(&path, None).unwrap();
            assert_eq!(metadata.name, name);
            assert_eq!(metadata.path, path);
        }
    }

    #[test]
    fn does_not_infer_home_or_system_apps_as_projects() {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let cwd = home.join("Downloads/plain-server");
        fs::create_dir_all(&cwd).unwrap();
        fs::write(home.join("package.json"), "{}").unwrap();
        assert!(detect_listener_project(&cwd, Some(&home)).is_none());
        for path in [
            "/",
            "/Users",
            "/System/Applications/App.app/Contents",
            "/Applications/Code.app/Contents",
            "/usr/local/lib/node_modules/pkg",
        ] {
            assert!(!is_project_directory_candidate(
                Path::new(path),
                Some(&home)
            ));
        }
        assert!(!is_project_directory_candidate(
            &home.join("Library/Containers/app"),
            Some(&home)
        ));
    }

    #[test]
    fn excludes_editor_extensions_but_discovers_codex_worktrees() {
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        for editor in [".vscode", ".vscode-insiders", ".cursor"] {
            let extension = home
                .join(editor)
                .join("extensions/ms-python.vscode-pylance-2026.3.1");
            let cwd = extension.join("dist");
            fs::create_dir_all(&cwd).unwrap();
            fs::write(
                extension.join("package.json"),
                r#"{"name":"vscode-pylance"}"#,
            )
            .unwrap();
            assert!(!is_project_directory_candidate(&extension, Some(&home)));
            assert!(detect_listener_project(&cwd, Some(&home)).is_none());
        }

        let project = home.join(".codex/worktrees/abcd/my-project");
        let cwd = project.join("src");
        fs::create_dir_all(&cwd).unwrap();
        fs::write(project.join("package.json"), r#"{"name":"my-project"}"#).unwrap();
        let metadata =
            detect_listener_project(&cwd, Some(&home)).expect("discover worktree project");
        assert_eq!(metadata.path, project);
        assert_eq!(metadata.name, "my-project");
    }

    #[test]
    fn skips_dependencies_when_finding_project_root() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("frontend");
        let cwd = path.join("node_modules/vite");
        fs::create_dir_all(&cwd).unwrap();
        fs::write(path.join("package.json"), r#"{"name":"frontend"}"#).unwrap();
        fs::write(cwd.join("package.json"), r#"{"name":"vite"}"#).unwrap();
        assert_eq!(detect_listener_project(&cwd, None).unwrap().path, path);
    }

    #[test]
    fn matches_saved_listeners_only_by_unique_path_and_port() {
        let path = Path::new("/tmp/port-scout-listener-test/api");
        let project = test_project("api", &path.to_string_lossy(), 3000);
        let projects = vec![project.clone()];
        assert!(matching_saved_listener_project(&projects, 3000, Some(path), None).is_some());
        assert!(matching_saved_listener_project(&projects, 3001, Some(path), None).is_none());
        assert!(matching_saved_listener_project(
            &projects,
            3000,
            Some(Path::new("/tmp/another/api")),
            None
        )
        .is_none());
        assert!(
            matching_saved_listener_project(&projects, 3000, Some(&path.join("nested")), None)
                .is_none()
        );
        assert!(matching_saved_listener_project(
            &[project.clone(), project],
            3000,
            Some(path),
            None
        )
        .is_none());
    }

    #[test]
    fn ignores_unknown_listeners_outside_discovery_folders() {
        let result = build_listener_discovery(
            ListenerSnapshot {
                listeners: vec![TcpListenerInfo {
                    port: 7000,
                    pid: 123,
                    process_name: "ControlCenter".to_string(),
                }],
                cwd_by_pid: HashMap::from([(123, PathBuf::from("/"))]),
                warnings: vec!["partial scan".to_string()],
            },
            &[],
            &[PathBuf::from("/Users/developer/projects")],
        );
        assert!(result.listeners.is_empty());
        assert_eq!(result.warnings, vec!["partial scan"]);
    }

    #[test]
    fn discovery_is_empty_without_selected_folders() {
        let settings: DiscoverySettings = serde_json::from_str("{}").unwrap();
        assert!(settings.folders.is_empty());
        let result = discover_in_folders(&settings, &[]).unwrap();
        assert!(result.listeners.is_empty());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn discovery_settings_persist_and_invalid_changes_preserve_previous_values() {
        let temp = TempDir::new().unwrap();
        let directory = fs::canonicalize(temp.path()).unwrap();
        let projects = directory.join("projects");
        fs::create_dir(&projects).unwrap();
        let config = directory.join("discovery.json");
        let folder = projects.to_string_lossy().to_string();
        let saved =
            update_discovery_folders(&config, vec![folder.clone(), folder.clone()]).unwrap();
        assert_eq!(saved.folders, vec![folder.clone()]);
        assert_eq!(load_json::<DiscoverySettings>(&config).unwrap(), saved);
        assert!(update_discovery_folders(
            &config,
            vec![folder, "/missing-port-scout-folder".into()]
        )
        .is_err());
        assert!(
            update_discovery_folders(&config, vec![config.to_string_lossy().into_owned()]).is_err()
        );
        assert_eq!(load_json::<DiscoverySettings>(&config).unwrap(), saved);
        assert!(update_discovery_folders(&config, vec![])
            .unwrap()
            .folders
            .is_empty());
        assert!(load_json::<DiscoverySettings>(&config)
            .unwrap()
            .folders
            .is_empty());
    }

    #[test]
    fn can_remove_saved_folders_when_other_saved_folders_are_unavailable() {
        let temp = TempDir::new().unwrap();
        let base = fs::canonicalize(temp.path()).unwrap();
        let config = base.join("discovery.json");
        let first = base.join("first");
        let second = base.join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        update_discovery_folders(
            &config,
            vec![
                first.to_string_lossy().into_owned(),
                second.to_string_lossy().into_owned(),
            ],
        )
        .unwrap();
        fs::remove_dir(first).unwrap();
        fs::remove_dir(&second).unwrap();
        let remaining =
            update_discovery_folders(&config, vec![second.to_string_lossy().into_owned()]).unwrap();
        assert_eq!(remaining.folders.len(), 1);
        assert!(update_discovery_folders(
            &config,
            vec![base.join("never-added").to_string_lossy().into_owned()]
        )
        .is_err());
        assert!(update_discovery_folders(&config, vec![])
            .unwrap()
            .folders
            .is_empty());
    }

    #[test]
    fn discovery_includes_nested_projects_but_not_sibling_prefixes_or_saved_outsiders() {
        let temp = TempDir::new().unwrap();
        let base = fs::canonicalize(temp.path()).unwrap();
        let root = base.join("projects");
        let inside = root.join("group/maqueta");
        let outside = base.join("projects-other/api");
        let unknown = root.join("no-project");
        for path in [&inside, &outside, &unknown] {
            fs::create_dir_all(path.join("src")).unwrap();
        }
        for path in [&inside, &outside] {
            fs::write(path.join("package.json"), r#"{"name":"la-maqueta"}"#).unwrap();
        }
        let snapshot = ListenerSnapshot {
            listeners: (1..=4)
                .map(|pid| TcpListenerInfo {
                    port: 5000 + pid as u16,
                    pid,
                    process_name: "node".into(),
                })
                .collect(),
            cwd_by_pid: HashMap::from([
                (1, inside.join("src")),
                (2, outside.clone()),
                (3, unknown),
            ]),
            warnings: vec![],
        };
        let saved_outside = test_project("outside", &outside.to_string_lossy(), 5002);
        let result = build_listener_discovery(snapshot, &[saved_outside], &[root]);
        assert_eq!(result.listeners.len(), 1);
        assert_eq!(result.listeners[0].name, "la-maqueta");
        assert_eq!(result.listeners[0].port, 5001);
        assert_eq!(result.listeners[0].path.as_deref(), inside.to_str());
    }

    #[test]
    fn discovery_does_not_promote_manifest_above_selected_folder() {
        let temp = TempDir::new().unwrap();
        let project = fs::canonicalize(temp.path()).unwrap();
        let selected = project.join("src");
        fs::create_dir(&selected).unwrap();
        fs::write(project.join("package.json"), r#"{"name":"parent"}"#).unwrap();
        let result = build_listener_discovery(
            ListenerSnapshot {
                listeners: vec![TcpListenerInfo {
                    port: 5173,
                    pid: 1,
                    process_name: "node".into(),
                }],
                cwd_by_pid: HashMap::from([(1, selected.clone())]),
                warnings: vec![],
            },
            &[],
            &[selected],
        );
        assert!(result.listeners.is_empty());
    }

    #[test]
    fn discovery_reports_unavailable_folders_without_scanning_everything() {
        let temp = TempDir::new().unwrap();
        let result = discover_in_folders(
            &DiscoverySettings {
                folders: vec![temp.path().join("missing").to_string_lossy().into_owned()],
            },
            &[],
        )
        .unwrap();
        assert!(result.listeners.is_empty());
        assert_eq!(result.warnings.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn discovery_normalizes_aliases_and_excludes_symlink_targets_outside_roots() {
        use std::os::unix::fs::symlink;
        let temp = TempDir::new().unwrap();
        let base = fs::canonicalize(temp.path()).unwrap();
        let root = base.join("projects");
        let outside = base.join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("package.json"), r#"{"name":"outside"}"#).unwrap();
        let alias = base.join("projects-alias");
        symlink(&root, &alias).unwrap();
        symlink(&outside, root.join("linked-project")).unwrap();
        let normalized = normalize_discovery_folders(
            vec![
                alias.to_string_lossy().into_owned(),
                root.to_string_lossy().into_owned(),
            ],
            &[],
        )
        .unwrap();
        assert_eq!(normalized, vec![root.to_string_lossy().into_owned()]);
        let result = build_listener_discovery(
            ListenerSnapshot {
                listeners: vec![TcpListenerInfo {
                    port: 5173,
                    pid: 1,
                    process_name: "node".into(),
                }],
                cwd_by_pid: HashMap::from([(
                    1,
                    fs::canonicalize(root.join("linked-project")).unwrap(),
                )]),
                warnings: vec![],
            },
            &[],
            &[root],
        );
        assert!(result.listeners.is_empty());
    }

    #[test]
    fn listener_snapshot_finds_a_real_local_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind temporary listener");
        let port = listener.local_addr().unwrap().port();
        let pid = std::process::id() as i32;
        let snapshot =
            collect_listener_snapshot(Some(&HashSet::from([port]))).expect("scan local listeners");
        assert!(snapshot
            .listeners
            .iter()
            .any(|listener| listener.pid == pid && listener.port == port));
        assert_eq!(
            snapshot.cwd_by_pid.get(&pid),
            Some(&fs::canonicalize(env::current_dir().unwrap()).unwrap())
        );
    }

    #[test]
    fn builds_localhost_url() {
        assert_eq!(build_localhost_url(5173), "http://localhost:5173");
    }

    #[test]
    fn validates_project_input() {
        let temp_one = TempDir::new().expect("tempdir one");
        let temp_two = TempDir::new().expect("tempdir two");
        let first_path = temp_one.path().to_string_lossy().to_string();
        let second_path = temp_two.path().to_string_lossy().to_string();

        validate_project_candidate("api", &first_path, 3000, "npm run dev", &[], None)
            .expect("valid input should pass");

        let existing = vec![Project {
            id: "1".to_string(),
            name: "existing".to_string(),
            path: normalize_path(&first_path).expect("normalize path"),
            port: 3000,
            start_command: default_start_command(),
            created_at: now_iso(),
        }];

        let duplicate_port =
            validate_project_candidate("dup", &second_path, 3000, "pnpm run dev", &existing, None);
        assert!(duplicate_port.is_ok());

        let duplicate_path =
            validate_project_candidate("dup", &first_path, 4000, "pnpm run dev", &existing, None);
        assert!(duplicate_path.is_err());
    }

    #[test]
    fn validates_update_ignoring_current_project_id() {
        let temp_one = TempDir::new().expect("tempdir one");
        let temp_two = TempDir::new().expect("tempdir two");

        let first_path = normalize_path(&temp_one.path().to_string_lossy()).expect("normalize one");
        let second_path =
            normalize_path(&temp_two.path().to_string_lossy()).expect("normalize two");

        let projects = vec![
            Project {
                id: "project-a".to_string(),
                name: "a".to_string(),
                path: first_path.clone(),
                port: 3000,
                start_command: default_start_command(),
                created_at: now_iso(),
            },
            Project {
                id: "project-b".to_string(),
                name: "b".to_string(),
                path: second_path.clone(),
                port: 4000,
                start_command: default_start_command(),
                created_at: now_iso(),
            },
        ];

        let valid_self_update = validate_project_candidate(
            "renamed",
            &first_path,
            3000,
            "bun run dev",
            &projects,
            Some("project-a"),
        );
        assert!(valid_self_update.is_ok());

        let duplicate_other_port = validate_project_candidate(
            "renamed",
            &first_path,
            4000,
            "bun run dev",
            &projects,
            Some("project-a"),
        );
        assert!(duplicate_other_port.is_ok());

        let duplicate_other_path = validate_project_candidate(
            "renamed",
            &second_path,
            3000,
            "bun run dev",
            &projects,
            Some("project-a"),
        );
        assert!(duplicate_other_path.is_err());
    }

    #[test]
    fn classifies_shared_port_owned_and_owned_by_other() {
        let projects = vec![
            test_project("project-a", "/tmp/workspace/project-a", 3000),
            test_project("project-b", "/tmp/workspace/project-b", 3000),
        ];

        let mut matched = HashMap::new();
        matched.insert("project-a".to_string(), vec![7001]);
        let states = classify_port_run_info(&projects, true, matched);

        assert_eq!(states["project-a"].run_state, RunState::Owned);
        assert_eq!(
            states["project-a"].owner_project_id.as_deref(),
            Some("project-a")
        );
        assert_eq!(states["project-a"].owner_pid, Some(7001));
        assert_eq!(states["project-b"].run_state, RunState::OwnedByOther);
        assert_eq!(
            states["project-b"].owner_project_id.as_deref(),
            Some("project-a")
        );
    }

    #[test]
    fn classifies_single_project_as_owned() {
        let projects = vec![test_project("project-a", "/tmp/workspace/project-a", 3000)];

        let mut matched = HashMap::new();
        matched.insert("project-a".to_string(), vec![7001]);
        let states = classify_port_run_info(&projects, true, matched);

        assert_eq!(states["project-a"].run_state, RunState::Owned);
        assert_eq!(states["project-a"].owner_pid, Some(7001));
    }

    #[test]
    fn classifies_shared_port_as_ambiguous_without_unique_owner() {
        let projects = vec![
            test_project("project-a", "/tmp/workspace/project-a", 3000),
            test_project("project-b", "/tmp/workspace/project-b", 3000),
        ];

        let states = classify_port_run_info(&projects, true, HashMap::new());
        assert_eq!(states["project-a"].run_state, RunState::Ambiguous);
        assert_eq!(states["project-b"].run_state, RunState::Ambiguous);
        assert!(states["project-a"].port_active);
        assert!(states["project-b"].port_active);
    }

    #[test]
    fn classifies_port_as_stopped_when_no_listener() {
        let projects = vec![
            test_project("project-a", "/tmp/workspace/project-a", 3000),
            test_project("project-b", "/tmp/workspace/project-b", 3000),
        ];

        let states = classify_port_run_info(&projects, false, HashMap::new());
        assert_eq!(states["project-a"].run_state, RunState::Stopped);
        assert_eq!(states["project-b"].run_state, RunState::Stopped);
        assert!(!states["project-a"].port_active);
        assert!(!states["project-b"].port_active);
    }

    #[test]
    fn maps_blocked_reason_from_run_state() {
        assert_eq!(blocked_reason_for_run_state(RunState::Owned), None);
        assert_eq!(
            blocked_reason_for_run_state(RunState::Stopped),
            Some(KillBlockedReason::NotRunning)
        );
        assert_eq!(
            blocked_reason_for_run_state(RunState::OwnedByOther),
            Some(KillBlockedReason::OwnedByOther)
        );
        assert_eq!(
            blocked_reason_for_run_state(RunState::Ambiguous),
            Some(KillBlockedReason::Ambiguous)
        );
    }

    #[test]
    fn maps_start_blocked_reason_from_run_state() {
        assert_eq!(
            start_blocked_reason_for_run_state(RunState::Owned),
            Some(StartBlockedReason::AlreadyRunning)
        );
        assert_eq!(start_blocked_reason_for_run_state(RunState::Stopped), None);
        assert_eq!(
            start_blocked_reason_for_run_state(RunState::OwnedByOther),
            Some(StartBlockedReason::OwnedByOther)
        );
        assert_eq!(
            start_blocked_reason_for_run_state(RunState::Ambiguous),
            Some(StartBlockedReason::Ambiguous)
        );
    }

    #[test]
    fn default_start_command_is_npm_dev() {
        assert_eq!(default_start_command(), "npm run dev");
    }

    #[test]
    fn launch_shell_path_includes_common_binary_dirs() {
        let path = launch_shell_path();
        assert!(path.split(':').any(|entry| entry == "/opt/homebrew/bin"));
        assert!(path.split(':').any(|entry| entry == "/usr/local/bin"));
        assert!(path.split(':').any(|entry| entry == "/usr/bin"));
        assert!(path.split(':').any(|entry| entry == "/bin"));
    }

    #[test]
    fn rejects_empty_start_command() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().to_string_lossy().to_string();
        let result = validate_project_candidate("api", &path, 3000, "   ", &[], None);
        assert!(result.is_err());
    }

    #[test]
    fn detects_start_command_with_package_manager_lockfiles() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("package.json"),
            r#"{"scripts":{"dev":"vite","start":"node server.js"}}"#,
        )
        .expect("write package json");

        let npm_command = detect_project_start_command_for_path(temp.path());
        assert_eq!(npm_command.as_deref(), Some("npm run dev"));

        fs::write(temp.path().join("pnpm-lock.yaml"), "lockfileVersion: '9.0'")
            .expect("write pnpm lock");
        let pnpm_command = detect_project_start_command_for_path(temp.path());
        assert_eq!(pnpm_command.as_deref(), Some("pnpm run dev"));

        fs::remove_file(temp.path().join("pnpm-lock.yaml")).expect("remove pnpm lock");
        fs::write(temp.path().join("yarn.lock"), "__metadata:").expect("write yarn lock");
        let yarn_command = detect_project_start_command_for_path(temp.path());
        assert_eq!(yarn_command.as_deref(), Some("yarn dev"));

        fs::remove_file(temp.path().join("yarn.lock")).expect("remove yarn lock");
        fs::write(temp.path().join("bun.lockb"), "bun").expect("write bun lock");
        let bun_command = detect_project_start_command_for_path(temp.path());
        assert_eq!(bun_command.as_deref(), Some("bun run dev"));
    }

    #[test]
    fn detects_start_command_prefers_start_when_dev_missing() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("package.json"),
            r#"{"scripts":{"start":"next start","serve":"serve -s dist"}}"#,
        )
        .expect("write package json");
        let command = detect_project_start_command_for_path(temp.path());
        assert_eq!(command.as_deref(), Some("npm run start"));
    }

    #[test]
    fn detects_no_start_command_when_scripts_missing() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("package.json"),
            r#"{"scripts":{"test":"vitest"}}"#,
        )
        .expect("write package json");
        let command = detect_project_start_command_for_path(temp.path());
        assert_eq!(command, None);
    }

    #[test]
    fn counts_only_running_projects_as_active() {
        let statuses = vec![
            test_status("project-a", true, RunState::Owned),
            test_status("project-b", false, RunState::OwnedByOther),
            test_status("project-c", false, RunState::Ambiguous),
            test_status("project-d", false, RunState::Stopped),
        ];

        assert_eq!(active_project_count(&statuses), 1);
    }

    #[test]
    fn tray_title_is_hidden_when_no_active_projects() {
        assert_eq!(tray_title_for_active_count(0), "");
    }

    #[test]
    fn tray_title_is_number_when_active_projects_exist() {
        assert_eq!(tray_title_for_active_count(1), "1");
        assert_eq!(tray_title_for_active_count(12), "12");
    }

    #[test]
    fn extracts_port_from_env_lines() {
        assert_eq!(extract_port_from_env_line("PORT=3000"), Some(3000));
        assert_eq!(extract_port_from_env_line("PORT = '5173'"), Some(5173));
        assert_eq!(
            extract_port_from_env_line("export PORT=4173 # comment"),
            Some(4173)
        );
        assert_eq!(extract_port_from_env_line("NOT_PORT=3000"), None);
        assert_eq!(extract_port_from_env_line("PORT=abc"), None);
    }

    #[test]
    fn detects_ports_from_package_script_patterns() {
        let ports = detect_ports_in_script("cross-env PORT=5173 vite --port 3000 -p 4000");
        assert!(ports.contains(&5173));
        assert!(ports.contains(&3000));
        assert!(ports.contains(&4000));
    }

    #[test]
    fn detects_ports_from_docker_compose_lines() {
        assert_eq!(
            detect_ports_in_compose_line("      - \"127.0.0.1:3000:3000\""),
            vec![3000]
        );
        assert_eq!(detect_ports_in_compose_line("      - 8080:80"), vec![8080]);
        assert_eq!(detect_ports_in_compose_line("ports: []"), Vec::<u16>::new());
    }

    #[test]
    fn detects_ports_from_esbuild_config_patterns() {
        let ports = detect_ports_in_esbuild_config(
            r#"
              const ctx = await esbuild.context({
                entryPoints: ['src/index.ts'],
                port: process.env.PORT || 3000
              });
              await ctx.serve({ servedir: 'public', port: 4100 });
              const ignored = { transport: 1234 };
            "#,
        );

        assert!(ports.iter().any(|(port, _)| *port == 3000));
        assert!(ports.iter().any(|(port, _)| *port == 4100));
        assert!(!ports.iter().any(|(port, _)| *port == 1234));
    }

    #[test]
    fn detects_ports_from_vite_config_patterns() {
        let ports = detect_ports_in_vite_config(
            r#"
              export default defineConfig({
                server: {
                  host: true,
                  port: process.env.PORT || 5173,
                },
              });
              server.port = 4173;
              const config = { transport: 1234 };
            "#,
        );

        assert!(ports.iter().any(|(port, _)| *port == 5173));
        assert!(ports.iter().any(|(port, _)| *port == 4173));
        assert!(!ports.iter().any(|(port, _)| *port == 1234));
    }

    #[test]
    fn ranks_and_dedupes_detected_candidates() {
        let ranked = rank_and_dedupe_candidates(vec![
            PortCandidate {
                port: 3000,
                source: PortSource::DockerCompose,
                detail: "compose".to_string(),
                confidence: 0.75,
            },
            PortCandidate {
                port: 5174,
                source: PortSource::ViteConfig,
                detail: "vite".to_string(),
                confidence: 0.80,
            },
            PortCandidate {
                port: 5175,
                source: PortSource::EsbuildConfig,
                detail: "esbuild".to_string(),
                confidence: 0.82,
            },
            PortCandidate {
                port: 3000,
                source: PortSource::Env,
                detail: ".env".to_string(),
                confidence: 0.95,
            },
            PortCandidate {
                port: 5173,
                source: PortSource::PackageScript,
                detail: "package".to_string(),
                confidence: 0.85,
            },
        ]);

        assert_eq!(ranked.len(), 4);
        assert_eq!(ranked[0].port, 3000);
        assert_eq!(ranked[0].source, PortSource::Env);
        assert_eq!(ranked[1].port, 5173);
        assert_eq!(ranked[1].source, PortSource::PackageScript);
        assert_eq!(ranked[2].port, 5175);
        assert_eq!(ranked[2].source, PortSource::EsbuildConfig);
        assert_eq!(ranked[3].port, 5174);
        assert_eq!(ranked[3].source, PortSource::ViteConfig);
    }

    #[test]
    fn detect_project_ports_rejects_invalid_path() {
        let path = Path::new("/tmp").join(format!(
            "port_scout_missing_{}",
            now_iso().replace(':', "_")
        ));
        let result = detect_project_ports_for_path(path.to_string_lossy().as_ref());
        assert!(result.is_err());
    }

    #[test]
    fn detects_branch_fallbacks() {
        let temp = TempDir::new().expect("tempdir");
        assert_eq!(detect_branch(temp.path()), "not-a-git-repo");

        run_git(temp.path(), ["init"]);
        run_git(temp.path(), ["config", "user.email", "dev@example.com"]);
        run_git(temp.path(), ["config", "user.name", "Dev"]);

        fs::write(temp.path().join("README.md"), "hello").expect("write readme");
        run_git(temp.path(), ["add", "README.md"]);
        run_git(temp.path(), ["commit", "-m", "init"]);

        let branch = detect_branch(temp.path());
        assert!(branch == "main" || branch == "master");

        run_git(temp.path(), ["checkout", "--detach"]);
        let detached = detect_branch(temp.path());
        assert!(detached.starts_with("detached@"));
    }

    #[test]
    fn detects_and_kills_dummy_server() {
        if Command::new("python3").arg("--version").output().is_err() {
            return;
        }

        let mut launched = None;
        for _ in 0..3 {
            let port = available_port();
            let mut child = Command::new("python3")
                .arg("-m")
                .arg("http.server")
                .arg(port.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn dummy server");

            if wait_for_port(port, Duration::from_secs(10)) {
                launched = Some((port, child));
                break;
            }

            let _ = child.kill();
            let _ = child.wait();
        }

        let (port, mut child) = launched.expect("dummy server should start listening");

        let pid = detect_listening_pid(port)
            .expect("detect port")
            .expect("pid should be present");

        assert!(is_pid_alive(pid).expect("pid check"));

        let terminated = {
            send_signal(pid, "-TERM").expect("term signal");
            let after_term = wait_until_port_free(port, Duration::from_secs(2)).expect("wait term");
            if after_term {
                true
            } else {
                send_signal(pid, "-KILL").expect("kill signal");
                wait_until_port_free(port, Duration::from_secs(1)).expect("wait kill")
            }
        };

        assert!(terminated);

        let _ = child.try_wait();
    }

    fn test_project(id: &str, path: &str, port: u16) -> Project {
        Project {
            id: id.to_string(),
            name: id.to_string(),
            path: path.to_string(),
            port,
            start_command: default_start_command(),
            created_at: now_iso(),
        }
    }

    fn test_status(project_id: &str, is_running: bool, run_state: RunState) -> ProjectStatus {
        ProjectStatus {
            project_id: project_id.to_string(),
            branch: "main".to_string(),
            is_running,
            pid: if is_running { Some(1234) } else { None },
            port_active: is_running,
            run_state,
            owner_project_id: if is_running {
                Some(project_id.to_string())
            } else {
                None
            },
            last_running_at: None,
            checked_at: now_iso(),
            error: None,
        }
    }

    fn run_git<const N: usize>(path: &Path, args: [&str; N]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .status()
            .expect("run git command");
        assert!(status.success());
    }

    fn available_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind random port");
        listener.local_addr().expect("local addr").port()
    }

    fn wait_for_port(port: u16, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if detect_listening_pid(port).ok().flatten().is_some() {
                return true;
            }
            thread::sleep(Duration::from_millis(100));
        }

        false
    }
}
