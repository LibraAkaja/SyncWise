import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { AlertTriangle, ArrowLeft, CheckCircle2, RotateCw, X } from "lucide-react";
import type { RunSummary } from "./types/generated/RunSummary";
import { formatBytes } from "./Review";
import ThemeControl, { type ThemePreference } from "./ThemeControl";

export type SyncProgressEvent = {
  runId: string; itemId: string; path: string; itemBytes: number; itemTotal: number;
  completedItems: number; totalItems: number; bytesDone: number; bytesTotal: number;
};
type SyncCompletionEvent = { runId: string; summary: RunSummary | null; error: { message: string } | null };

function errorMessage(error: unknown): string {
  return typeof error === "object" && error !== null && "message" in error ? String(error.message) : String(error);
}

const statusText: Record<RunSummary["status"], string> = {
  running: "Running", completed: "Completed", needs_attention: "Finished with errors",
  cancelled: "Cancelled", abandoned: "Dismissed",
};

export default function Execution({ runId, onClose, theme, onThemeChange }: {
  runId: string; onClose: () => void; theme?: ThemePreference;
  onThemeChange?: (theme: ThemePreference) => void;
}) {
  const [currentRun, setCurrentRun] = useState(runId);
  const [progress, setProgress] = useState<SyncProgressEvent | null>(null);
  const [summary, setSummary] = useState<RunSummary | null>(null);
  const [error, setError] = useState("");
  const [cancelling, setCancelling] = useState(false);
  const [finishedWithoutSummary, setFinishedWithoutSummary] = useState(false);
  const activeRun = useRef(runId);
  activeRun.current = currentRun;

  useEffect(() => {
    let disposed = false;
    const unlisteners: UnlistenFn[] = [];
    (async () => {
      const onProgress = await listen<SyncProgressEvent>("syncwise://sync-progress", ({ payload }) => {
        if (payload.runId === activeRun.current) setProgress(payload);
      });
      const onComplete = await listen<SyncCompletionEvent>("syncwise://sync-complete", ({ payload }) => {
        if (payload.runId !== activeRun.current) return;
        setCancelling(false);
        setFinishedWithoutSummary(payload.summary === null);
        if (payload.error) setError(payload.error.message);
        else if (!payload.summary) setError("The sync ended, but its final report is unavailable. Check recovery after returning to setup.");
        setSummary(payload.summary);
      });
      if (disposed) { onProgress(); onComplete(); return; }
      unlisteners.push(onProgress, onComplete);
      // Catch a completion that happened before the listeners were attached.
      try {
        const latest = await invoke<RunSummary>("get_run", { runId });
        if (!disposed && latest.status !== "running") {
          setSummary(latest);
          setFinishedWithoutSummary(true);
        }
      } catch (failure) { if (!disposed) setError(errorMessage(failure)); }
    })();
    return () => { disposed = true; unlisteners.forEach((remove) => remove()); };
  }, [runId]);

  async function cancel() {
    setCancelling(true);
    try { await invoke("cancel_sync", { runId: currentRun }); }
    catch (failure) { setError(errorMessage(failure)); setCancelling(false); }
  }

  async function retry() {
    setError("");
    try {
      const resumed = await invoke<string>("resume_sync", { runId: currentRun });
      setCurrentRun(resumed); setSummary(null); setProgress(null); setFinishedWithoutSummary(false);
    } catch (failure) { setError(errorMessage(failure)); }
  }

  const running = summary === null && !finishedWithoutSummary;
  const percent = progress
    ? progress.bytesTotal > 0
      ? Math.min(100, Math.round((progress.bytesDone / progress.bytesTotal) * 100))
      : progress.totalItems > 0
        ? Math.round((progress.completedItems / progress.totalItems) * 100)
        : 0
    : 0;
  const failed = summary ? summary.attentionItems : [];

  return <main className="review-page execution-page">
    <header className="review-header"><div className="review-title"><div><p className="eyebrow">SYNCWISE</p><h1>{running ? "Copying files" : "Sync report"}</h1></div></div><ThemeControl value={theme ?? "system"} onChange={onThemeChange ?? (() => {})} /></header>
    {error && <p className="review-error" role="alert">{error}</p>}
    {running && <section className="execution-panel" aria-label="Sync progress">
      <progress max={100} value={percent} aria-label="Overall progress" />
      <p role="status" aria-live="polite">
        {progress ? `${progress.completedItems.toLocaleString()} of ${progress.totalItems.toLocaleString()} files, ${formatBytes(progress.bytesDone)} of ${formatBytes(progress.bytesTotal)} (${percent}%)` : "Starting..."}
      </p>
      {progress && <p className="row-path" title={progress.path}>{progress.path}</p>}
      <button className="secondary-button" type="button" disabled={cancelling} onClick={cancel}><X size={16} />{cancelling ? "Cancelling after this file..." : "Cancel sync"}</button>
    </section>}
    {summary && <section className="execution-panel" aria-label="Final report">
      <h2>{summary.status === "completed" ? <CheckCircle2 size={20} /> : <AlertTriangle size={20} />}{statusText[summary.status]}</h2>
      <dl className="preflight-stats">
        <dt>Copied</dt><dd>{summary.doneItems.toLocaleString()} of {summary.totalItems.toLocaleString()} files</dd>
        <dt>Data written</dt><dd>{formatBytes(summary.bytesWritten)}</dd>
        <dt>Needs attention</dt><dd>{(summary.totalItems - summary.doneItems).toLocaleString()}</dd>
      </dl>
      {summary.error && <p className="review-error">{summary.error}</p>}
      {failed.length > 0 && <ul className="preflight-issues" aria-label="Files that were not copied">
        {failed.map((item) => <li key={item.itemId} className="blocking-issue"><strong>{item.sourceRelativePath}</strong>: {item.message ?? item.state.replace("_", " ")}</li>)}
      </ul>}
      {summary.totalItems - summary.doneItems > failed.length && <p className="review-muted">Showing the first {failed.length} of {(summary.totalItems - summary.doneItems).toLocaleString()} files that need attention.</p>}
      <div className="dialog-actions">
        <button className="secondary-button" type="button" onClick={onClose}><ArrowLeft size={16} />Back to setup</button>
        {(summary.status === "needs_attention" || summary.status === "cancelled") && <button type="button" onClick={retry}><RotateCw size={16} />Retry remaining files</button>}
      </div>
    </section>}
    {finishedWithoutSummary && !summary && <section className="execution-panel" aria-label="Final report unavailable">
      <h2><AlertTriangle size={20} />Final report unavailable</h2>
      <p>The sync has stopped, but its final status could not be loaded. Return to setup to check recovery before retrying.</p>
      <div className="dialog-actions">
        <button className="secondary-button" type="button" onClick={onClose}><ArrowLeft size={16} />Back to setup</button>
      </div>
    </section>}
  </main>;
}
