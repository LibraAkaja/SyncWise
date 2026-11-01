import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "@testing-library/jest-dom/vitest";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import Execution from "./Execution";
import type { RunSummary } from "./types/generated/RunSummary";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

const handlers = new Map<string, (event: { payload: unknown }) => void>();
const emit = (name: string, payload: unknown) => act(() => handlers.get(name)!({ payload }));

const running: RunSummary = { runId: "run-1", planId: "p", sourceRoot: "C:\\S", destinationRoot: "D:\\D", status: "running", startedMs: 1, finishedMs: null, journalPath: "j", snapshotPath: "s", error: null, totalItems: 2, doneItems: 0, bytesWritten: 0, attentionItems: [] };
const failed: RunSummary = { ...running, status: "needs_attention", doneItems: 1, bytesWritten: 2048, attentionItems: [{ itemId: "b", sourceRelativePath: "b.mp3", destinationRelativePath: "b.mp3", sizeBytes: 10, state: "failed", bytesWritten: 0, message: "Drive was removed." }] };

beforeEach(() => {
  handlers.clear();
  vi.mocked(listen).mockReset().mockImplementation((async (name: string, handler: never) => { handlers.set(name, handler); return () => {}; }) as never);
  vi.mocked(invoke).mockReset().mockImplementation((async (command: string) => {
    if (command === "get_run") return running;
    if (command === "resume_sync") return "run-1";
    return undefined;
  }) as never);
});
afterEach(cleanup);

describe("Execution", () => {
  it("shows progress, per-file errors, and retries the remaining files", async () => {
    const user = userEvent.setup();
    render(<Execution runId="run-1" onClose={vi.fn()} />);
    await screen.findByText("Starting...");
    await vi.waitFor(() => expect(handlers.size).toBe(2));
    emit("syncwise://sync-progress", { runId: "other", itemId: "x", path: "x", itemBytes: 0, itemTotal: 1, completedItems: 0, totalItems: 1, bytesDone: 0, bytesTotal: 1 });
    expect(screen.getByText("Starting...")).toBeInTheDocument();
    emit("syncwise://sync-progress", { runId: "run-1", itemId: "a", path: "a.mp3", itemBytes: 512, itemTotal: 1024, completedItems: 0, totalItems: 2, bytesDone: 512, bytesTotal: 2048 });
    expect(screen.getByText(/0 of 2 files, 512 B of 2.0 KB \(25%\)/)).toBeInTheDocument();
    expect(screen.getByText("a.mp3")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Cancel sync/ }));
    expect(invoke).toHaveBeenCalledWith("cancel_sync", { runId: "run-1" });

    emit("syncwise://sync-complete", { runId: "run-1", summary: failed, error: null });
    expect(await screen.findByText("Finished with errors")).toBeInTheDocument();
    expect(screen.getByText("b.mp3")).toBeInTheDocument();
    expect(screen.getByText(/Drive was removed\./)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Retry remaining files/ }));
    expect(invoke).toHaveBeenCalledWith("resume_sync", { runId: "run-1" });
    expect(await screen.findByRole("button", { name: /Cancel sync/ })).toBeInTheDocument();
  });

  it("does not offer retry after a fully successful run", async () => {
    render(<Execution runId="run-1" onClose={vi.fn()} />);
    await vi.waitFor(() => expect(handlers.size).toBe(2));
    emit("syncwise://sync-complete", { runId: "run-1", summary: { ...running, status: "completed", doneItems: 2, attentionItems: [] }, error: null });
    expect(await screen.findByText("Completed")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Retry/ })).not.toBeInTheDocument();
  });

  it("stops showing progress when completion has no saved report", async () => {
    render(<Execution runId="run-1" onClose={vi.fn()} />);
    await vi.waitFor(() => expect(handlers.size).toBe(2));
    emit("syncwise://sync-complete", {
      runId: "run-1",
      summary: null,
      error: { message: "Local database is unavailable." },
    });
    expect(await screen.findByRole("heading", { name: "Final report unavailable" })).toBeInTheDocument();
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
    expect(screen.getByText("Local database is unavailable.")).toBeInTheDocument();
  });
});
