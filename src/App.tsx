import { open } from '@tauri-apps/plugin-dialog';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import {
  DndContext,
  KeyboardSensor,
  PointerSensor,
  closestCenter,
  useSensor,
  useSensors,
} from '@dnd-kit/core';
import {
  Check,
  ChevronLeft,
  ExternalLink,
  GitBranch,
  Monitor,
  Moon,
  Play,
  RefreshCw,
  Settings as SettingsIcon,
  Square,
  Sun,
  Trash2,
  Settings2,
  X,
} from 'lucide-react';
import type { DragEndEvent } from '@dnd-kit/core';
import {
  SortableContext,
  arrayMove,
  sortableKeyboardCoordinates,
  useSortable,
  verticalListSortingStrategy,
} from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { useEffect, useRef, useState } from 'react';
import type { CSSProperties, FormEvent, ReactNode } from 'react';
import { useAppStore } from './store';
import type {
  PortCandidate,
  PortDetectionResult,
  PortSource,
  Project,
  ThemeMode,
} from './types';

const REFRESH_MS = 5000;

function relativeTime(iso: string | null): string {
  if (!iso) {
    return 'never';
  }

  const delta = Date.now() - new Date(iso).getTime();
  if (Number.isNaN(delta) || delta < 0) {
    return 'now';
  }

  const minutes = Math.floor(delta / 60000);
  if (minutes < 1) {
    return '<1m';
  }
  if (minutes < 60) {
    return `${minutes}m`;
  }

  const hours = Math.floor(minutes / 60);
  const remainingMinutes = minutes % 60;
  if (hours < 24) {
    return remainingMinutes === 0 ? `${hours}h` : `${hours}h ${remainingMinutes}m`;
  }

  const days = Math.floor(hours / 24);
  return `${days}d`;
}

function sourceLabel(source: PortSource): string {
  switch (source) {
    case 'env':
      return '.env';
    case 'package-script':
      return 'package';
    case 'esbuild-config':
      return 'esbuild';
    case 'vite-config':
      return 'vite';
    case 'docker-compose':
      return 'compose';
  }
}

function sharedPortWarningText(names: string[]): string {
  if (names.length === 1) {
    return `Port is also configured for "${names[0]}".`;
  }
  return `Port is also configured for: ${names.join(', ')}.`;
}

// ---------------------------------------------------------------------------
// SortableProjectCard — read-only card with dnd-kit drag handle
// ---------------------------------------------------------------------------

interface SortableProjectCardProps {
  project: Project;
  busy: boolean;
  confirmRemoveProjectId: string | null;
  beginProjectDraft: (id: string) => void;
  setConfirmRemoveProjectId: (id: string | null) => void;
  handleConfirmRemoveProject: (id: string) => Promise<void>;
}

function SortableProjectCard({
  project,
  busy,
  confirmRemoveProjectId,
  beginProjectDraft,
  setConfirmRemoveProjectId,
  handleConfirmRemoveProject,
}: SortableProjectCardProps) {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: project.id,
  });

  const style: CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
    opacity: isDragging ? 0.5 : undefined,
  };

  return (
    <article
      className="editable-card"
      ref={setNodeRef}
      style={style}
    >
      <button
        className="drag-handle"
        type="button"
        title="Drag to reorder"
        aria-label="Drag to reorder"
        {...attributes}
        {...listeners}
      >
        ⠿
      </button>
      <div className="editable-card-body">
        <div className="editable-card-info">
          <div className="read-only-row">
            <strong>{project.name}</strong>
            <span>{project.path}</span>
            <span>:{project.port}</span>
          </div>
        </div>

        <div className="editable-card-actions">
          <button
            className="ghost-icon-btn"
            type="button"
            title="Edit"
            disabled={busy}
            onClick={() => beginProjectDraft(project.id)}
          >
            <Settings2 size={14} />
          </button>
          {confirmRemoveProjectId === project.id ? (
            <div className="confirm-actions">
              <button
                className="ghost-icon-btn danger"
                type="button"
                title="Confirm remove"
                disabled={busy}
                onClick={() => void handleConfirmRemoveProject(project.id)}
              >
                <Check size={14} />
              </button>
              <button
                className="ghost-icon-btn"
                type="button"
                title="Cancel remove"
                disabled={busy}
                onClick={() => setConfirmRemoveProjectId(null)}
              >
                <X size={14} />
              </button>
            </div>
          ) : (
            <button
              className="ghost-icon-btn danger"
              type="button"
              title="Remove"
              disabled={busy}
              onClick={() => setConfirmRemoveProjectId(project.id)}
            >
              <Trash2 size={14} />
            </button>
          )}
        </div>
      </div>
    </article>
  );
}

// ---------------------------------------------------------------------------
// SortableDraftCard — registers disabled sortable so dnd-kit tracks position
// ---------------------------------------------------------------------------

function SortableDraftCard({ id, children }: { id: string; children: ReactNode }) {
  // disabled: true keeps the card registered with dnd-kit (so collision detection
  // and sorting around it works correctly) without making it draggable.
  const { setNodeRef } = useSortable({ id, disabled: true });
  return (
    <article className="editable-card" ref={setNodeRef}>
      {children}
    </article>
  );
}

// ---------------------------------------------------------------------------
// AddProjectDialog
// ---------------------------------------------------------------------------

interface AddProjectDialogProps {
  open: boolean;
  loading: boolean;
  name: string;
  path: string;
  port: string;
  startCommand: string;
  detectingPort: boolean;
  detectedCandidates: PortCandidate[];
  detectedError: string | null;
  addPortConflictNames: string[];
  onClose: () => void;
  onSubmit: (event: FormEvent<HTMLFormElement>) => void | Promise<void>;
  onBrowsePath: () => Promise<void>;
  onNameChange: (value: string) => void;
  onPathChange: (value: string) => void;
  onPortChange: (value: string) => void;
  onStartCommandChange: (value: string) => void;
  onCandidateSelect: (port: number) => void;
}

function AddProjectDialog({
  open,
  loading,
  name,
  path,
  port,
  startCommand,
  detectingPort,
  detectedCandidates,
  detectedError,
  addPortConflictNames,
  onClose,
  onSubmit,
  onBrowsePath,
  onNameChange,
  onPathChange,
  onPortChange,
  onStartCommandChange,
  onCandidateSelect,
}: AddProjectDialogProps) {
  if (!open) {
    return null;
  }

  return (
    <div
      className="dialog-layer"
      role="presentation"
      onClick={() => {
        if (!loading) {
          onClose();
        }
      }}
    >
      <section
        className="dialog-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="add-project-title"
        onClick={(event) => event.stopPropagation()}
      >
        <header className="dialog-header">
          <div className="dialog-header-copy">
            <h2 id="add-project-title">Add project</h2>
            <p>Track a local app and its port.</p>
          </div>
          <button
            className="ghost-icon-btn"
            type="button"
            title="Close"
            aria-label="Close add project dialog"
            disabled={loading}
            onClick={onClose}
          >
            <X size={14} />
          </button>
        </header>

        <form className="settings-form dialog-form" onSubmit={onSubmit}>
          <label>
            Path
            <div className="path-row">
              <input
                value={path}
                onChange={(event) => onPathChange(event.target.value)}
                placeholder="/Users/me/project"
                required
                autoFocus
              />
              <button type="button" disabled={loading} onClick={() => void onBrowsePath()}>
                Browse
              </button>
            </div>
          </label>

          <label>
            Name
            <input
              value={name}
              onChange={(event) => onNameChange(event.target.value)}
              placeholder="api"
              required
            />
          </label>

          <label>
            Port
            <input
              value={port}
              onChange={(event) => onPortChange(event.target.value)}
              inputMode="numeric"
              placeholder="3000"
              required
            />
          </label>

          <label>
            Start command
            <input
              value={startCommand}
              onChange={(event) => onStartCommandChange(event.target.value)}
              placeholder="npm run dev"
              required
            />
          </label>

          <div className="port-detect-meta">
            {detectingPort ? <p className="detecting-text">Detecting port...</p> : null}
            {!detectingPort && detectedCandidates.length > 0 ? (
              <>
                <p className="detected-title">
                  Detected port: <strong>{detectedCandidates[0].port}</strong>
                </p>
                <div className="candidate-list">
                  {detectedCandidates.map((candidate) => (
                    <button
                      key={`${candidate.source}-${candidate.port}-${candidate.detail}`}
                      className="candidate-chip"
                      type="button"
                      onClick={() => onCandidateSelect(candidate.port)}
                      title={candidate.detail}
                    >
                      {candidate.port} · {sourceLabel(candidate.source)}
                    </button>
                  ))}
                </div>
              </>
            ) : null}
            {!detectingPort && path.trim() && detectedCandidates.length === 0 && !detectedError ? (
              <p className="detecting-text">No port detected, please enter manually.</p>
            ) : null}
            {detectedError ? <p className="detect-error">{detectedError}</p> : null}
            {addPortConflictNames.length > 0 ? (
              <p className="shared-port-warning">{sharedPortWarningText(addPortConflictNames)}</p>
            ) : null}
          </div>

          <div className="dialog-actions">
            <button className="secondary-btn" type="button" disabled={loading} onClick={onClose}>
              Cancel
            </button>
            <button className="primary-btn" type="submit" disabled={loading}>
              Add project
            </button>
          </div>
        </form>
      </section>
    </div>
  );
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

export default function App() {
  const {
    activeTab,
    projects,
    projectDrafts,
    statusByProject,
    settings,
    loading,
    checkingUpdates,
    busyByProject,
    themeMode,
    toast,
    setActiveTab,
    beginProjectDraft,
    updateProjectDraft,
    cancelProjectDraft,
    saveProjectDraft,
    hydrate,
    refreshStatus,
    addProject,
    removeProject,
    reorderProjects,
    openProject,
    startProject,
    killProject,
    setAutostart,
    setThemeMode,
    checkForUpdates,
    hideMainWindow,
    quitApp,
    clearToast,
  } = useAppStore();

  const [isAddProjectDialogOpen, setIsAddProjectDialogOpen] = useState(false);
  const [newName, setNewName] = useState('');
  const [newPath, setNewPath] = useState('');
  const [newPort, setNewPort] = useState('');
  const [newStartCommand, setNewStartCommand] = useState('npm run dev');
  const [detectingPort, setDetectingPort] = useState(false);
  const [detectedCandidates, setDetectedCandidates] = useState<PortCandidate[]>([]);
  const [detectedError, setDetectedError] = useState<string | null>(null);
  const [manualPortEdited, setManualPortEdited] = useState(false);
  const [manualStartCommandEdited, setManualStartCommandEdited] = useState(false);
  const [confirmRemoveProjectId, setConfirmRemoveProjectId] = useState<string | null>(null);

  const detectRequestRef = useRef(0);
  const manualPortEditedRef = useRef(false);
  const manualStartCommandEditedRef = useRef(false);
  const skipNextDebouncedDetectRef = useRef(false);

  function resetAddProjectDialog() {
    detectRequestRef.current += 1;
    skipNextDebouncedDetectRef.current = false;
    setNewName('');
    setNewPath('');
    setNewPort('');
    setNewStartCommand('npm run dev');
    setManualPortEdited(false);
    setManualStartCommandEdited(false);
    setDetectingPort(false);
    setDetectedCandidates([]);
    setDetectedError(null);
  }

  function closeAddProjectDialog() {
    if (loading) {
      return;
    }

    setIsAddProjectDialogOpen(false);
    resetAddProjectDialog();
  }

  // dnd-kit sensors
  const sensors = useSensors(
    useSensor(PointerSensor),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  useEffect(() => {
    void hydrate();

    const timer = window.setInterval(() => {
      void refreshStatus();
    }, REFRESH_MS);

    let unlisten: (() => void) | undefined;
    void listen('refresh-requested', () => {
      void refreshStatus();
    }).then((dispose) => {
      unlisten = dispose;
    });

    return () => {
      clearInterval(timer);
      if (unlisten) {
        unlisten();
      }
    };
  }, [hydrate, refreshStatus]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        if (isAddProjectDialogOpen) {
          closeAddProjectDialog();
          return;
        }

        void hideMainWindow();
      }
    };

    window.addEventListener('keydown', onKeyDown);
    return () => {
      window.removeEventListener('keydown', onKeyDown);
    };
  }, [hideMainWindow, isAddProjectDialogOpen, loading]);

  useEffect(() => {
    manualPortEditedRef.current = manualPortEdited;
  }, [manualPortEdited]);

  useEffect(() => {
    manualStartCommandEditedRef.current = manualStartCommandEdited;
  }, [manualStartCommandEdited]);

  useEffect(() => {
    if (!toast) {
      return;
    }

    const timer = window.setTimeout(() => {
      clearToast();
    }, 2800);

    return () => {
      clearTimeout(timer);
    };
  }, [toast, clearToast]);

  const detectPortsForPath = async (path: string) => {
    const trimmedPath = path.trim();
    if (!trimmedPath) {
      detectRequestRef.current += 1;
      setDetectingPort(false);
      setDetectedCandidates([]);
      setDetectedError(null);
      if (!manualStartCommandEditedRef.current) {
        setNewStartCommand('npm run dev');
      }
      return;
    }

    const requestId = ++detectRequestRef.current;
    setDetectingPort(true);
    setDetectedError(null);
    try {
      const result = await invoke<PortDetectionResult>('detect_project_ports', {
        path: trimmedPath,
      });

      if (requestId !== detectRequestRef.current) {
        return;
      }

      setDetectedCandidates(result.candidates);
      setDetectedError(result.errors.length > 0 ? result.errors[0] : null);
      if (!manualPortEditedRef.current && result.bestPort !== null) {
        setNewPort(String(result.bestPort));
      }
      if (!manualStartCommandEditedRef.current && result.suggestedStartCommand) {
        setNewStartCommand(result.suggestedStartCommand);
      }
    } catch (error) {
      if (requestId !== detectRequestRef.current) {
        return;
      }

      setDetectedCandidates([]);
      const message =
        error && typeof error === 'object' && 'message' in error
          ? String((error as { message: unknown }).message)
          : String(error);
      setDetectedError(message);
    } finally {
      if (requestId === detectRequestRef.current) {
        setDetectingPort(false);
      }
    }
  };

  const setAutoHideSuspended = async (suspended: boolean) => {
    try {
      await invoke('set_auto_hide_suspended', { suspended });
    } catch {
      // Ignore: not critical for add/edit flow correctness.
    }
  };

  const resolveDialogSelection = (selection: string | string[] | null): string | null => {
    if (typeof selection === 'string') {
      return selection;
    }
    if (Array.isArray(selection) && selection.length > 0 && typeof selection[0] === 'string') {
      return selection[0];
    }
    return null;
  };

  useEffect(() => {
    if (!isAddProjectDialogOpen) {
      return;
    }

    const trimmedPath = newPath.trim();
    if (!trimmedPath) {
      detectRequestRef.current += 1;
      setDetectingPort(false);
      setDetectedCandidates([]);
      setDetectedError(null);
      if (!manualStartCommandEditedRef.current) {
        setNewStartCommand('npm run dev');
      }
      return;
    }

    if (skipNextDebouncedDetectRef.current) {
      skipNextDebouncedDetectRef.current = false;
      return;
    }

    const timer = window.setTimeout(() => {
      void detectPortsForPath(trimmedPath);
    }, 350);

    return () => {
      clearTimeout(timer);
    };
  }, [isAddProjectDialogOpen, newPath]);

  const handleBrowseNewPath = async () => {
    await setAutoHideSuspended(true);
    try {
      const selection = await open({
        directory: true,
        multiple: false,
        title: 'Select project folder',
      });

      const selectedPath = resolveDialogSelection(selection);
      if (selectedPath) {
        skipNextDebouncedDetectRef.current = true;
        setNewPath(selectedPath);
        if (!newName.trim()) {
          const parts = selectedPath.split('/');
          setNewName(parts[parts.length - 1] || 'project');
        }
        void detectPortsForPath(selectedPath);
      }
    } finally {
      await setAutoHideSuspended(false);
    }
  };

  const handleBrowseDraftPath = async (projectId: string) => {
    await setAutoHideSuspended(true);
    try {
      const selection = await open({
        directory: true,
        multiple: false,
        title: 'Select project folder',
      });

      const selectedPath = resolveDialogSelection(selection);
      if (selectedPath) {
        updateProjectDraft(projectId, { path: selectedPath });
      }
    } finally {
      await setAutoHideSuspended(false);
    }
  };

  const handleAddProject = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();

    const parsedPort = Number.parseInt(newPort, 10);
    if (!Number.isInteger(parsedPort) || parsedPort < 1 || parsedPort > 65535) {
      return;
    }

    const previousIds = new Set(useAppStore.getState().projects.map((project) => project.id));

    await addProject({
      name: newName.trim(),
      path: newPath.trim(),
      port: parsedPort,
      startCommand: newStartCommand.trim(),
    });

    const nextProjects = useAppStore.getState().projects;
    const wasAdded = nextProjects.some((project) => !previousIds.has(project.id));
    if (wasAdded) {
      setIsAddProjectDialogOpen(false);
      resetAddProjectDialog();
    }
  };

  const handleConfirmRemoveProject = async (projectId: string) => {
    await removeProject(projectId);
    setConfirmRemoveProjectId((current) => (current === projectId ? null : current));
  };

  const handleDragEnd = (event: DragEndEvent) => {
    const { active, over } = event;
    if (!over || active.id === over.id) return;

    const ids = projects.map((p) => p.id);
    const oldIndex = ids.indexOf(active.id as string);
    const newIndex = ids.indexOf(over.id as string);
    if (oldIndex === -1 || newIndex === -1) return;

    const reordered = arrayMove(ids, oldIndex, newIndex);
    void reorderProjects(reordered);
  };

  const parsedNewPort = Number.parseInt(newPort, 10);
  const addPortConflictNames = Number.isInteger(parsedNewPort)
    ? projects.filter((project) => project.port === parsedNewPort).map((project) => project.name)
    : [];

  const renderProjectsTab = () => (
    <section className="view projects-view">
      <header className="view-header" data-tauri-drag-region>
        <h1>Projects</h1>
        <div className="view-actions">
          <button className="icon-btn" onClick={() => void refreshStatus()} type="button" title="Refresh status">
            <RefreshCw size={18} />
          </button>
          <button className="icon-btn" onClick={() => setActiveTab('settings')} type="button" title="Settings">
            <SettingsIcon size={18} />
          </button>
        </div>
      </header>

      <div className="scroll-area">
        {projects.length === 0 ? (
          <p className="empty">No configured projects yet. Open Settings to add your first one.</p>
        ) : (
          projects.map((project) => {
            const status = statusByProject[project.id];
            const runState = status?.runState ?? 'stopped';
            const isRunning = runState === 'owned';
            const busy = !!busyByProject[project.id];
            const ownerName =
              status?.ownerProjectId
                ? projects.find((item) => item.id === status.ownerProjectId)?.name ?? 'another project'
                : null;
            const runStateMessage =
              runState === 'owned-by-other'
                ? `Port active by ${ownerName ?? 'another project'}`
                : runState === 'ambiguous'
                  ? 'Port active (owner unclear)'
                  : null;
            return (
              <article className="project-card" key={project.id}>
                <div className="project-row-top">
                  <div className="project-heading">
                    <span className={isRunning ? 'status-dot running' : 'status-dot stopped'} />
                    <h2>{project.name}</h2>
                  </div>

                  {runState === 'owned' || runState === 'stopped' ? (
                    <div className="project-actions">
                      {runState === 'owned' ? (
                        <>
                          <button
                            className="danger-pill"
                            type="button"
                            title="Stop project"
                            disabled={busy}
                            onClick={() => void killProject(project.id)}
                          >
                            <Square size={12} fill="currentColor" />
                          </button>
                          <button
                            className="neutral-pill"
                            type="button"
                            title="Open project"
                            disabled={busy}
                            onClick={() => void openProject(project.id)}
                          >
                            <ExternalLink size={12} />
                          </button>
                        </>
                      ) : (
                        <button
                          className="neutral-pill"
                          type="button"
                          title="Start localhost server"
                          aria-label="Start localhost server"
                          disabled={busy}
                          onClick={() => void startProject(project.id)}
                        >
                          <Play size={12} fill="currentColor" />
                        </button>
                      )}
                    </div>
                  ) : null}
                </div>

                <div className="project-meta">
                  <span className="meta-left">
                    <span className="branch-icon"><GitBranch size={12} /></span>
                    {status?.branch ?? 'loading'}
                    <span className="port-label">:{project.port}</span>
                  </span>
                  <span className="meta-right">{relativeTime(status?.lastRunningAt ?? null)}</span>
                </div>

                {runStateMessage ? <p className="status-note">{runStateMessage}</p> : null}
                {status?.error ? <p className="error-text">{status.error}</p> : null}
              </article>
            );
          })
        )}
      </div>
    </section>
  );

  const renderSettingsTab = () => (
    <section className="view settings-view">
      <header className="view-header" data-tauri-drag-region>
        <button className="icon-btn back-btn" onClick={() => setActiveTab('projects')} type="button" title="Back to projects">
          <ChevronLeft size={20} />
        </button>
        <h1>Settings</h1>
        <div className="view-actions" />
      </header>

      <div className="scroll-area settings-scroll">
        <section className="settings-section">
          <div className="settings-section-header">
            <div className="settings-section-copy">
              <h2>Projects</h2>
              <p>Reorder, edit, and add tracked apps.</p>
            </div>
            <button
              className="section-action-btn"
              type="button"
              disabled={loading}
              onClick={() => setIsAddProjectDialogOpen(true)}
            >
              Add project
            </button>
          </div>

          {projects.length === 0 ? (
            <p className="empty small">No projects configured.</p>
          ) : (
            <DndContext
              sensors={sensors}
              collisionDetection={closestCenter}
              onDragEnd={handleDragEnd}
            >
              <SortableContext
                items={projects.map((p) => p.id)}
                strategy={verticalListSortingStrategy}
              >
                <div className="editable-list">
                  {projects.map((project) => {
                    const draft = projectDrafts[project.id];
                    const busy = !!busyByProject[project.id];
                    const parsedDraftPort = draft ? Number.parseInt(draft.port, 10) : NaN;
                    const draftPortConflictNames =
                      draft && Number.isInteger(parsedDraftPort)
                        ? projects
                            .filter((candidate) => candidate.id !== project.id && candidate.port === parsedDraftPort)
                            .map((candidate) => candidate.name)
                        : [];

                    if (draft) {
                      return (
                        <SortableDraftCard id={project.id} key={project.id}>
                          <div className="editable-card-info">
                            <label>
                              Name
                              <input
                                value={draft.name}
                                onChange={(event) =>
                                  updateProjectDraft(project.id, { name: event.target.value })
                                }
                                disabled={busy}
                              />
                            </label>

                            <label>
                              Path
                              <div className="path-row">
                                <input
                                  value={draft.path}
                                  onChange={(event) =>
                                    updateProjectDraft(project.id, { path: event.target.value })
                                  }
                                  disabled={busy}
                                />
                                <button
                                  type="button"
                                  disabled={busy}
                                  onClick={() => void handleBrowseDraftPath(project.id)}
                                >
                                  Browse
                                </button>
                              </div>
                            </label>

                            <label>
                              Port
                              <input
                                value={draft.port}
                                onChange={(event) =>
                                  updateProjectDraft(project.id, { port: event.target.value })
                                }
                                inputMode="numeric"
                                disabled={busy}
                              />
                            </label>

                            <label>
                              Start command
                              <input
                                value={draft.startCommand}
                                onChange={(event) =>
                                  updateProjectDraft(project.id, { startCommand: event.target.value })
                                }
                                placeholder="npm run dev"
                                disabled={busy}
                              />
                            </label>

                            {draftPortConflictNames.length > 0 ? (
                              <p className="shared-port-warning">
                                {sharedPortWarningText(draftPortConflictNames)}
                              </p>
                            ) : null}

                            <div className="inline-actions">
                              <button
                                className="primary-btn"
                                type="button"
                                disabled={busy}
                                onClick={() => void saveProjectDraft(project.id)}
                              >
                                Save
                              </button>
                              <button
                                className="secondary-btn"
                                type="button"
                                disabled={busy}
                                onClick={() => cancelProjectDraft(project.id)}
                              >
                                Cancel
                              </button>
                              {confirmRemoveProjectId === project.id ? (
                                <>
                                  <button
                                    className="link-btn danger"
                                    type="button"
                                    disabled={busy}
                                    onClick={() => void handleConfirmRemoveProject(project.id)}
                                  >
                                    Confirm remove
                                  </button>
                                  <button
                                    className="link-btn"
                                    type="button"
                                    disabled={busy}
                                    onClick={() => setConfirmRemoveProjectId(null)}
                                  >
                                    Cancel remove
                                  </button>
                                </>
                              ) : (
                                <button
                                  className="link-btn danger"
                                  type="button"
                                  disabled={busy}
                                  onClick={() => setConfirmRemoveProjectId(project.id)}
                                >
                                  Remove
                                </button>
                              )}
                            </div>
                          </div>
                        </SortableDraftCard>
                      );
                    }

                    return (
                      <SortableProjectCard
                        key={project.id}
                        project={project}
                        busy={busy}
                        confirmRemoveProjectId={confirmRemoveProjectId}
                        beginProjectDraft={beginProjectDraft}
                        setConfirmRemoveProjectId={setConfirmRemoveProjectId}
                        handleConfirmRemoveProject={handleConfirmRemoveProject}
                      />
                    );
                  })}
                </div>
              </SortableContext>
            </DndContext>
          )}
        </section>

        <section className="settings-section">
          <div className="settings-section-header">
            <div className="settings-section-copy">
              <h2>Application</h2>
              <p>Appearance, startup, and update controls.</p>
            </div>
          </div>

          <div className="settings-group">
            <div className="settings-row">
              <div className="settings-row-copy">
                <h3>Appearance</h3>
                <p>Choose how Port Scout looks.</p>
              </div>
              <div className="theme-toggle">
                {([
                  { mode: 'light', icon: <Sun size={14} />, label: 'Light' },
                  { mode: 'dark', icon: <Moon size={14} />, label: 'Dark' },
                  { mode: 'system', icon: <Monitor size={14} />, label: 'System' },
                ] as { mode: ThemeMode; icon: ReactNode; label: string }[]).map(({ mode, icon, label }) => (
                  <button
                    key={mode}
                    type="button"
                    title={label}
                    className={themeMode === mode ? 'active' : ''}
                    onClick={() => setThemeMode(mode)}
                  >
                    {icon}
                  </button>
                ))}
              </div>
            </div>

            <label className="settings-row settings-row-interactive">
              <div className="settings-row-copy">
                <h3>Start at login</h3>
                <p>Launch Port Scout when you sign in.</p>
              </div>
              <input
                type="checkbox"
                checked={settings?.autostartEnabled ?? false}
                onChange={(event) => void setAutostart(event.target.checked)}
              />
            </label>

            <div className="settings-row settings-row-stack">
              <div className="settings-row-copy">
                <h3>Updates</h3>
                <p>Check for a newer version manually.</p>
              </div>
              <button
                className="action-btn"
                type="button"
                onClick={() => void checkForUpdates()}
                disabled={checkingUpdates}
              >
                {checkingUpdates ? 'Checking updates...' : 'Check updates'}
              </button>
            </div>
          </div>

          <div className="settings-danger-zone">
            <button
              className="action-btn action-btn-quit"
              type="button"
              onClick={() => void quitApp()}
            >
              Quit
            </button>
          </div>
        </section>
      </div>
    </section>
  );

  return (
    <main className={`utility-shell${isAddProjectDialogOpen ? ' dialog-open' : ''}`}>
      <section className="utility-panel">
        <div className="content-panel">
          {activeTab === 'projects' ? renderProjectsTab() : renderSettingsTab()}
        </div>
      </section>

      <AddProjectDialog
        open={isAddProjectDialogOpen}
        loading={loading}
        name={newName}
        path={newPath}
        port={newPort}
        startCommand={newStartCommand}
        detectingPort={detectingPort}
        detectedCandidates={detectedCandidates}
        detectedError={detectedError}
        addPortConflictNames={addPortConflictNames}
        onClose={closeAddProjectDialog}
        onSubmit={handleAddProject}
        onBrowsePath={handleBrowseNewPath}
        onNameChange={setNewName}
        onPathChange={setNewPath}
        onPortChange={(value) => {
          setManualPortEdited(true);
          setNewPort(value);
        }}
        onStartCommandChange={(value) => {
          setManualStartCommandEdited(true);
          setNewStartCommand(value);
        }}
        onCandidateSelect={(portNumber) => {
          setManualPortEdited(true);
          setNewPort(String(portNumber));
        }}
      />

      {toast ? <aside className={`toast toast-${toast.tone}`}>{toast.message}</aside> : null}
    </main>
  );
}
