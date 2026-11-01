import { useDeferredValue, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useVirtualizer } from "@tanstack/react-virtual";
import { ArrowLeft, ArrowRight, Check, Clock3, Folder, Search, ShieldCheck, Undo2, X } from "lucide-react";
import type { Plan } from "./types/generated/Plan";
import type { PlanItemKind } from "./types/generated/PlanItemKind";
import type { Decision } from "./types/generated/Decision";
import type { DecisionChange } from "./types/generated/DecisionChange";
import type { DecisionState } from "./types/generated/DecisionState";
import type { ReviewPreflight } from "./types/generated/ReviewPreflight";

const tabs: { kind: PlanItemKind; label: string }[] = [
  { kind: "copy", label: "Missing" },
  { kind: "possible_duplicate", label: "Possible duplicates" },
  { kind: "already_present", label: "Already present" },
  { kind: "conflict", label: "Conflicts" },
  { kind: "error", label: "Errors" },
];

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes.toLocaleString()} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit += 1; }
  return `${value.toFixed(1)} ${units[unit]}`;
}

function errorMessage(error: unknown): string {
  return typeof error === "object" && error !== null && "message" in error
    ? String(error.message) : String(error);
}

export default function Review({ plan, initialDecisions, onBack, onStarted }: {
  plan: Plan; initialDecisions: DecisionState; onBack: (decisions: DecisionState) => void;
  onStarted: (runId: string, plan: Plan) => void;
}) {
  const [decisions, setDecisions] = useState(initialDecisions);
  const [tab, setTab] = useState<PlanItemKind>("copy");
  const [search, setSearch] = useState("");
  const query = useDeferredValue(search.trim().toLocaleLowerCase());
  const [selectedId, setSelectedId] = useState(plan.items.find((item) => item.kind === "copy")?.id ?? plan.items[0]?.id ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [history, setHistory] = useState<DecisionChange[][]>([]);
  const [summary, setSummary] = useState<ReviewPreflight | null>(null);
  const [acknowledged, setAcknowledged] = useState(false);
  const [prepared, setPrepared] = useState(false);
  const [memoryReset, setMemoryReset] = useState(false);
  const [reviewItems] = useState(() => [...plan.items, ...plan.issues.map((issue, index) => ({
    id: `plan-issue-${index}`, sourceRelativePath: issue.message, destinationRelativePath: "",
    sizeBytes: 0, sourceModifiedTimeUnixMs: null, kind: "error" as const, reason: issue.message,
  }))]);
  const [indexes] = useState(() => ({
    items: new Map(reviewItems.map((item) => [item.id, item])),
    matches: new Map((plan.matching?.matches ?? []).map((entry) => [entry.sourceRelativePath, entry.candidates])),
    groups: new Map((plan.matching?.groups ?? []).flatMap((group) => group.sourceRelativePaths.map((path) => [path, group] as const))),
  }));
  const scrollRef = useRef<HTMLDivElement>(null);
  const dialogRef = useRef<HTMLDialogElement>(null);
  const preflightButtonRef = useRef<HTMLButtonElement>(null);
  const rows = reviewItems.filter((item) => item.kind === tab && (!query || item.sourceRelativePath.toLocaleLowerCase().includes(query)));
  const virtual = useVirtualizer({ count: rows.length, getScrollElement: () => scrollRef.current,
    estimateSize: () => 58, overscan: 5, scrollMargin: 38, initialRect: { width: 800, height: 420 } });
  const selected = indexes.items.get(selectedId);
  const candidates = selected ? indexes.matches.get(selected.sourceRelativePath) ?? [] : [];
  const [candidateIndex, setCandidateIndex] = useState(0);
  const candidate = candidates[candidateIndex] ?? candidates[0];
  const group = selected ? indexes.groups.get(selected.sourceRelativePath) : undefined;
  const actionable = selected?.kind === "copy" || selected?.kind === "possible_duplicate";
  const pending = plan.items.filter((item) => (item.kind === "copy" || item.kind === "possible_duplicate") && !decisions.choices[item.id]).length;
  const approved = Object.values(decisions.choices).filter((choice) => choice === "approve").length;
  const counts = new Map(tabs.map(({ kind }) => [kind, reviewItems.filter((item) => item.kind === kind).length]));

  useEffect(() => {
    if (summary && !dialogRef.current?.open) dialogRef.current?.showModal();
  }, [summary]);

  function closeSummary() {
    dialogRef.current?.close();
    setSummary(null); setAcknowledged(false);
    preflightButtonRef.current?.focus();
  }

  async function change(changes: DecisionChange[], bulk = false, undo = false) {
    if (busy || changes.length === 0) return;
    const inverse = changes.map(({ itemId }) => ({ itemId, decision: decisions.choices[itemId] ?? null }));
    setBusy(true); setError(""); setPrepared(false);
    try {
      const state = await invoke<DecisionState>("review_decisions", { planId: plan.id, revision: decisions.revision, changes, bulk });
      setDecisions(state);
      setHistory((previous) => undo ? previous.slice(0, -1) : [...previous.slice(-29), inverse]);
    } catch (failure) { setError(errorMessage(failure)); }
    finally { setBusy(false); }
  }

  async function preflight(confirm = false) {
    setBusy(true); setError("");
    try {
      const checked = await invoke<ReviewPreflight>(confirm ? "confirm_review" : "review_preflight", { planId: plan.id, revision: decisions.revision });
      setSummary(checked);
      if (confirm && checked.canPrepare) setPrepared(true);
    } catch (failure) { setError(errorMessage(failure)); }
    finally { setBusy(false); }
  }

  async function startSync() {
    setBusy(true); setError("");
    try {
      const runId = await invoke<string>("start_sync", { planId: plan.id, revision: decisions.revision });
      onStarted(runId, plan);
    } catch (failure) { setError(errorMessage(failure)); setBusy(false); }
  }

  async function resetMemory() {
    setBusy(true); setError("");
    try { await invoke("reset_review_memory", { planId: plan.id }); setMemoryReset(true); }
    catch (failure) { setError(errorMessage(failure)); }
    finally { setBusy(false); }
  }

  function chooseTab(kind: PlanItemKind) {
    setTab(kind); setSearch(""); setCandidateIndex(0);
    setSelectedId(reviewItems.find((item) => item.kind === kind)?.id ?? "");
    scrollRef.current?.scrollTo({ top: 0 });
  }

  function moveSelection(event: React.KeyboardEvent<HTMLDivElement>) {
    const current = rows.findIndex((item) => item.id === selectedId);
    let next = current;
    if (event.key === "ArrowDown") next = Math.min(rows.length - 1, current + 1);
    else if (event.key === "ArrowUp") next = Math.max(0, current - 1);
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = rows.length - 1;
    else if (event.key === "PageDown") next = Math.min(rows.length - 1, current + 7);
    else if (event.key === "PageUp") next = Math.max(0, current - 7);
    else return;
    event.preventDefault();
    if (rows[next]) { setSelectedId(rows[next].id); setCandidateIndex(0); virtual.scrollToIndex(next, { align: "auto" }); }
  }

  return <main className="review-page">
    <header className="review-header">
      <div className="review-title"><button className="icon-button" type="button" title="Back to setup" aria-label="Back to setup" disabled={busy} onClick={() => onBack(decisions)}><ArrowLeft size={18} /></button><div><p className="eyebrow">SYNCWISE</p><h1>Review files</h1></div></div>
      <div className="root-labels"><div><span className="source-direction">FROM</span><span title={plan.sourceRoot}>{plan.sourceRoot}</span></div><div><span className="destination-direction">TO</span><span title={plan.destinationRoot}>{plan.destinationRoot}</span></div></div>
    </header>
    <div className="review-toolbar">
      <label className="search-field"><Search size={16} /><input type="search" aria-label="Search source files" value={search} onChange={(event) => setSearch(event.target.value)} placeholder="Search files" /></label>
      <button className="secondary-button" type="button" disabled={busy || history.length === 0} onClick={() => change(history[history.length - 1], false, true)}><Undo2 size={16} />Undo decision</button>
      <button className="secondary-button" type="button" disabled={busy || memoryReset} onClick={resetMemory}>Reset remembered rejections</button>
    </div>
    {error && <p className="review-error" role="alert">{error}</p>}
    {memoryReset && <p role="status" className="review-notice">Remembered rejections cleared for future scans. Current decisions are unchanged.</p>}
    {plan.matching?.postingListsTruncated && <p className="review-notice">Candidate retrieval was limited. Some duplicate suggestions may be missing.</p>}
    <div className="review-layout">
      <section className="review-list" aria-label="Comparison results">
        <div className="review-tabs" role="tablist" aria-label="Result categories">{tabs.map(({ kind, label }, index) => <button key={kind} type="button" role="tab" tabIndex={tab === kind ? 0 : -1} aria-selected={tab === kind} aria-controls="result-panel" id={`tab-${kind}`} onClick={() => chooseTab(kind)} onKeyDown={(event) => {
          const next = event.key === "ArrowRight" ? (index + 1) % tabs.length : event.key === "ArrowLeft" ? (index + tabs.length - 1) % tabs.length : event.key === "Home" ? 0 : event.key === "End" ? tabs.length - 1 : -1;
          if (next >= 0) { event.preventDefault(); chooseTab(tabs[next].kind); event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>("[role=tab]")[next]?.focus(); }
        }}>{label}<span>{counts.get(kind)?.toLocaleString()}</span></button>)}</div>
        <div className="list-actions"><span>{rows.length.toLocaleString()} results</span>
          {tab === "copy" && <button className="text-button" type="button" disabled={busy || !rows.some((item) => !decisions.choices[item.id])} onClick={() => change(rows.filter((item) => !decisions.choices[item.id]).map((item) => ({ itemId: item.id, decision: "approve" })), true)}><Check size={15} />Approve missing</button>}
          <button className="text-button" type="button" disabled={busy || !rows.some((item) => (item.kind === "copy" || item.kind === "possible_duplicate") && !decisions.choices[item.id])} onClick={() => change(rows.filter((item) => (item.kind === "copy" || item.kind === "possible_duplicate") && !decisions.choices[item.id]).map((item) => ({ itemId: item.id, decision: "defer" })), true)}><Clock3 size={15} />Defer undecided</button>
        </div>
        <div role="tabpanel" id="result-panel" aria-labelledby={`tab-${tab}`}>
          <div className="review-table" role="table" aria-label="File comparison" aria-rowcount={rows.length + 1} aria-colcount={4} tabIndex={0} ref={scrollRef} onKeyDown={moveSelection} aria-activedescendant={virtual.getVirtualItems().some((row) => rows[row.index]?.id === selectedId) ? `row-${selectedId}` : undefined}>
            <div className="table-header table-grid" role="row"><span role="columnheader">Source</span><span role="columnheader">Destination candidate</span><span role="columnheader">Evidence</span><span role="columnheader">Decision</span></div>
            <div role="rowgroup" style={{ height: virtual.getTotalSize(), position: "relative" }}>
              {virtual.getVirtualItems().map((row) => {
                const item = rows[row.index]; const best = indexes.matches.get(item.sourceRelativePath)?.[0];
                return <div key={item.id} id={`row-${item.id}`} role="row" aria-rowindex={row.index + 2} aria-selected={selectedId === item.id} className={`table-grid result-row ${selectedId === item.id ? "is-selected" : ""}`} style={{ position: "absolute", top: row.start - 38, width: "100%", height: row.size }} onClick={() => { setSelectedId(item.id); setCandidateIndex(0); scrollRef.current?.focus(); }}>
                  <div role="cell" title={item.sourceRelativePath}><span className="row-path">{item.sourceRelativePath}</span><small>{formatBytes(item.sizeBytes)}</small></div>
                  <span role="cell" className="row-path" title={best?.destinationRelativePath ?? item.destinationRelativePath}>{best?.destinationRelativePath ?? (item.kind === "copy" ? "No candidate" : item.destinationRelativePath)}</span>
                  <span role="cell" className="evidence-label">{best ? best.label === "likely_same_song" ? "Likely" : "Uncertain" : item.kind === "copy" ? "Missing" : tabs.find((entry) => entry.kind === item.kind)?.label}</span>
                  <span role="cell" className={`decision-label choice-${decisions.choices[item.id] ?? "pending"}`}>{decisions.choices[item.id] === "approve" ? "Approved" : decisions.choices[item.id] === "reject" ? "Rejected" : decisions.choices[item.id] === "defer" ? "Deferred" : item.kind === "conflict" || item.kind === "error" ? "Blocked" : item.kind === "already_present" ? "Skip" : "Undecided"}</span>
                </div>;
              })}
            </div>
            {rows.length === 0 && <div className="review-empty"><Folder size={28} /><p>No {search ? "matching files" : tabs.find((entry) => entry.kind === tab)?.label.toLowerCase()}.</p></div>}
          </div>
        </div>
      </section>
      <aside className="evidence-panel" aria-label="Match evidence">
        <h2>File details</h2>
        {selected ? <>
          <dl><dt>Source</dt><dd>{selected.sourceRelativePath}</dd><dt>Size</dt><dd>{formatBytes(selected.sizeBytes)}</dd></dl>
          {candidates.length > 1 && <label className="candidate-select">Destination candidate<select aria-label="Destination candidate" value={candidateIndex} onChange={(event) => setCandidateIndex(Number(event.target.value))}>{candidates.map((entry, index) => <option key={entry.destinationRelativePath} value={index}>{entry.destinationRelativePath}</option>)}</select></label>}
          {candidate && <><dl><dt>Destination</dt><dd>{candidate.destinationRelativePath}</dd><dt>Confidence</dt><dd>{candidate.label === "likely_same_song" ? "Likely same song" : "Uncertain"} ({Math.round(candidate.score * 100)}%)</dd></dl><h3>Evidence</h3><ul className="evidence-reasons">{candidate.reasons.map((reason) => <li key={reason}>{reason}</li>)}</ul></>}
          {!candidate && <p className="item-reason">{selected.reason ?? (selected.kind === "copy" ? "No duplicate candidate was retained." : "This item will not be copied.")}</p>}
          {group && <div className="group-warning"><h3>Shared destination candidate</h3><ul>{group.sourceRelativePaths.slice(0, 20).map((path) => <li key={path}>{path}</li>)}</ul>{group.sourceRelativePaths.length > 20 && <p>{group.sourceRelativePaths.length - 20} additional source files share this candidate.</p>}</div>}
          {actionable && <div className="decision-controls" role="group" aria-label="Decision for selected file">{([{ choice: "approve", label: "Approve copy", Icon: Check }, { choice: "reject", label: "Reject copy", Icon: X }, { choice: "defer", label: "Defer", Icon: Clock3 }] as const).map(({ choice, label, Icon }) => <button key={choice} type="button" className="secondary-button" aria-pressed={decisions.choices[selected.id] === choice} disabled={busy} onClick={() => change([{ itemId: selected.id, decision: choice as Decision }])}><Icon size={16} />{label}</button>)}</div>}
        </> : <p className="review-muted">No file selected.</p>}
      </aside>
    </div>
    <footer className="review-footer"><div role="status" aria-live="polite"><strong>{approved.toLocaleString()} approved</strong><span>{pending.toLocaleString()} undecided</span>{busy && <span>Checking...</span>}{prepared && <span className="prepared-status"><ShieldCheck size={16} />Plan confirmed</span>}</div><button ref={preflightButtonRef} type="button" disabled={busy || approved === 0} onClick={() => preflight()}><ArrowRight size={17} />Review preflight</button></footer>
    <dialog className="preflight-dialog" ref={dialogRef} onCancel={(event) => { event.preventDefault(); closeSummary(); }} aria-labelledby="preflight-title">
      <div className="dialog-heading"><h2 id="preflight-title">Confirm sync plan</h2><button className="icon-button" type="button" aria-label="Close preflight" title="Close preflight" onClick={closeSummary}><X size={18} /></button></div>
      {summary && <>
        <dl className="preflight-roots"><dt>FROM</dt><dd>{plan.sourceRoot}</dd><dt>TO</dt><dd>{plan.destinationRoot}</dd></dl>
        <dl className="preflight-stats"><dt>Files to copy</dt><dd>{summary.approvedCount.toLocaleString()}</dd><dt>Required</dt><dd>{formatBytes(summary.requiredBytes)}</dd><dt>Available</dt><dd>{summary.availableBytes === null ? "Unknown" : formatBytes(summary.availableBytes)}</dd><dt>Conflicts / errors (skipped)</dt><dd>{summary.conflictCount}</dd><dt>Rejected / deferred</dt><dd>{summary.rejectedCount} / {summary.deferredCount}</dd><dt>Undecided</dt><dd>{summary.pendingCount}</dd></dl>
        {summary.pendingCount > 0 && <p className="review-error">Decide or defer the remaining files before confirming.</p>}
        <ul className="preflight-issues">{summary.issues.slice(0, 50).map((issue, index) => <li className={issue.blocking ? "blocking-issue" : ""} key={index}>{issue.message}</li>)}</ul>{summary.issues.length > 50 && <p className="review-error">{summary.issues.length - 50} additional issues require a new scan.</p>}
        <label className="confirmation-check"><input type="checkbox" checked={acknowledged} onChange={(event) => setAcknowledged(event.target.checked)} />I confirm the source, destination, and selected files.</label>
        {error && <p className="review-error" role="alert">{error}</p>}
        <div className="dialog-actions"><button className="secondary-button" type="button" disabled={busy} onClick={closeSummary}>Back to review</button><button type="button" disabled={busy || prepared || !summary.canPrepare || !acknowledged} onClick={() => preflight(true)}><Check size={16} />Confirm plan</button><button type="button" disabled={busy || !prepared} onClick={startSync}>Start Sync</button></div>
      </>}
    </dialog>
  </main>;
}