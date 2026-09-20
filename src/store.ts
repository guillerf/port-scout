import { invoke } from '@tauri-apps/api/core';
import { confirm } from '@tauri-apps/plugin-dialog';
import { relaunch } from '@tauri-apps/plugin-process';
import { check } from '@tauri-apps/plugin-updater';
import { create } from 'zustand';
import type {
  ActiveTab,
  AddProjectInput,
  DiscoveredListener,
  DiscoverySettings,
  KillResult,
  ListenerDiscoveryResult,
  Project,
  ProjectDraft,
  ProjectStatus,
  Settings,
  StartResult,
  ThemeMode,
  Toast,
  UpdateProjectInput,
} from './types';

const THEME_KEY = 'theme';

function readStoredTheme(): ThemeMode {
  try {
    const stored = localStorage.getItem(THEME_KEY);
    if (stored === 'light' || stored === 'dark' || stored === 'system') {
      return stored;
    }
  } catch {
    // localStorage unavailable (SSR or storage access blocked)
  }
  return 'system';
}

function applyThemeToDOM(mode: ThemeMode): void {
  if (typeof document === 'undefined') return;
  if (mode === 'system') {
    document.documentElement.removeAttribute('data-theme');
  } else {
    document.documentElement.setAttribute('data-theme', mode);
  }
}

interface AppStore {
  activeTab: ActiveTab;
  themeMode: ThemeMode;
  projects: Project[];
  projectDrafts: Record<string, ProjectDraft>;
  statusByProject: Record<string, ProjectStatus>;
  discoveredListeners: DiscoveredListener[];
  discoveryWarnings: string[];
  discoveryError: string | null;
  discoveryCheckedAt: string | null;
  discoverySettings: DiscoverySettings | null;
  discoverySettingsError: string | null;
  discoverySettingsLoading: boolean;
  discoveryFoldersBusy: boolean;
  pendingDiscoveryFolders: string[] | null;
  projectsError: string | null;
  statusError: string | null;
  settingsError: string | null;
  settings: Settings | null;
  loading: boolean;
  refreshing: boolean;
  checkingUpdates: boolean;
  busyByProject: Record<string, boolean>;
  toast: Toast | null;
  setActiveTab: (tab: ActiveTab) => void;
  setThemeMode: (mode: ThemeMode) => void;
  beginProjectDraft: (projectId: string) => void;
  updateProjectDraft: (projectId: string, patch: Partial<ProjectDraft>) => void;
  cancelProjectDraft: (projectId: string) => void;
  saveProjectDraft: (projectId: string) => Promise<void>;
  hydrate: () => Promise<void>;
  refreshStatus: (force?: boolean) => Promise<void>;
  loadDiscoverySettings: () => Promise<void>;
  setDiscoveryFolders: (folders: string[]) => Promise<boolean>;
  addProject: (input: AddProjectInput) => Promise<void>;
  updateProject: (input: UpdateProjectInput) => Promise<boolean>;
  removeProject: (projectId: string) => Promise<void>;
  reorderProjects: (projectIds: string[]) => Promise<void>;
  openProject: (projectId: string) => Promise<void>;
  startProject: (projectId: string) => Promise<StartResult | null>;
  killProject: (projectId: string) => Promise<KillResult | null>;
  setAutostart: (enabled: boolean) => Promise<void>;
  checkForUpdates: () => Promise<void>;
  hideMainWindow: () => Promise<void>;
  quitApp: () => Promise<void>;
  clearToast: () => void;
}

const toStatusMap = (items: ProjectStatus[]): Record<string, ProjectStatus> => {
  const map: Record<string, ProjectStatus> = {};
  for (const item of items) {
    map[item.projectId] = item;
  }
  return map;
};

const errorMessage = (error: unknown): string => {
  if (typeof error === 'string') {
    return error;
  }
  if (error && typeof error === 'object' && 'message' in error) {
    const message = (error as { message: string }).message;
    if (typeof message === 'string' && message.length > 0) {
      return message;
    }
  }
  return 'Unexpected error';
};

const updaterErrorMessage = (error: unknown): string => {
  const message = errorMessage(error);
  const lower = message.toLowerCase();

  if (
    lower.includes('permission') ||
    lower.includes('not allowed') ||
    lower.includes('forbidden') ||
    lower.includes('denied')
  ) {
    return 'Updater permission denied. Ensure `updater:default` and `process:default` are granted.';
  }

  if (
    lower.includes('signature') ||
    lower.includes('verify') ||
    lower.includes('verification') ||
    lower.includes('minisign') ||
    lower.includes('pubkey') ||
    lower.includes('public key')
  ) {
    return 'Update signature verification failed. Check updater public key and signed artifacts.';
  }

  if (
    lower.includes('network') ||
    lower.includes('timed out') ||
    lower.includes('timeout') ||
    lower.includes('failed to fetch') ||
    lower.includes('404') ||
    lower.includes('no such host') ||
    lower.includes('connection')
  ) {
    return 'Update endpoint is unreachable. Verify updater URL and release assets.';
  }

  return message;
};

let toastId = 0;
let reorderSeq = 0;
let hydrateInFlight: Promise<void> | null = null;
let refreshInFlight: Promise<void> | null = null;
let refreshQueued: Promise<void> | null = null;
let discoverySettingsInFlight: Promise<void> | null = null;
let discoveryGeneration = 0;

const listenerKey = (listener: DiscoveredListener): string => (
  JSON.stringify([listener.pid, listener.port, listener.path])
);

function newestListenersFirst(previous: DiscoveredListener[], incoming: DiscoveredListener[]): DiscoveredListener[] {
  const previousKeys = new Set(previous.map(listenerKey));
  const incomingByKey = new Map(incoming.map((listener) => [listenerKey(listener), listener]));
  const added = [...incomingByKey.values()].filter((listener) => !previousKeys.has(listenerKey(listener)));
  const retained = previous.flatMap((listener) => {
    const current = incomingByKey.get(listenerKey(listener));
    return current ? [current] : [];
  });
  return [...added, ...retained];
}

// Apply stored theme immediately on module load (before first render)
const initialTheme = readStoredTheme();
applyThemeToDOM(initialTheme);

export const useAppStore = create<AppStore>((set, get) => ({
  activeTab: 'projects',
  themeMode: initialTheme,
  projects: [],
  projectDrafts: {},
  statusByProject: {},
  discoveredListeners: [],
  discoveryWarnings: [],
  discoveryError: null,
  discoveryCheckedAt: null,
  discoverySettings: null,
  discoverySettingsError: null,
  discoverySettingsLoading: false,
  discoveryFoldersBusy: false,
  pendingDiscoveryFolders: null,
  projectsError: null,
  statusError: null,
  settingsError: null,
  settings: null,
  loading: false,
  refreshing: false,
  checkingUpdates: false,
  busyByProject: {},
  toast: null,

  setActiveTab: (tab) => {
    set({ activeTab: tab });
  },

  setThemeMode: (mode) => {
    try {
      localStorage.setItem(THEME_KEY, mode);
    } catch {
      // localStorage unavailable (SSR or storage access blocked)
    }
    applyThemeToDOM(mode);
    set({ themeMode: mode });
  },

  beginProjectDraft: (projectId) => {
    const project = get().projects.find((item) => item.id === projectId);
    if (!project) {
      return;
    }

    set((state) => ({
      projectDrafts: {
        ...state.projectDrafts,
        [projectId]: {
          name: project.name,
          path: project.path,
          port: String(project.port),
          startCommand: project.startCommand,
        },
      },
    }));
  },

  updateProjectDraft: (projectId, patch) => {
    set((state) => {
      const existing = state.projectDrafts[projectId];
      if (!existing) {
        return state;
      }

      return {
        projectDrafts: {
          ...state.projectDrafts,
          [projectId]: {
            ...existing,
            ...patch,
          },
        },
      };
    });
  },

  cancelProjectDraft: (projectId) => {
    set((state) => {
      const next = { ...state.projectDrafts };
      delete next[projectId];
      return { projectDrafts: next };
    });
  },

  saveProjectDraft: async (projectId) => {
    const draft = get().projectDrafts[projectId];
    if (!draft) {
      return;
    }

    const parsedPort = Number.parseInt(draft.port, 10);
    if (!Number.isInteger(parsedPort) || parsedPort < 1 || parsedPort > 65535) {
      set({
        toast: {
          id: ++toastId,
          tone: 'error',
          message: 'Port must be in the range 1..65535',
        },
      });
      return;
    }

    const trimmedStartCommand = draft.startCommand.trim();
    if (!trimmedStartCommand) {
      set({
        toast: {
          id: ++toastId,
          tone: 'error',
          message: 'Start command cannot be empty',
        },
      });
      return;
    }

    await get().updateProject({
      id: projectId,
      name: draft.name.trim(),
      path: draft.path.trim(),
      port: parsedPort,
      startCommand: trimmedStartCommand,
    });
  },

  hydrate: async () => {
    if (hydrateInFlight) {
      return hydrateInFlight;
    }

    set({ loading: true });
    // Commit each result independently: a login-item or status failure must not
    // prevent saved projects from appearing, even while discovery is still busy.
    hydrateInFlight = Promise.all([
      invoke<Project[]>('list_projects')
        .then((projects) => set({ projects, projectsError: null }))
        .catch((error: unknown) => set({ projectsError: errorMessage(error) })),
      invoke<Settings>('get_settings')
        .then((settings) => set({ settings, settingsError: null }))
        .catch((error: unknown) => set({ settingsError: errorMessage(error) })),
      get().loadDiscoverySettings(),
    ]).then(() => undefined).finally(() => {
      hydrateInFlight = null;
      set({ loading: false });
    });
    return hydrateInFlight;
  },

  refreshStatus: async (force = false) => {
    // Polling, tray events and button clicks share one request. A slow system
    // scan must not build up overlapping lsof processes or overwrite newer data.
    if (refreshInFlight) {
      if (force) {
        // Mutations need a scan started after they finish. Share one queued
        // scan until it begins; later mutations can queue the next scan.
        if (!refreshQueued) {
          refreshQueued = refreshInFlight.then(() => {
            refreshQueued = null;
            return get().refreshStatus();
          });
        }
        return refreshQueued;
      }
      return refreshInFlight;
    }

    const generation = discoveryGeneration;
    const canDiscover = !!get().discoverySettings?.folders.length && !get().discoveryFoldersBusy;
    set({ refreshing: true });
    refreshInFlight = Promise.all([
      invoke<ProjectStatus[]>('refresh_status')
        .then((statuses) => set({ statusByProject: toStatusMap(statuses), statusError: null }))
        .catch((error: unknown) => set({ statusError: errorMessage(error) })),
      canDiscover ? invoke<ListenerDiscoveryResult>('discover_listeners')
        .then((result) => {
          if (generation !== discoveryGeneration) return;
          set((state) => ({
            discoveredListeners: newestListenersFirst(state.discoveredListeners, result.listeners),
            discoveryWarnings: result.warnings,
            discoveryError: null,
            discoveryCheckedAt: new Date().toISOString(),
          }));
        })
        .catch((error: unknown) => {
          if (generation === discoveryGeneration) set({ discoveryError: errorMessage(error) });
        }) : Promise.resolve(),
    ]).then(() => undefined).finally(() => {
      refreshInFlight = null;
      set({ refreshing: false });
    });
    return refreshInFlight;
  },

  loadDiscoverySettings: async () => {
    if (get().discoveryFoldersBusy) return;
    if (discoverySettingsInFlight) return discoverySettingsInFlight;

    const generation = discoveryGeneration;
    set({ discoverySettingsLoading: true });
    discoverySettingsInFlight = invoke<DiscoverySettings>('get_discovery_settings')
      .then((discoverySettings) => {
        if (generation !== discoveryGeneration) return;
        // A reload can observe externally changed folders, so invalidate any
        // scan that began with the previous filter before committing settings.
        discoveryGeneration += 1;
        set({
          discoverySettings,
          discoverySettingsError: null,
          pendingDiscoveryFolders: null,
          discoveredListeners: [],
          discoveryWarnings: [],
          discoveryError: null,
          discoveryCheckedAt: null,
        });
      })
      .catch((error: unknown) => {
        if (generation === discoveryGeneration) set({ discoverySettingsError: errorMessage(error) });
      })
      .finally(() => {
        discoverySettingsInFlight = null;
        set({ discoverySettingsLoading: false });
      });
    await discoverySettingsInFlight;
    await get().refreshStatus(true);
  },

  setDiscoveryFolders: async (folders) => {
    if (get().discoveryFoldersBusy || get().discoverySettingsLoading) return false;
    discoveryGeneration += 1;
    set({
      discoveryFoldersBusy: true,
      discoverySettingsError: null,
      pendingDiscoveryFolders: [...folders],
      discoveredListeners: [],
      discoveryWarnings: [],
      discoveryError: null,
      discoveryCheckedAt: null,
    });

    let saved = false;
    try {
      const discoverySettings = await invoke<DiscoverySettings>('set_discovery_folders', { folders });
      set({ discoverySettings, pendingDiscoveryFolders: null });
      saved = true;
    } catch (error) {
      set({ discoverySettingsError: errorMessage(error) });
    } finally {
      // Invalidate scans started while persistence was pending, then request
      // one after the mutation so the previous folder filter cannot reappear.
      discoveryGeneration += 1;
      set({ discoveryFoldersBusy: false });
    }
    await get().refreshStatus(true);
    return saved;
  },

  addProject: async (input) => {
    set({ loading: true });
    try {
      const project = await invoke<Project>('add_project', { input });

      set((state) => ({
        projects: [...state.projects, project],
        loading: false,
        toast: {
          id: ++toastId,
          tone: 'success',
          message: `Added ${project.name}`,
        },
      }));

      await get().refreshStatus(true);
    } catch (error) {
      set({
        loading: false,
        toast: {
          id: ++toastId,
          tone: 'error',
          message: errorMessage(error),
        },
      });
    }
  },

  updateProject: async (input) => {
    set((state) => ({
      busyByProject: { ...state.busyByProject, [input.id]: true },
    }));

    try {
      const project = await invoke<Project>('update_project', { input });
      set((state) => {
        const nextBusy = { ...state.busyByProject };
        delete nextBusy[input.id];

        const nextDrafts = { ...state.projectDrafts };
        delete nextDrafts[input.id];

        return {
          busyByProject: nextBusy,
          projectDrafts: nextDrafts,
          projects: state.projects.map((item) => (item.id === project.id ? project : item)),
          toast: {
            id: ++toastId,
            tone: 'success',
            message: `Updated ${project.name}`,
          },
        };
      });

      await get().refreshStatus(true);
      return true;
    } catch (error) {
      set((state) => ({
        busyByProject: {
          ...state.busyByProject,
          [input.id]: false,
        },
        toast: {
          id: ++toastId,
          tone: 'error',
          message: errorMessage(error),
        },
      }));
      return false;
    }
  },

  removeProject: async (projectId) => {
    set((state) => ({
      busyByProject: { ...state.busyByProject, [projectId]: true },
    }));

    try {
      await invoke('remove_project', { projectId });
      set((state) => {
        const nextBusy = { ...state.busyByProject };
        delete nextBusy[projectId];

        const nextStatus = { ...state.statusByProject };
        delete nextStatus[projectId];

        const nextDrafts = { ...state.projectDrafts };
        delete nextDrafts[projectId];

        return {
          projects: state.projects.filter((project) => project.id !== projectId),
          statusByProject: nextStatus,
          projectDrafts: nextDrafts,
          busyByProject: nextBusy,
          toast: {
            id: ++toastId,
            tone: 'success',
            message: 'Project removed',
          },
        };
      });

      await get().refreshStatus(true);
    } catch (error) {
      set((state) => ({
        busyByProject: {
          ...state.busyByProject,
          [projectId]: false,
        },
        toast: {
          id: ++toastId,
          tone: 'error',
          message: errorMessage(error),
        },
      }));
    }
  },

  reorderProjects: async (projectIds) => {
    // Capture current order for rollback on failure
    const previousProjects = get().projects;

    // Optimistic update using Map/Set for O(n)
    set((state) => {
      const projectById = new Map(state.projects.map((p) => [p.id, p] as const));
      const idsSet = new Set(projectIds);
      const reordered: Project[] = [];
      for (const id of projectIds) {
        const p = projectById.get(id);
        if (p) reordered.push(p);
      }
      for (const p of state.projects) {
        if (!idsSet.has(p.id)) reordered.push(p);
      }
      return { projects: reordered };
    });

    const seq = ++reorderSeq;

    try {
      const projects = await invoke<Project[]>('reorder_projects', { projectIds });
      if (seq === reorderSeq) {
        set({ projects });
      }
    } catch (error) {
      if (seq === reorderSeq) {
        set({
          projects: previousProjects,
          toast: {
            id: ++toastId,
            tone: 'error',
            message: errorMessage(error),
          },
        });
      }
    }
  },

  openProject: async (projectId) => {
    set((state) => ({
      busyByProject: { ...state.busyByProject, [projectId]: true },
    }));

    try {
      await invoke('open_project_url', { projectId });
      set((state) => {
        const nextBusy = { ...state.busyByProject };
        delete nextBusy[projectId];
        return { busyByProject: nextBusy };
      });
    } catch (error) {
      set((state) => ({
        busyByProject: {
          ...state.busyByProject,
          [projectId]: false,
        },
        toast: {
          id: ++toastId,
          tone: 'error',
          message: errorMessage(error),
        },
      }));
    }
  },

  startProject: async (projectId) => {
    set((state) => ({
      busyByProject: { ...state.busyByProject, [projectId]: true },
    }));

    try {
      const result = await invoke<StartResult>('start_project_server', { projectId });
      await get().refreshStatus(true);

      const blockedMessage =
        result.blockedReason === 'already-running'
          ? 'Project is already running'
          : result.blockedReason === 'owned-by-other'
            ? 'Cannot start: port is owned by another configured project'
            : result.blockedReason === 'ambiguous'
              ? 'Cannot start: port owner is ambiguous'
              : null;
      const launchErrorMessage = !result.launched ? 'Start command was not launched' : null;

      set((state) => {
        const nextBusy = { ...state.busyByProject };
        delete nextBusy[projectId];
        const message = blockedMessage ?? launchErrorMessage;

        if (!message) {
          return { busyByProject: nextBusy };
        }

        return {
          busyByProject: nextBusy,
          toast: {
            id: ++toastId,
            tone: 'error',
            message,
          },
        };
      });

      return result;
    } catch (error) {
      set((state) => ({
        busyByProject: {
          ...state.busyByProject,
          [projectId]: false,
        },
        toast: {
          id: ++toastId,
          tone: 'error',
          message: errorMessage(error),
        },
      }));
      return null;
    }
  },

  killProject: async (projectId) => {
    set((state) => ({
      busyByProject: { ...state.busyByProject, [projectId]: true },
    }));

    try {
      const result = await invoke<KillResult>('kill_project_port', { projectId });
      await get().refreshStatus(true);

      const blockedMessage =
        result.blockedReason === 'not-running'
          ? 'Project is not running'
          : result.blockedReason === 'owned-by-other'
            ? 'Cannot stop: port is owned by another configured project'
            : result.blockedReason === 'ambiguous'
              ? 'Cannot stop: port owner is ambiguous'
              : null;

      set((state) => {
        const nextBusy = { ...state.busyByProject };
        delete nextBusy[projectId];
        const message =
          blockedMessage ??
          (result.terminated ? null : 'Stop signal sent but process is still alive');

        if (!message) {
          return { busyByProject: nextBusy };
        }

        return {
          busyByProject: nextBusy,
          toast: {
            id: ++toastId,
            tone: 'error',
            message,
          },
        };
      });

      return result;
    } catch (error) {
      set((state) => ({
        busyByProject: {
          ...state.busyByProject,
          [projectId]: false,
        },
        toast: {
          id: ++toastId,
          tone: 'error',
          message: errorMessage(error),
        },
      }));
      return null;
    }
  },

  setAutostart: async (enabled) => {
    try {
      const settings = await invoke<Settings>('set_autostart', { enabled });
      set({ settings, settingsError: null });
    } catch (error) {
      set({
        toast: {
          id: ++toastId,
          tone: 'error',
          message: errorMessage(error),
        },
      });
    }
  },

  checkForUpdates: async () => {
    set({ checkingUpdates: true });
    try {
      const update = await check();
      if (!update) {
        set({
          checkingUpdates: false,
          toast: {
            id: ++toastId,
            tone: 'success',
            message: 'Already on the latest version',
          },
        });
        return;
      }

      await update.downloadAndInstall();
      set({ checkingUpdates: false });

      const shouldRelaunch = await confirm(
        `Update ${update.version} installed. Restart Port Scout now to apply it?`,
        {
          title: 'Port Scout',
          kind: 'info',
          okLabel: 'Restart now',
          cancelLabel: 'Later',
        },
      );

      if (shouldRelaunch) {
        await relaunch();
        return;
      }

      set({
        toast: {
          id: ++toastId,
          tone: 'success',
          message: `Update ${update.version} installed. Restart Port Scout later to finish update.`,
        },
      });
    } catch (error) {
      set({
        checkingUpdates: false,
        toast: {
          id: ++toastId,
          tone: 'error',
          message: updaterErrorMessage(error),
        },
      });
    }
  },

  hideMainWindow: async () => {
    await invoke('hide_main_window');
  },

  quitApp: async () => {
    await invoke('quit_app');
  },

  clearToast: () => {
    set({ toast: null });
  },
}));
