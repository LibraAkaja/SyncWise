import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type { FolderPair } from "./types/generated/FolderPair";
import type { ScanProgress } from "./types/generated/ScanProgress";
import type { ScanResult } from "./types/generated/ScanResult";
import type { SyncError } from "./types/generated/SyncError";
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
};

type ScanResults = {
  source: ScanResult | null;
  destination: ScanResult | null;
};

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
    stopListening();
  }

  useEffect(() => {
    return () => {
      stopListening();
      if (activeScanId.current) {
        void invoke("cancel_scan", { scanId: activeScanId.current }).catch(() => {});
      }
    };
  }, []);

  async function validateFolders(source: string, destination: string) {
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

  return (
    <main className="container">
      <section className="setup-panel" aria-labelledby="app-title">
        <p className="eyebrow">SYNCWISE / SETUP</p>
        <h1 id="app-title">Choose folders</h1>
        <p className="summary">Select where files come from and where they go.</p>

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
