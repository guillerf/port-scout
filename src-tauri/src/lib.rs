use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, Position, Rect, Size, State,
    WebviewWindow, Window, WindowEvent,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};

const PROJECTS_FILE: &str = "projects.json";
const RUNTIME_FILE: &str = "runtime.json";
const POPOVER_BASE_WIDTH: f64 = 320.0;
const POPOVER_BASE_HEIGHT: f64 = 420.0;
const POPOVER_MIN_WIDTH: f64 = 300.0;
const POPOVER_MIN_HEIGHT: f64 = 380.0;
const POPOVER_SAFE_MARGIN: f64 = 24.0;
const POPOVER_OFFSET_Y_LOGICAL: f64 = 8.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    id: String,
    name: String,
    path: String,
    port: u16,
    created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddProjectInput {
    name: String,
    path: String,
    port: u16,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateProjectInput {
    id: String,
    name: String,
    path: String,
    port: u16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectStatus {
    project_id: String,
    branch: String,
    is_running: bool,
    pid: Option<i32>,
    last_running_at: Option<String>,
    checked_at: String,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct KillResult {
    project_id: String,
    attempted_pid: Option<i32>,
    terminated: bool,
    signal_used: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    autostart_enabled: bool,
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

    let validated =
        validate_project_candidate(&input.name, &input.path, input.port, &projects, None)?;

    let project = Project {
        id: uuid::Uuid::new_v4().to_string(),
        name: validated.name,
        path: validated.path,
        port: validated.port,
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
        &projects,
        Some(&input.id),
    )?;

    projects[index].name = validated.name;
    projects[index].path = validated.path;
    projects[index].port = validated.port;

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
fn refresh_status(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<ProjectStatus>, String> {
    let projects = state
        .projects
        .lock()
        .map_err(|_| "projects lock poisoned")?
        .clone();

    let mut runtime = state.runtime.lock().map_err(|_| "runtime lock poisoned")?;
    let mut changed_runtime = false;
    let checked_at = now_iso();

    let mut statuses = Vec::with_capacity(projects.len());
    for project in projects {
        let path = Path::new(&project.path);
        let path_exists = path.is_dir();
        let branch = if path_exists {
            detect_branch(path)
        } else {
            "not-a-git-repo".to_string()
        };

        let pid = detect_listening_pid(project.port)?;
        let is_running = pid.is_some();

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
            last_running_at,
            checked_at: checked_at.clone(),
            error,
        });
    }

    if changed_runtime {
        save_runtime(&app, &runtime)?;
    }

    Ok(statuses)
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
        .ok_or_else(|| "Project not found".to_string())?;

    let pid = detect_listening_pid(project.port)?;
    let Some(pid) = pid else {
        return Ok(KillResult {
            project_id,
            attempted_pid: None,
            terminated: false,
            signal_used: "none".to_string(),
        });
    };

    send_signal(pid, "-TERM")?;
    let mut signal_used = "SIGTERM".to_string();

    let terminated_after_term = wait_until_port_free(project.port, Duration::from_secs(2))?;
    let terminated = if terminated_after_term {
        true
    } else {
        signal_used = "SIGKILL".to_string();
        send_signal(pid, "-KILL")?;
        wait_until_port_free(project.port, Duration::from_secs(1))?
    };

    Ok(KillResult {
        project_id,
        attempted_pid: Some(pid),
        terminated,
        signal_used,
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
}

fn validate_project_candidate(
    name: &str,
    path: &str,
    port: u16,
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

    let normalized_path = normalize_path(path)?;
    let as_path = Path::new(&normalized_path);
    if !as_path.is_dir() {
        return Err("Project path must be an existing directory".to_string());
    }

    for project in projects {
        if ignore_project_id.is_some_and(|id| project.id == id) {
            continue;
        }

        if project.port == port {
            return Err(format!(
                "Port {} is already assigned to project '{}'",
                port, project.name
            ));
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

    let ranked_candidates = rank_and_dedupe_candidates(candidates);
    let best_port = ranked_candidates.first().map(|candidate| candidate.port);

    Ok(PortDetectionResult {
        best_port,
        candidates: ranked_candidates,
        errors,
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

fn parse_first_pid(stdout: &str) -> Option<i32> {
    stdout
        .lines()
        .find_map(|line| line.trim().parse::<i32>().ok())
}

fn detect_listening_pid(port: u16) -> Result<Option<i32>, String> {
    let output = Command::new("lsof")
        .arg("-nP")
        .arg(format!("-iTCP:{port}"))
        .arg("-sTCP:LISTEN")
        .arg("-t")
        .output()
        .map_err(|error| format!("Failed to check port status: {error}"))?;

    if output.stdout.is_empty() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_first_pid(&stdout))
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

fn rect_values(rect: &Rect) -> (f64, f64, f64, f64) {
    let (x, y) = match rect.position {
        Position::Physical(position) => (position.x as f64, position.y as f64),
        Position::Logical(position) => (position.x, position.y),
    };

    let (width, height) = match rect.size {
        Size::Physical(size) => (size.width as f64, size.height as f64),
        Size::Logical(size) => (size.width, size.height),
    };

    (x, y, width, height)
}

struct PopoverSize {
    logical_width: f64,
    logical_height: f64,
    physical_width: f64,
    physical_height: f64,
    physical_offset_y: f64,
}

fn clamp_with_soft_min(base: f64, min: f64, max: f64) -> f64 {
    if max <= 0.0 {
        return base;
    }

    if max < min {
        return max;
    }

    base.clamp(min, max)
}

fn resolve_popover_size(app: &AppHandle, anchor_x: f64, anchor_y: f64) -> PopoverSize {
    let mut scale_factor = 1.0;
    let mut max_logical_width = POPOVER_BASE_WIDTH;
    let mut max_logical_height = POPOVER_BASE_HEIGHT;

    if let Ok(Some(monitor)) = app.monitor_from_point(anchor_x, anchor_y) {
        scale_factor = monitor.scale_factor();
        let work_area = monitor.work_area();
        max_logical_width = (work_area.size.width as f64 / scale_factor) - POPOVER_SAFE_MARGIN;
        max_logical_height = (work_area.size.height as f64 / scale_factor) - POPOVER_SAFE_MARGIN;
    }

    let logical_width =
        clamp_with_soft_min(POPOVER_BASE_WIDTH, POPOVER_MIN_WIDTH, max_logical_width);
    let logical_height =
        clamp_with_soft_min(POPOVER_BASE_HEIGHT, POPOVER_MIN_HEIGHT, max_logical_height);

    PopoverSize {
        logical_width,
        logical_height,
        physical_width: logical_width * scale_factor,
        physical_height: logical_height * scale_factor,
        physical_offset_y: POPOVER_OFFSET_Y_LOGICAL * scale_factor,
    }
}

fn clamp_popover_position(
    app: &AppHandle,
    anchor_x: f64,
    anchor_y: f64,
    popover_physical_width: f64,
    popover_physical_height: f64,
) -> (i32, i32) {
    let mut x = anchor_x;
    let mut y = anchor_y;

    if let Ok(Some(monitor)) = app.monitor_from_point(anchor_x, anchor_y) {
        let work_area = monitor.work_area();
        let left = work_area.position.x as f64;
        let top = work_area.position.y as f64;
        let right = left + work_area.size.width as f64;
        let bottom = top + work_area.size.height as f64;

        let max_x = (right - popover_physical_width).max(left);
        let max_y = (bottom - popover_physical_height).max(top);

        x = x.clamp(left, max_x);
        y = y.clamp(top, max_y);
    }

    (x.round() as i32, y.round() as i32)
}

fn show_popover_at_tray_rect(app: &AppHandle, window: &WebviewWindow, rect: &Rect) {
    let (tray_x, tray_y, tray_w, tray_h) = rect_values(rect);
    let popover = resolve_popover_size(app, tray_x, tray_y);

    let desired_x = tray_x + (tray_w - popover.physical_width) / 2.0;
    let desired_y = tray_y + tray_h + popover.physical_offset_y;
    let (x, y) = clamp_popover_position(
        app,
        desired_x,
        desired_y,
        popover.physical_width,
        popover.physical_height,
    );

    let _ = window.set_size(Size::Logical(LogicalSize::new(
        popover.logical_width,
        popover.logical_height,
    )));
    let _ = window.set_position(Position::Physical(PhysicalPosition::new(x, y)));
    let _ = window.show();
    let _ = window.set_focus();
}

fn toggle_popover_window(app: &AppHandle, tray_rect: Option<&Rect>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };

    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
        return;
    }

    if let Some(rect) = tray_rect {
        show_popover_at_tray_rect(app, &window, rect);
    } else {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let refresh = MenuItem::with_id(app, "refresh", "Refresh", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&refresh, &separator, &quit])?;

    let mut tray_builder = TrayIconBuilder::with_id("server-ports-tray").menu(&menu);

    if let Some(icon) = app.default_window_icon().cloned() {
        tray_builder = tray_builder.icon(icon).icon_as_template(true);
    }

    tray_builder
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
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
        if suspended {
            return;
        }
        let _ = window.hide();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
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
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_projects,
            add_project,
            update_project,
            remove_project,
            reorder_projects,
            refresh_status,
            open_project_url,
            kill_project_port,
            get_settings,
            set_autostart,
            detect_project_ports,
            hide_main_window,
            set_auto_hide_suspended,
            quit_app,
        ]);

    builder
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
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

    #[test]
    fn parses_first_pid() {
        assert_eq!(parse_first_pid("123\n456\n"), Some(123));
        assert_eq!(parse_first_pid("\n"), None);
        assert_eq!(parse_first_pid("abc\n999\n"), Some(999));
    }

    #[test]
    fn builds_localhost_url() {
        assert_eq!(build_localhost_url(5173), "http://localhost:5173");
    }

    #[test]
    fn validates_project_input() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().to_string_lossy().to_string();

        validate_project_candidate("api", &path, 3000, &[], None).expect("valid input should pass");

        let existing = vec![Project {
            id: "1".to_string(),
            name: "existing".to_string(),
            path: normalize_path(&path).expect("normalize path"),
            port: 3000,
            created_at: now_iso(),
        }];

        let duplicate_port = validate_project_candidate("dup", &path, 3000, &existing, None);
        assert!(duplicate_port.is_err());
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
                created_at: now_iso(),
            },
            Project {
                id: "project-b".to_string(),
                name: "b".to_string(),
                path: second_path.clone(),
                port: 4000,
                created_at: now_iso(),
            },
        ];

        let valid_self_update =
            validate_project_candidate("renamed", &first_path, 3000, &projects, Some("project-a"));
        assert!(valid_self_update.is_ok());

        let duplicate_other_port =
            validate_project_candidate("renamed", &first_path, 4000, &projects, Some("project-a"));
        assert!(duplicate_other_port.is_err());

        let duplicate_other_path =
            validate_project_candidate("renamed", &second_path, 3000, &projects, Some("project-a"));
        assert!(duplicate_other_path.is_err());
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
            "server_ports_missing_{}",
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

        let port = available_port();

        let mut child = Command::new("python3")
            .arg("-m")
            .arg("http.server")
            .arg(port.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn dummy server");

        wait_for_port(port, Duration::from_secs(5));

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

    fn wait_for_port(port: u16, timeout: Duration) {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if detect_listening_pid(port).ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }

        panic!("timeout waiting for port {port} to start listening");
    }
}
