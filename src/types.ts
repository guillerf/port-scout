export type ActiveTab = 'projects' | 'settings';

export type ThemeMode = 'light' | 'dark' | 'system';

export interface Project {
  id: string;
  name: string;
  path: string;
  port: number;
  startCommand: string;
  createdAt: string;
}

export interface AddProjectInput {
  name: string;
  path: string;
  port: number;
  startCommand: string;
}

export type PortSource =
  | 'env'
  | 'package-script'
  | 'esbuild-config'
  | 'vite-config'
  | 'docker-compose';

export interface PortCandidate {
  port: number;
  source: PortSource;
  detail: string;
  confidence: number;
}

export interface PortDetectionResult {
  bestPort: number | null;
  candidates: PortCandidate[];
  errors: string[];
  suggestedStartCommand: string | null;
}

export interface UpdateProjectInput {
  id: string;
  name: string;
  path: string;
  port: number;
  startCommand: string;
}

export interface ProjectDraft {
  name: string;
  path: string;
  port: string;
  startCommand: string;
}

export interface ProjectStatus {
  projectId: string;
  branch: string;
  isRunning: boolean;
  pid: number | null;
  portActive: boolean;
  runState: 'stopped' | 'owned' | 'owned-by-other' | 'ambiguous';
  ownerProjectId: string | null;
  lastRunningAt: string | null;
  checkedAt: string;
  error: string | null;
}

export interface KillResult {
  projectId: string;
  attemptedPid: number | null;
  terminated: boolean;
  signalUsed: string;
  blockedReason: 'not-running' | 'owned-by-other' | 'ambiguous' | null;
}

export interface StartResult {
  projectId: string;
  attemptedCommand: string;
  launched: boolean;
  spawnedPid: number | null;
  blockedReason: 'already-running' | 'owned-by-other' | 'ambiguous' | null;
}

export interface Settings {
  autostartEnabled: boolean;
}

export interface Toast {
  id: number;
  tone: 'error' | 'success';
  message: string;
}
