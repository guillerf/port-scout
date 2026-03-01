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
        <div className="read-only-row">
          <strong>{project.name}</strong>
          <span>{project.path}</span>
          <span>:{project.port}</span>
        </div>

        <div className="inline-actions">
          <button
            className="secondary-btn"
            type="button"
            disabled={busy}
            onClick={() => beginProjectDraft(project.id)}
          >
            Edit
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
    killProject,
    setAutostart,
    checkForUpdates,
    hideMainWindow,
    quitApp,
    clearToast,
  } = useAppStore();

  const [newName, setNewName] = useState('');
  const [newPath, setNewPath] = useState('');
  const [newPort, setNewPort] = useState('');
  const [detectingPort, setDetectingPort] = useState(false);
  const [detectedCandidates, setDetectedCandidates] = useState<PortCandidate[]>([]);
  const [detectedError, setDetectedError] = useState<string | null>(null);
  const [manualPortEdited, setManualPortEdited] = useState(false);
  const [confirmRemoveProjectId, setConfirmRemoveProjectId] = useState<string | null>(null);

  const detectRequestRef = useRef(0);
  const manualPortEditedRef = useRef(false);
  const skipNextDebouncedDetectRef = useRef(false);

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
        void hideMainWindow();
      }
    };

    window.addEventListener('keydown', onKeyDown);
    return () => {
      window.removeEventListener('keydown', onKeyDown);
    };
  }, [hideMainWindow]);

  useEffect(() => {
    manualPortEditedRef.current = manualPortEdited;
  }, [manualPortEdited]);

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
    const trimmedPath = newPath.trim();
    if (!trimmedPath) {
      detectRequestRef.current += 1;
      setDetectingPort(false);
      setDetectedCandidates([]);
      setDetectedError(null);
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
  }, [newPath]);

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

    await addProject({
      name: newName.trim(),
      path: newPath.trim(),
      port: parsedPort,
    });

    setNewName('');
    setNewPath('');
    setNewPort('');
    setManualPortEdited(false);
    setDetectingPort(false);
    setDetectedCandidates([]);
    setDetectedError(null);
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

  const renderProjectsTab = () => (
    <section className="view projects-view">
      <header className="view-header" data-tauri-drag-region>
        <h1>Projects</h1>
        <div className="view-actions">
          <button className="icon-btn" onClick={() => void refreshStatus()} type="button" title="Refresh status">
            ↻
          </button>
          <button className="icon-btn" onClick={() => setActiveTab('settings')} type="button" title="Settings">
            ⚙
          </button>
        </div>
      </header>

      <div className="scroll-area">
        {projects.length === 0 ? (
          <p className="empty">No configured projects yet. Open Settings to add your first one.</p>
        ) : (
          projects.map((project) => {
            const status = statusByProject[project.id];
            const isRunning = !!status?.isRunning;
            const busy = !!busyByProject[project.id];

            return (
              <article className="project-card" key={project.id}>
                <div className="project-row-top">
                  <div className="project-heading">
                    <span className={isRunning ? 'status-dot running' : 'status-dot stopped'} />
                    <h2>{project.name}</h2>
                  </div>

                  <div className="project-actions">
                    <button
                      className="danger-pill"
                      type="button"
                      disabled={!isRunning || busy}
                      onClick={() => void killProject(project.id)}
                    >
                      Stop
                    </button>
                    <button
                      className="neutral-pill"
                      type="button"
                      disabled={busy}
                      onClick={() => void openProject(project.id)}
                    >
                      Open
                    </button>
                  </div>
                </div>

                <div className="project-meta">
                  <span className="meta-left">
                    <span className="branch-icon">⑂</span>
                    {status?.branch ?? 'loading'}
                    <span className="port-label">:{project.port}</span>
                  </span>
                  <span className="meta-right">{relativeTime(status?.lastRunningAt ?? null)}</span>
                </div>

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
          ←
        </button>
        <h1>Settings</h1>
        <div className="view-actions" />
      </header>

      <div className="scroll-area settings-scroll">
        <section className="settings-section">
          <h2>Add project</h2>
          <form className="settings-form" onSubmit={handleAddProject}>
            <label>
              Path
              <div className="path-row">
                <input
                  value={newPath}
                  onChange={(event) => setNewPath(event.target.value)}
                  placeholder="/Users/me/project"
                  required
                />
                <button type="button" onClick={() => void handleBrowseNewPath()}>
                  Browse
                </button>
              </div>
            </label>

            <label>
              Name
              <input
                value={newName}
                onChange={(event) => setNewName(event.target.value)}
                placeholder="api"
                required
              />
            </label>

            <label>
              Port
              <input
                value={newPort}
                onChange={(event) => {
                  setManualPortEdited(true);
                  setNewPort(event.target.value);
                }}
                inputMode="numeric"
                placeholder="3000"
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
                        onClick={() => {
                          setManualPortEdited(true);
                          setNewPort(String(candidate.port));
                        }}
                        title={candidate.detail}
                      >
                        {candidate.port} · {sourceLabel(candidate.source)}
                      </button>
                    ))}
                  </div>
                </>
              ) : null}
              {!detectingPort && newPath.trim() && detectedCandidates.length === 0 && !detectedError ? (
                <p className="detecting-text">No port detected, please enter manually.</p>
              ) : null}
              {detectedError ? <p className="detect-error">{detectedError}</p> : null}
            </div>

            <button className="primary-btn" type="submit" disabled={loading}>
              Add project
            </button>
          </form>
        </section>

        <section className="settings-section">
          <h2>Configured projects</h2>
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

                    if (draft) {
                      return (
                        <SortableDraftCard id={project.id} key={project.id}>
                          <div className="editable-card-body">
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

        <section className="settings-section compact">
          <h2>Application</h2>
          <label className="toggle-row">
            <span>Start at login</span>
            <input
              type="checkbox"
              checked={settings?.autostartEnabled ?? false}
              onChange={(event) => void setAutostart(event.target.checked)}
            />
          </label>

          <button
            className="action-btn"
            type="button"
            onClick={() => void checkForUpdates()}
            disabled={checkingUpdates}
          >
            {checkingUpdates ? 'Checking updates...' : 'Check updates'}
          </button>

          <button
            className="action-btn action-btn-quit"
            type="button"
            onClick={() => void quitApp()}
          >
            Quit
          </button>
        </section>
      </div>
    </section>
  );

  return (
    <main className="utility-shell">
      <section className="utility-panel">
        <div className="content-panel">
          {activeTab === 'projects' ? renderProjectsTab() : renderSettingsTab()}
        </div>
      </section>

      {toast ? <aside className={`toast toast-${toast.tone}`}>{toast.message}</aside> : null}
    </main>
  );
}
