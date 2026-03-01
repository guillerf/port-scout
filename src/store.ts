import { invoke } from '@tauri-apps/api/core';
import { check } from '@tauri-apps/plugin-updater';
import { create } from 'zustand';
import type {
  ActiveTab,
  AddProjectInput,
  KillResult,
  Project,
  ProjectDraft,
  ProjectStatus,
  Settings,
  Toast,
  UpdateProjectInput,
} from './types';

interface AppStore {
  activeTab: ActiveTab;
  projects: Project[];
  projectDrafts: Record<string, ProjectDraft>;
  statusByProject: Record<string, ProjectStatus>;
  settings: Settings | null;
  loading: boolean;
  checkingUpdates: boolean;
  busyByProject: Record<string, boolean>;
  toast: Toast | null;
  setActiveTab: (tab: ActiveTab) => void;
  beginProjectDraft: (projectId: string) => void;
  updateProjectDraft: (projectId: string, patch: Partial<ProjectDraft>) => void;
  cancelProjectDraft: (projectId: string) => void;
  saveProjectDraft: (projectId: string) => Promise<void>;
  hydrate: () => Promise<void>;
  refreshStatus: () => Promise<void>;
  addProject: (input: AddProjectInput) => Promise<void>;
  updateProject: (input: UpdateProjectInput) => Promise<boolean>;
  removeProject: (projectId: string) => Promise<void>;
  openProject: (projectId: string) => Promise<void>;
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

const sortProjects = (projects: Project[]): Project[] =>
  [...projects].sort((left, right) => left.name.localeCompare(right.name));

let toastId = 0;

export const useAppStore = create<AppStore>((set, get) => ({
  activeTab: 'projects',
  projects: [],
  projectDrafts: {},
  statusByProject: {},
  settings: null,
  loading: false,
  checkingUpdates: false,
  busyByProject: {},
  toast: null,

  setActiveTab: (tab) => {
    set({ activeTab: tab });
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

    await get().updateProject({
      id: projectId,
      name: draft.name.trim(),
      path: draft.path.trim(),
      port: parsedPort,
    });
  },

  hydrate: async () => {
    set({ loading: true });
    try {
      const [projects, statuses, settings] = await Promise.all([
        invoke<Project[]>('list_projects'),
        invoke<ProjectStatus[]>('refresh_status'),
        invoke<Settings>('get_settings'),
      ]);

      set({
        projects: sortProjects(projects),
        statusByProject: toStatusMap(statuses),
        settings,
        loading: false,
      });
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

  refreshStatus: async () => {
    try {
      const statuses = await invoke<ProjectStatus[]>('refresh_status');
      set({ statusByProject: toStatusMap(statuses) });
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

  addProject: async (input) => {
    set({ loading: true });
    try {
      const project = await invoke<Project>('add_project', { input });

      set((state) => ({
        projects: sortProjects([...state.projects, project]),
        loading: false,
        toast: {
          id: ++toastId,
          tone: 'success',
          message: `Added ${project.name}`,
        },
      }));

      await get().refreshStatus();
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
          projects: sortProjects(
            state.projects.map((item) => (item.id === project.id ? project : item)),
          ),
          toast: {
            id: ++toastId,
            tone: 'success',
            message: `Updated ${project.name}`,
          },
        };
      });

      await get().refreshStatus();
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

  killProject: async (projectId) => {
    set((state) => ({
      busyByProject: { ...state.busyByProject, [projectId]: true },
    }));

    try {
      const result = await invoke<KillResult>('kill_project_port', { projectId });
      await get().refreshStatus();

      set((state) => {
        const nextBusy = { ...state.busyByProject };
        delete nextBusy[projectId];

        return {
          busyByProject: nextBusy,
          toast: {
            id: ++toastId,
            tone: 'success',
            message: result.terminated
              ? `Stopped PID ${result.attemptedPid ?? 'unknown'}`
              : 'Port was already free',
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
      set({ settings });
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
      set({
        checkingUpdates: false,
        toast: {
          id: ++toastId,
          tone: 'success',
          message: `Update ${update.version} installed. Relaunching...`,
        },
      });
    } catch (error) {
      set({
        checkingUpdates: false,
        toast: {
          id: ++toastId,
          tone: 'error',
          message: errorMessage(error),
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
