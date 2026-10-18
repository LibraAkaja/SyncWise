import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { FolderPair } from "./types/generated/FolderPair";
import "./App.css";

type FolderValidationStatus = "incomplete" | "ready" | "checking" | "valid" | "invalid";

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

  async function validateFolders(source: string, destination: string) {
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
            <button type="button" onClick={() => chooseFolder("source")}>
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
            <button type="button" onClick={() => chooseFolder("destination")}>
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
            {validationStatus === "incomplete" && "Choose both folders to validate the pair."}
            {validationStatus === "ready" && "Folders have changed. Validate them before continuing."}
            {validationStatus === "checking" && "Checking folder paths…"}
            {validationStatus === "valid" && "Folder pair is valid."}
            {validationStatus === "invalid" && (error || "Folder pair is invalid.")}
          </span>
        </div>

        <button
          className="validate-button"
          type="button"
          disabled={!sourcePath || !destinationPath || validationStatus === "checking"}
          onClick={() => validateFolders(sourcePath, destinationPath)}
        >
          Validate folders
        </button>

        {validatedPair && (
          <div className="canonical-paths">
            <span>Validated source</span>
            <code>{validatedPair.sourceRoot}</code>
            <span>Validated destination</span>
            <code>{validatedPair.destinationRoot}</code>
          </div>
        )}
      </section>
    </main>
  );
}

export default App;
