import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "@testing-library/jest-dom/vitest";
import { invoke } from "@tauri-apps/api/core";
import App from "./App";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));

afterEach(cleanup);
beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "list_interrupted_runs") return [] as never;
    if (command === "list_run_history") return [{
      runId: "run-1",
      sourceRoot: "C:\\Music",
      destinationRoot: "D:\\Backup",
      status: "completed",
      startedMs: 1,
      finishedMs: 2,
      totalItems: 4,
      doneItems: 4,
      bytesWritten: 4096,
      error: null,
    }] as never;
    if (command === "list_profiles") return [{
      id: 7,
      name: "Music backup",
      sourceRoot: "C:\\Music",
      destinationRoot: "D:\\Backup",
      updatedMs: 1,
    }] as never;
    if (command === "validate_folder_pair") return {
      sourceRoot: "C:\\Music",
      destinationRoot: "D:\\Backup",
    } as never;
    if (command === "save_profile") return {
      id: 7,
      name: "Updated music",
      sourceRoot: "C:\\Music",
      destinationRoot: "D:\\Backup",
      updatedMs: 2,
    } as never;
    throw new Error(`Unexpected command ${command}`);
  });
});

describe("saved folder profiles", () => {
  it("revalidates a loaded profile and saves the validated folder pair", async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.selectOptions(await screen.findByRole("combobox", { name: "Saved profiles" }), "7");
    await user.click(screen.getByRole("button", { name: "Load profile" }));
    expect(await screen.findByText("Folder pair is valid.")).toBeInTheDocument();
    expect(vi.mocked(invoke).mock.calls).toContainEqual([
      "validate_folder_pair",
      { sourcePath: "C:\\Music", destinationPath: "D:\\Backup" },
    ]);

    await user.type(screen.getByRole("textbox", { name: "Save current folders as" }), "Updated music");
    await user.click(screen.getByRole("button", { name: "Save profile" }));
    expect(vi.mocked(invoke).mock.calls).toContainEqual([
      "save_profile",
      { name: "Updated music", sourcePath: "C:\\Music", destinationPath: "D:\\Backup" },
    ]);
  });

  it("filters recent run history by status and folder path", async () => {
    const user = userEvent.setup();
    render(<App />);

    expect(await screen.findByText("completed")).toBeInTheDocument();
    await user.selectOptions(screen.getByRole("combobox", { name: "Filter history by status" }), "cancelled");
    expect(screen.getByText("No runs match these filters.")).toBeInTheDocument();
    await user.selectOptions(screen.getByRole("combobox", { name: "Filter history by status" }), "all");
    await user.type(screen.getByRole("searchbox", { name: "Search run history" }), "not-this-folder");
    expect(screen.getByText("No runs match these filters.")).toBeInTheDocument();
    await user.clear(screen.getByRole("searchbox", { name: "Search run history" }));
    expect(screen.getByText("4 / 4 files · 4,096 bytes written")).toBeInTheDocument();
  });

  it("applies and persists a selected theme", async () => {
    const user = userEvent.setup();
    localStorage.removeItem("syncwise-theme");
    render(<App />);

    await user.selectOptions(screen.getByRole("combobox", { name: "Theme" }), "dark");
    expect(document.documentElement).toHaveAttribute("data-theme", "dark");
    expect(localStorage.getItem("syncwise-theme")).toBe("dark");
  });
});
