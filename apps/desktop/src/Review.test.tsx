import { beforeAll, beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "@testing-library/jest-dom/vitest";
import { invoke } from "@tauri-apps/api/core";
import Review from "./Review";
import type { Plan } from "./types/generated/Plan";
import type { DecisionState } from "./types/generated/DecisionState";
import type { DecisionChange } from "./types/generated/DecisionChange";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

beforeAll(() => {
  Object.defineProperty(HTMLElement.prototype, "offsetHeight", { configurable: true, get: () => 420 });
  Object.defineProperty(HTMLElement.prototype, "offsetWidth", { configurable: true, get: () => 900 });
  HTMLElement.prototype.scrollTo = function (options?: ScrollToOptions | number) {
    this.scrollTop = typeof options === "object" ? options.top ?? 0 : options ?? 0;
    queueMicrotask(() => this.dispatchEvent(new Event("scroll")));
  };
  HTMLDialogElement.prototype.showModal = function () { this.setAttribute("open", ""); };
  HTMLDialogElement.prototype.close = function () { this.removeAttribute("open"); };
});

afterEach(cleanup);
beforeEach(() => { vi.mocked(invoke).mockReset(); });

function fixture(count: number, duplicate = false): Plan {
  const items = Array.from({ length: count }, (_, index) => ({ id: `item-${index}`, sourceRelativePath: `file-${String(index).padStart(5, "0")}.txt`, destinationRelativePath: `file-${index}.txt`, sizeBytes: 123, sourceModifiedTimeUnixMs: 1, kind: "copy" as const, reason: null }));
  const plan: Plan = { id: "plan-test", sourceRoot: "C:\\Source", destinationRoot: "D:\\Destination", items, issues: [], requiredBytes: count * 123, availableBytes: 10_000_000, canExecute: true, matching: { matches: [], groups: [], withinSideDuplicates: { source: [], destination: [], unavailableFiles: 0 }, candidateComparisons: 0, postingListsTruncated: false } };
  if (duplicate) {
    plan.items.push({ id: "duplicate", sourceRelativePath: "Song - Artist.mp3", destinationRelativePath: "Song - Artist.mp3", sizeBytes: 123, sourceModifiedTimeUnixMs: 1, kind: "possible_duplicate", reason: "Review this match." });
    plan.matching!.matches.push({ sourceRelativePath: "Song - Artist.mp3", candidates: [{ destinationRelativePath: "Artist - Song.mp3", score: 1, titleSimilarity: 1, artistSimilarity: 1, sizePlausibility: 1, label: "likely_same_song", reasons: ["No hashes or audio duration evidence."], suggested: true }] });
  }
  return plan;
}

function connect(plan: Plan, blocked = false) {
  let state: DecisionState = { revision: 0, choices: {} };
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    const input = args as Record<string, unknown>;
    if (command === "review_decisions") {
      expect(input.revision).toBe(state.revision);
      const choices = { ...state.choices };
      for (const change of input.changes as DecisionChange[]) {
        if (change.decision) choices[change.itemId] = change.decision;
        else delete choices[change.itemId];
      }
      state = { revision: state.revision + 1, choices };
      return state as never;
    }
    if (command === "start_sync") {
      expect(input.revision).toBe(state.revision);
      return "run-1" as never;
    }
    if (command === "review_preflight" || command === "confirm_review") {
      const values = Object.values(state.choices);
      return { planId: plan.id, revision: state.revision, approvedCount: values.filter((choice) => choice === "approve").length,
        pendingCount: plan.items.length - values.length, rejectedCount: values.filter((choice) => choice === "reject").length,
        deferredCount: values.filter((choice) => choice === "defer").length, conflictCount: 0,
        requiredBytes: 123, availableBytes: 10_000_000, canPrepare: !blocked && plan.items.length === values.length,
        issues: blocked ? [{ code: "invalid_relative_path", message: "Source changed. Scan again.", blocking: true }] : [] } as never;
    }
    if (command === "reset_review_memory") return undefined as never;
    throw new Error(`Unexpected command ${command}`);
  });
  return state;
}

describe("Review", () => {
  it("filters by decision and displays exact within-side duplicate groups", async () => {
    const user = userEvent.setup();
    const plan = fixture(2);
    plan.matching!.withinSideDuplicates.source.push({
      sizeBytes: 123,
      relativePaths: Array.from({ length: 205 }, (_, index) => `copy-${index}.txt`),
    });
    const initial = connect(plan);
    initial.choices["item-0"] = "reject";
    render(<Review plan={plan} initialDecisions={initial} onBack={vi.fn()} onStarted={vi.fn()} />);

    await user.selectOptions(screen.getByRole("combobox", { name: "Filter by decision" }), "reject");
    expect(screen.getByRole("table", { name: "File comparison" })).toHaveAttribute("aria-rowcount", "2");
    expect(screen.getByText("file-00000.txt")).toBeInTheDocument();
    expect(screen.queryByText("file-00001.txt")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Within-side duplicates/ }));
    expect(screen.getByRole("table", { name: "Within-side duplicate files" })).toBeInTheDocument();
    expect(screen.getByText("copy-0.txt")).toBeInTheDocument();
    expect(screen.getByText("Exact duplicates within each folder")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Next files" }));
    expect(screen.getByText("copy-100.txt")).toBeInTheDocument();
  });

  it("announces when no within-side duplicates are found", async () => {
    const user = userEvent.setup();
    const plan = fixture(0);
    render(<Review plan={plan} initialDecisions={connect(plan)} onBack={vi.fn()} onStarted={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: /Within-side duplicates/ }));
    expect(screen.getByText("No exact within-side duplicates were found.")).toBeInTheDocument();
  });

  it("keeps 20,000 rows virtual and supports keyboard selection and bulk missing approval", async () => {
    const user = userEvent.setup(); const plan = fixture(20_000);
    render(<Review plan={plan} initialDecisions={connect(plan)} onBack={vi.fn()} onStarted={vi.fn()} />);
    const table = screen.getByRole("table", { name: "File comparison" });
    expect(table).toHaveAttribute("aria-rowcount", "20001");
    expect(within(table).getAllByRole("row").length).toBeLessThan(35);
    table.focus(); await user.keyboard("{End}");
    expect(within(screen.getByRole("complementary")).getByText("file-19999.txt")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Approve missing" }));
    expect(await screen.findByText("20,000 approved")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Review preflight" }));
    const modal = await screen.findByRole("dialog");
    await user.click(within(modal).getByRole("checkbox", { name: /I confirm the source/ }));
    await user.click(within(modal).getByRole("button", { name: "Confirm plan" }));
    expect(await screen.findByText("Plan confirmed")).toBeInTheDocument();
    expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "start_sync")).toBe(false);
  });

  it("reviews evidence, reverses individual choices, and requires explicit confirmation", async () => {
    const user = userEvent.setup(); const plan = fixture(1, true);
    const onStarted = vi.fn();
    render(<Review plan={plan} initialDecisions={connect(plan)} onBack={vi.fn()} onStarted={onStarted} />);
    await user.click(screen.getByRole("button", { name: "Approve missing" }));
    screen.getByRole("tab", { name: /Missing/ }).focus();
    await user.keyboard("{ArrowRight}");
    expect(screen.getByRole("tab", { name: /Possible duplicates/ })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByText("No hashes or audio duration evidence.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Approve missing" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Reject copy" }));
    await user.click(screen.getByRole("button", { name: "Undo decision" }));
    expect(await screen.findByText("1 undecided")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Defer" }));
    await user.click(screen.getByRole("button", { name: "Review preflight" }));
    const modal = await screen.findByRole("dialog");
    expect(within(modal).getByRole("button", { name: "Confirm plan" })).toBeDisabled();
    expect(within(modal).getByRole("button", { name: "Start Sync" })).toBeDisabled();
    expect(within(modal).getByText("C:\\Source")).toBeInTheDocument();
    await user.click(within(modal).getByRole("checkbox", { name: /I confirm the source/ }));
    await user.click(within(modal).getByRole("button", { name: "Confirm plan" }));
    expect(await screen.findByText("Plan confirmed")).toBeInTheDocument();
    expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "start_sync")).toBe(false);
    await user.click(within(modal).getByRole("checkbox", { name: /Verify each copied file/ }));
    await user.click(within(modal).getByRole("button", { name: "Start Sync" }));
    expect(onStarted).toHaveBeenCalledWith("run-1", plan);
    expect(vi.mocked(invoke).mock.calls.find(([command]) => command === "start_sync")?.[1]).toMatchObject({
      verifyAfterCopy: true,
    });
  });

  it("cannot confirm blocking preflight issues even after acknowledgement", async () => {
    const user = userEvent.setup(); const plan = fixture(1);
    render(<Review plan={plan} initialDecisions={connect(plan, true)} onBack={vi.fn()} onStarted={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Approve missing" }));
    await user.click(screen.getByRole("button", { name: "Review preflight" }));
    const modal = await screen.findByRole("dialog");
    await user.click(within(modal).getByRole("checkbox", { name: /I confirm the source/ }));
    expect(within(modal).getByText("Source changed. Scan again.")).toBeInTheDocument();
    expect(within(modal).getByRole("button", { name: "Confirm plan" })).toBeDisabled();
  });
});