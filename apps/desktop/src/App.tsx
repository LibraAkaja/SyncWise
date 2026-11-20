import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type { FolderPair } from "./types/generated/FolderPair";
import type { ScanProgress } from "./types/generated/ScanProgress";
import type { ScanResult } from "./types/generated/ScanResult";
import type { SyncError } from "./types/generated/SyncError";
import type { Plan } from "./types/generated/Plan";
import type { DecisionState } from "./types/generated/DecisionState";
import Review from "./Review";
import Execution from "./Execution";
import type { RunSummary } from "./types/generated/RunSummary";
import type { RunHistoryEntry } from "./types/generated/RunHistoryEntry";
import type { RecoveryReport } from "./types/generated/RecoveryReport";
import ThemeControl, { type ThemePreference } from "./ThemeControl";
import "./App.css";

type FolderValidationStatus = "incomplete" | "checking" | "valid" | "invalid";
type ScanSide = "source" | "destination";
type ScanStatus = "idle" | "running" | "completed" | "cancelled" | "error";

type ScanProgressEvent = {
  scanId: string;
  side: ScanSide;
  progress: ScanProgress;
};

type ScanCompletionEvent = {
  scanId: string;
  source: ScanResult | null;
  destination: ScanResult | null;
  error: SyncError | null;
  cancelled: boolean;
  plan: Plan | null;
  decisions: DecisionState | null;
};

type ScanResults = {
  source: ScanResult | null;
  destination: ScanResult | null;
};

type SavedProfile = {
  id: number;
  name: string;
  sourceRoot: string;
  destinationRoot: string;
  updatedMs: number;
};

function initialTheme(): ThemePreference {
  try {
    const saved = localStorage.getItem("syncwise-theme");
    return saved === "light" || saved === "dark" ? saved : "system";
  } catch {
    return "system";
  }
}

const emptyScanProgress: ScanProgress = {
  filesScanned: 0,
  directoriesScanned: 0,
  issuesFound: 0,
};

function messageFromError(error: unknown): string {
  if (typeof error === "string") {
    try {
      const parsed: unknown = JSON.parse(error);
      if (
        typeof parsed === "object" &&
        parsed !== null &&
        "message" in parsed &&
        typeof parsed.message === "string"
      ) {
        return parsed.message;
      }
    } catch {
      return error;
    }
    return error;
  }

  if (error instanceof Error) return error.message;
  if (
    typeof error === "object" &&
    error !== null &&
    "message" in error &&
    typeof error.message === "string"
  ) {
    return error.message;
  }
  return "The selected folders could not be validated.";
}

function App() {
  const [sourcePath, setSourcePath] = useState("");
  const [destinationPath, setDestinationPath] = useState("");
  const [validatedPair, setValidatedPair] = useState<FolderPair | null>(null);
  const [validationStatus, setValidationStatus] =
    useState<FolderValidationStatus>("incomplete");
  const [error, setError] = useState("");
  const [scanStatus, setScanStatus] = useState<ScanStatus>("idle");
  const [scanId, setScanId] = useState<string | null>(null);
  const [scanError, setScanError] = useState("");
  const [scanProgress, setScanProgress] = useState<Record<ScanSide, ScanProgress>>({
    source: emptyScanProgress,
    destination: emptyScanProgress,
  });
  const [scanResults, setScanResults] = useState<ScanResults | null>(null);
  const [review, setReview] = useState<{ plan: Plan; decisions: DecisionState } | null>(null);
  const [showReview, setShowReview] = useState(false);
  const [executionRun, setExecutionRun] = useState<string | null>(null);
  const [interrupted, setInterrupted] = useState<RunSummary[]>([]);
  const [inspection, setInspection] = useState<{ runId: string; report: RecoveryReport } | null>(null);
  const [recoveryError, setRecoveryError] = useState("");
  const [profiles, setProfiles] = useState<SavedProfile[]>([]);
  const [profileName, setProfileName] = useState("");
  const [selectedProfileId, setSelectedProfileId] = useState("");
  const [profileError, setProfileError] = useState("");
  const [theme, setTheme] = useState<ThemePreference>(initialTheme);
  const [themeError, setThemeError] = useState("");
  const [runHistory, setRunHistory] = useState<RunHistoryEntry[]>([]);
  const [historySearch, setHistorySearch] = useState("");
  const [historyStatus, setHistoryStatus] = useState("all");
  const [historyError, setHistoryError] = useState("");
  const activeScanId = useRef<string | null>(null);
  const unlistenProgress = useRef<UnlistenFn | null>(null);
  const unlistenCompletion = useRef<UnlistenFn | null>(null);

  function stopListening() {
    unlistenProgress.current?.();
    unlistenCompletion.current?.();
    unlistenProgress.current = null;
    unlistenCompletion.current = null;
  }

  function applyScanProgress(payload: ScanProgressEvent) {
    if (payload.scanId !== activeScanId.current) return;
    setScanProgress((current) => ({ ...current, [payload.side]: payload.progress }));
  }

  function applyScanCompletion(payload: ScanCompletionEvent) {
    if (payload.scanId !== activeScanId.current) return;
    activeScanId.current = null;
    setScanId(null);
    setScanResults({ source: payload.source, destination: payload.destination });
    setScanError(payload.error?.message ?? "");
    setScanStatus(
      payload.error ? "error" : payload.cancelled ? "cancelled" : "completed",
    );
    if (!payload.cancelled && !payload.error && payload.plan && payload.decisions) {
      setReview({ plan: payload.plan, decisions: payload.decisions });
      setShowReview(true);
    }
    stopListening();
  }

  useEffect(() => {
    void refreshInterruptedRuns();
    void refreshProfiles();
    void refreshRunHistory();
    return () => {
      stopListening();
      if (activeScanId.current) {
        void invoke("cancel_scan", { scanId: activeScanId.current }).catch(() => {});
      }
    };
  }, []);

  useEffect(() => {
    const preference = window.matchMedia?.("(prefers-color-scheme: dark)");
    const applyTheme = () => {
      document.documentElement.dataset.theme =
        theme === "system" ? (preference?.matches ? "dark" : "light") : theme;
    };
    applyTheme();
    if (theme === "system") preference?.addEventListener("change", applyTheme);
    try {
      localStorage.setItem("syncwise-theme", theme);
      setThemeError("");
    } catch {
      setThemeError("Theme is active but could not be saved on this device.");
    }
    return () => preference?.removeEventListener("change", applyTheme);
  }, [theme]);

  async function refreshInterruptedRuns() {
    try {
      setInterrupted(await invoke<RunSummary[]>("list_interrupted_runs"));
      setRecoveryError("");
    } catch (caughtError) {
      setRecoveryError(messageFromError(caughtError));
    }
  }

  async function refreshProfiles() {
    try {
      setProfiles(await invoke<SavedProfile[]>("list_profiles"));
      setProfileError("");
    } catch (caughtError) {
      setProfileError(messageFromError(caughtError));
    }
  }

  async function refreshRunHistory() {
    try {
      setRunHistory(await invoke<RunHistoryEntry[]>("list_run_history"));
      setHistoryError("");
    } catch (caughtError) {
      setHistoryError(messageFromError(caughtError));
    }
  }

  async function saveProfile() {
    if (!validatedPair || !profileName.trim()) return;
    setProfileError("");
    try {
      const profile = await invoke<SavedProfile>("save_profile", {
        name: profileName,
        sourcePath: validatedPair.sourceRoot,
        destinationPath: validatedPair.destinationRoot,
      });
      setProfiles((current) => [...current.filter((entry) => entry.id !== profile.id), profile]
        .sort((left, right) => left.name.localeCompare(right.name)));
      setSelectedProfileId(String(profile.id));
      setProfileName("");
    } catch (caughtError) {
      setProfileError(messageFromError(caughtError));
    }
  }

  async function loadProfile() {
    const profile = profiles.find((entry) => String(entry.id) === selectedProfileId);
    if (!profile) return;
    setSourcePath(profile.sourceRoot);
    setDestinationPath(profile.destinationRoot);
    await validateFolders(profile.sourceRoot, profile.destinationRoot);
  }

  async function deleteProfile() {
    const profileId = Number(selectedProfileId);
    if (!Number.isSafeInteger(profileId)) return;
    setProfileError("");
    try {
      await invoke("delete_profile", { profileId });
      setProfiles((current) => current.filter((entry) => entry.id !== profileId));
      setSelectedProfileId("");
    } catch (caughtError) {
      setProfileError(messageFromError(caughtError));
    }
  }

  async function validateFolders(source: string, destination: string) {
    setReview(null); setShowReview(false);
    setScanStatus("idle");
    setScanResults(null);
    setScanError("");
    if (!source || !destination) {
      setValidationStatus("incomplete");
      setValidatedPair(null);
      return;
    }

    setValidationStatus("checking");
    setValidatedPair(null);
    setError("");
    try {
      const pair = await invoke<FolderPair>("validate_folder_pair", {
        sourcePath: source,
        destinationPath: destination,
      });
      setValidatedPair(pair);
      setValidationStatus("valid");
    } catch (caughtError) {
      setValidationStatus("invalid");
      setError(messageFromError(caughtError));
    }
  }

  async function chooseFolder(side: "source" | "destination") {
    setError("");
    try {
      const selectedPath = await open({
        directory: true,
        multiple: false,
        title: side === "source" ? "Choose source folder" : "Choose destination folder",
      });
      if (typeof selectedPath !== "string") return;

      const nextSource = side === "source" ? selectedPath : sourcePath;
      const nextDestination = side === "destination" ? selectedPath : destinationPath;
      setSourcePath(nextSource);
      setDestinationPath(nextDestination);
      await validateFolders(nextSource, nextDestination);
    } catch (caughtError) {
      setValidationStatus("invalid");
      setError(messageFromError(caughtError));
    }
  }

  async function startScan() {
    if (!validatedPair) return;
    setReview(null); setShowReview(false);

    setScanStatus("running");
    setScanError("");
    setScanResults(null);
    setScanProgress({ source: emptyScanProgress, destination: emptyScanProgress });
    activeScanId.current = null;

    const earlyProgress: ScanProgressEvent[] = [];
    let earlyCompletion: ScanCompletionEvent | null = null;
    const pendingUnlisteners: UnlistenFn[] = [];

    try {
      const removeProgressListener = await listen<ScanProgressEvent>(
        "syncwise://scan-progress",
        ({ payload }) => {
          if (activeScanId.current === null) earlyProgress.push(payload);
          else applyScanProgress(payload);
        },
      );
      pendingUnlisteners.push(removeProgressListener);
      unlistenProgress.current = removeProgressListener;

      const removeCompletionListener = await listen<ScanCompletionEvent>(
        "syncwise://scan-complete",
        ({ payload }) => {
          if (activeScanId.current === null) earlyCompletion = payload;
          else applyScanCompletion(payload);
        },
      );
      pendingUnlisteners.push(removeCompletionListener);
      unlistenCompletion.current = removeCompletionListener;

      const startedScanId = await invoke<string>("start_scan", {
        sourcePath: validatedPair.sourceRoot,
        destinationPath: validatedPair.destinationRoot,
      });
      activeScanId.current = startedScanId;
      setScanId(startedScanId);

      for (const progress of earlyProgress) applyScanProgress(progress);
      if (earlyCompletion) applyScanCompletion(earlyCompletion);
    } catch (caughtError) {
      pendingUnlisteners.forEach((removeListener) => removeListener());
      unlistenProgress.current = null;
      unlistenCompletion.current = null;
      activeScanId.current = null;
      setScanId(null);
      setScanStatus("error");
      setScanError(messageFromError(caughtError));
    }
  }

  async function cancelScan() {
    if (!scanId) return;
    try {
      const cancelled = await invoke<boolean>("cancel_scan", { scanId });
      if (!cancelled) setScanError("This scan is no longer active.");
    } catch (caughtError) {
      setScanError(messageFromError(caughtError));
    }
  }

  async function resumeInterrupted(runId: string) {
    setRecoveryError("");
    try {
      await invoke<string>("resume_sync", { runId });
      setInterrupted([]);
      setExecutionRun(runId);
    } catch (caughtError) { setRecoveryError(messageFromError(caughtError)); }
  }

  async function inspectInterrupted(runId: string) {
    setRecoveryError("");
    try { setInspection({ runId, report: await invoke<RecoveryReport>("inspect_run", { runId }) }); }
    catch (caughtError) { setRecoveryError(messageFromError(caughtError)); }
  }

  async function dismissInterrupted(runId: string) {
    setRecoveryError("");
    try {
      await invoke("abandon_run", { runId });
      setInterrupted((runs) => runs.filter((run) => run.runId !== runId));
      setInspection((current) => (current?.runId === runId ? null : current));
    } catch (caughtError) { setRecoveryError(messageFromError(caughtError)); }
  }

  if (executionRun) {
    return <Execution key={executionRun} runId={executionRun} onClose={() => {
      setExecutionRun(null); setShowReview(false); setReview(null);
      void refreshInterruptedRuns();
      void refreshRunHistory();
    }} theme={theme} onThemeChange={setTheme} />;
  }

  if (showReview && review) {
    return <Review key={review.plan.id} plan={review.plan} initialDecisions={review.decisions} onBack={(decisions) => { setReview({ ...review, decisions }); setShowReview(false); }} onStarted={(runId) => setExecutionRun(runId)} theme={theme} onThemeChange={setTheme} />;
  }

  const filteredHistory = runHistory.filter((run) => {
    const query = historySearch.trim().toLocaleLowerCase();
    const matchesText = !query || `${run.sourceRoot}\n${run.destinationRoot}\n${run.runId}`
      .toLocaleLowerCase().includes(query);
    return matchesText && (historyStatus === "all" || run.status === historyStatus);
  });

  return (
    <main className="container">
      <section className="setup-panel" aria-labelledby="app-title">
        <p className="eyebrow">SYNCWISE / SETUP</p>
        <h1 id="app-title">Choose folders</h1>
        <p className="summary">Select where files come from and where they go.</p>
        <div className="setup-preferences">
          <ThemeControl value={theme} onChange={setTheme} />
          {themeError && <p className="review-error" role="status">{themeError}</p>}
        </div>

        {interrupted.map((run) => <div className="review-notice recovery-banner" role="alert" key={run.runId}>
          <p><strong>An earlier sync did not finish.</strong> {run.doneItems.toLocaleString()} of {run.totalItems.toLocaleString()} files were copied to {run.destinationRoot}. Nothing was deleted or replaced.</p>
          <div className="dialog-actions">
            <button type="button" onClick={() => resumeInterrupted(run.runId)}>Resume</button>
            <button className="secondary-button" type="button" onClick={() => inspectInterrupted(run.runId)}>Inspect temp files</button>
            <button className="secondary-button" type="button" onClick={() => dismissInterrupted(run.runId)}>Dismiss</button>
          </div>
          {inspection?.runId === run.runId && <div>
            <p>{inspection.report.orphanedTempPaths.length.toLocaleString()} leftover temporary files and {inspection.report.interruptedItems.length.toLocaleString()} unfinished items. Nothing is removed automatically.</p>
            <ul className="preflight-issues">{inspection.report.orphanedTempPaths.slice(0, 20).map((path) => <li key={path}>{path}</li>)}</ul>
          </div>}
        </div>)}
        {recoveryError && <p className="review-error" role="alert">{recoveryError}</p>}

        <section className="profile-section" aria-label="Saved profiles">
          <div className="profile-controls">
            <label>
              Saved profiles
              <select
                aria-label="Saved profiles"
                value={selectedProfileId}
                onChange={(event) => setSelectedProfileId(event.target.value)}
              >
                <option value="">Choose a saved folder pair</option>
                {profiles.map((profile) => (
                  <option key={profile.id} value={profile.id}>{profile.name}</option>
                ))}
              </select>
            </label>
            <button type="button" className="secondary-button" disabled={!selectedProfileId || scanStatus === "running" || validationStatus === "checking"} onClick={() => void loadProfile()}>
              Load profile
            </button>
            <button type="button" className="secondary-button" disabled={!selectedProfileId || scanStatus === "running"} onClick={() => void deleteProfile()}>
              Delete profile
            </button>
          </div>
          <div className="profile-save">
            <label htmlFor="profile-name">Save current folders as</label>
            <input id="profile-name" value={profileName} maxLength={80} onChange={(event) => setProfileName(event.target.value)} placeholder="e.g. Music backup" />
            <button type="button" disabled={!validatedPair || !profileName.trim() || scanStatus === "running"} onClick={() => void saveProfile()}>
              Save profile
            </button>
          </div>
          {profileError && <p className="review-error" role="alert">{profileError}</p>}
        </section>

        <section className="history-section" aria-labelledby="history-heading">
          <div className="history-heading">
            <div>
              <h2 id="history-heading">Sync history</h2>
              <p>Recent runs are stored locally; showing up to 100.</p>
            </div>
            <button type="button" className="secondary-button" onClick={() => void refreshRunHistory()}>Refresh history</button>
          </div>
          <div className="history-filters">
            <label>
              Search run history
              <input aria-label="Search run history" type="search" value={historySearch} onChange={(event) => setHistorySearch(event.target.value)} placeholder="Folder path or run ID" />
            </label>
            <label>
              Run status
              <select aria-label="Filter history by status" value={historyStatus} onChange={(event) => setHistoryStatus(event.target.value)}>
                <option value="all">All statuses</option>
                <option value="running">Running</option>
                <option value="completed">Completed</option>
                <option value="needs_attention">Needs attention</option>
                <option value="cancelled">Cancelled</option>
                <option value="abandoned">Dismissed</option>
              </select>
            </label>
          </div>
          {historyError && <p className="review-error" role="alert">{historyError}</p>}
          <p className="history-count" role="status" aria-live="polite">{filteredHistory.length.toLocaleString()} runs</p>
          {filteredHistory.length > 0 ? <ul className="history-list" aria-label="Recent sync runs">
            {filteredHistory.map((run) => <li key={run.runId}>
              <div>
                <strong>{run.status.replace("_", " ")}</strong>
                <span>{new Date(run.startedMs).toLocaleString()}</span>
                <span className="history-roots" title={`${run.sourceRoot} → ${run.destinationRoot}`}>{run.sourceRoot} → {run.destinationRoot}</span>
                <span>{run.doneItems.toLocaleString()} / {run.totalItems.toLocaleString()} files · {run.bytesWritten.toLocaleString()} bytes written</span>
                {run.error && <span className="history-error">{run.error}</span>}
              </div>
            </li>)}
          </ul> : !historyError && <div className="history-empty"><p>{runHistory.length ? "No runs match these filters." : "No sync runs yet."}</p></div>}
        </section>

        <div className="folder-list">
          <div className="folder-row">
            <div className="folder-copy">
              <span className="folder-direction source-direction">FROM</span>
              <strong>Source</strong>
              <span className="folder-path">{sourcePath || "No source folder selected"}</span>
            </div>
            <button
              type="button"
              disabled={scanStatus === "running" || validationStatus === "checking"}
              onClick={() => chooseFolder("source")}
            >
              Choose source
            </button>
          </div>

          <div className="folder-row">
            <div className="folder-copy">
              <span className="folder-direction destination-direction">TO</span>
              <strong>Destination</strong>
              <span className="folder-path">
                {destinationPath || "No destination folder selected"}
              </span>
            </div>
            <button
              type="button"
              disabled={scanStatus === "running" || validationStatus === "checking"}
              onClick={() => chooseFolder("destination")}
            >
              Choose destination
            </button>
          </div>
        </div>

        <div
          className={`validation validation-${validationStatus}`}
          role="status"
          aria-live="polite"
        >
          <span className="status-indicator" />
          <span>
            {validationStatus === "incomplete" && "Choose source and destination folders."}
            {validationStatus === "checking" && "Checking folder paths…"}
            {validationStatus === "valid" && "Folder pair is valid."}
            {validationStatus === "invalid" && (error || "Folder pair is invalid.")}
          </span>
        </div>

        {validatedPair && (
          <section className="scan-section" aria-labelledby="scan-title">
            <div className="scan-heading">
              <h2 id="scan-title">Folder scan</h2>
              <div className="scan-actions">
                {review && <button type="button" onClick={() => setShowReview(true)}>Review results</button>}
                <button
                  type="button"
                  onClick={startScan}
                  disabled={scanStatus === "running" || validationStatus !== "valid"}
                >
                  Scan folders
                </button>
                {scanStatus === "running" && (
                  <button className="cancel-button" type="button" onClick={cancelScan}>
                    Cancel scan
                  </button>
                )}
              </div>
            </div>

            {scanStatus !== "idle" && (
              <div className={`scan-state scan-${scanStatus}`} role="status" aria-live="polite">
                <span className="status-indicator" />
                <span>
                  {scanStatus === "running" && "Scanning source and destination…"}
                  {scanStatus === "completed" && "Scan complete."}
                  {scanStatus === "cancelled" && "Scan cancelled; partial results are shown."}
                  {scanStatus === "error" && scanError}
                </span>
              </div>
            )}

            {scanStatus === "running" && (
              <div className="scan-progress-list">
                {(["source", "destination"] as const).map((side) => (
                  <div className="scan-progress-row" key={side}>
                    <strong>{side === "source" ? "Source" : "Destination"}</strong>
                    <span>{scanProgress[side].filesScanned.toLocaleString()} files</span>
                    <span>{scanProgress[side].directoriesScanned.toLocaleString()} folders</span>
                    <span>{scanProgress[side].issuesFound.toLocaleString()} issues</span>
                  </div>
                ))}
              </div>
            )}

            {scanError && scanStatus !== "error" && (
              <p className="scan-error">{scanError}</p>
            )}

            {scanResults && (
              <div className="scan-results">
                {(["source", "destination"] as const).map((side) => {
                  const result = scanResults[side];
                  if (!result) return null;

                  return (
                    <section className="scan-result" key={side}>
                      <div className="scan-result-heading">
                        <h3>{side === "source" ? "Source index" : "Destination index"}</h3>
                        <span>{result.files.length.toLocaleString()} files</span>
                      </div>
                      {result.issues.length > 0 && (
                        <p className="scan-issue-count">
                          {result.issues.length.toLocaleString()} items skipped or unreadable
                        </p>
                      )}
                      <ul className="file-preview">
                        {result.files.slice(0, 8).map((file) => (
                          <li key={file.relativePath}>
                            <span title={file.relativePath}>{file.relativePath}</span>
                            <code>{file.sizeBytes.toLocaleString()} B</code>
                          </li>
                        ))}
                      </ul>
                      {result.files.length > 8 && (
                        <p className="preview-note">
                          Showing 8 of {result.files.length.toLocaleString()} indexed files.
                        </p>
                      )}
                      {result.issues.slice(0, 3).map((issue, index) => (
                        <p className="scan-issue" key={`${issue.relativePath}-${index}`}>
                          {issue.relativePath ? `${issue.relativePath}: ` : ""}
                          {issue.message}
                        </p>
                      ))}
                    </section>
                  );
                })}
              </div>
            )}
          </section>
        )}
      </section>
    </main>
  );
}

export default App;
