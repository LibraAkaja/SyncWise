import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import "./App.css";

type TaskStatus = "running" | "completed" | "cancelled";
type DisplayStatus = "ready" | TaskStatus | "error";

type TaskProgress = {
  taskId: string;
  progress: number;
  status: TaskStatus;
};

const taskProgressEvent = "syncwise://task-progress";

function App() {
  const [taskId, setTaskId] = useState<string | null>(null);
  const [progress, setProgress] = useState(0);
  const [status, setStatus] = useState<DisplayStatus>("ready");
  const [error, setError] = useState("");
  const activeTaskId = useRef<string | null>(null);
  const unlisten = useRef<UnlistenFn | null>(null);

  useEffect(() => {
    return () => unlisten.current?.();
  }, []);

  function applyProgress(payload: TaskProgress) {
    if (payload.taskId !== activeTaskId.current) return;

    setProgress(payload.progress);
    setStatus(payload.status);

    if (payload.status !== "running") {
      activeTaskId.current = null;
      setTaskId(null);
      unlisten.current?.();
      unlisten.current = null;
    }
  }

  async function startTask() {
    setError("");
    setProgress(0);
    setStatus("running");

    const earlyProgress = { current: null as TaskProgress | null };

    try {
      unlisten.current = await listen<TaskProgress>(taskProgressEvent, ({ payload }) => {
        if (activeTaskId.current === null) {
          earlyProgress.current = payload;
          return;
        }

        applyProgress(payload);
      });

      const startedTaskId = await invoke<string>("start_demo_task");
      activeTaskId.current = startedTaskId;
      setTaskId(startedTaskId);

      if (earlyProgress.current?.taskId === startedTaskId) {
        applyProgress(earlyProgress.current);
      }
    } catch (caughtError) {
      unlisten.current?.();
      unlisten.current = null;
      activeTaskId.current = null;
      setTaskId(null);
      setStatus("error");
      setError(caughtError instanceof Error ? caughtError.message : String(caughtError));
    }
  }

  async function cancelTask() {
    if (!taskId) return;

    try {
      const cancelled = await invoke<boolean>("cancel_demo_task", { taskId });
      if (!cancelled) setError("The task is no longer active.");
    } catch (caughtError) {
      setError(caughtError instanceof Error ? caughtError.message : String(caughtError));
    }
  }

  return (
    <main className="container">
      <section className="task-panel" aria-labelledby="app-title">
        <p className="eyebrow">SYNCWISE / RUNTIME CHECK</p>
        <h1 id="app-title">Background task</h1>
        <p className="summary">Rust task progress and cancellation</p>

        <div className={`status status-${status}`} role="status" aria-live="polite">
          <span className="status-indicator" />
          <span>
            {status === "ready" && "Ready"}
            {status === "running" && "Running"}
            {status === "completed" && "Completed"}
            {status === "cancelled" && "Cancelled"}
            {status === "error" && "Could not start"}
          </span>
          {taskId && <code>{taskId}</code>}
        </div>

        <div
          className="progress-track"
          role="progressbar"
          aria-label="Task progress"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={progress}
        >
          <span className="progress-fill" style={{ width: `${progress}%` }} />
        </div>
        <div className="progress-label">
          <span>Progress</span>
          <span>{progress}%</span>
        </div>

        <div className="actions">
          <button type="button" onClick={startTask} disabled={status === "running"}>
            Start task
          </button>
          {status === "running" && (
            <button className="cancel-button" type="button" onClick={cancelTask}>
              Cancel
            </button>
          )}
        </div>

        {error && <p className="error-message">{error}</p>}
      </section>
    </main>
  );
}

export default App;
