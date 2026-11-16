// AI Branches & Worktrees drawer: shows isolated git worktrees created by Full-capacity
// AIs on branch `consilium/<agent>`. Lets the human review commits, merge them into the
// main project checkout, or discard them.
import { useCallback, useEffect, useState } from "react";
import { type AgentBranch, type AgentInfo, api, senderName } from "./api";

interface Props {
  conversation?: string | null;
  agents: AgentInfo[];
  onClose: () => void;
}

export function BranchesDrawer({ conversation, agents, onClose }: Props) {
  const [branches, setBranches] = useState<AgentBranch[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [actionMsg, setActionMsg] = useState<string | null>(null);
  const [confirming, setConfirming] = useState<{
    type: "merge" | "discard";
    repo: string;
    agent: string;
    branch: string;
  } | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const list = await api.agentBranches(conversation ?? undefined);
      setBranches(list);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [conversation]);

  useEffect(() => {
    void load();
  }, [load]);

  const handleMerge = async (repo: string, agent: string, branchName: string) => {
    setBusy(true);
    setActionMsg(null);
    setError(null);
    try {
      const commit = await api.mergeAgentBranch(repo, agent, conversation ?? undefined);
      setActionMsg(`Merged ${branchName} successfully (commit ${commit}).`);
      setConfirming(null);
      await load();
    } catch (e) {
      setError(`Merge failed: ${e}`);
    } finally {
      setBusy(false);
    }
  };

  const handleDiscard = async (repo: string, agent: string, branchName: string) => {
    setBusy(true);
    setActionMsg(null);
    setError(null);
    try {
      await api.discardAgentBranch(repo, agent, conversation ?? undefined);
      setActionMsg(`Discarded ${branchName} and removed its worktree.`);
      setConfirming(null);
      await load();
    } catch (e) {
      setError(`Discard failed: ${e}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <aside className="audit-drawer branches-drawer" aria-label="AI Branches and Worktrees">
      <div className="audit-head">
        <strong>AI Branches &amp; Worktrees</strong>
        <span className="spacer" />
        <button className="btn small ghost" onClick={() => void load()} disabled={loading || busy} title="Refresh isolated branches">
          ↻ Refresh
        </button>
        <button className="btn small ghost" onClick={onClose} title="Close branches review">
          Close
        </button>
      </div>
      <p className="muted small">
        In Full capacity with isolated copies on, each AI makes changes in its own git worktree (branch{" "}
        <code>consilium/&lt;agent&gt;</code>). Edits don't touch your checkout until you review and merge them.
      </p>

      {actionMsg && <div className="notice success-notice">{actionMsg}</div>}
      {error && <p className="repo-error">{error}</p>}

      {loading && <p className="muted small">Scanning for isolated AI copies…</p>}

      {!loading && branches.length === 0 && (
        <div className="empty-branches">
          <p className="muted">No isolated AI branches found for this project.</p>
          <small className="muted">
            Enable <b>Isolated copies in Full capacity</b> in Settings to run Full-capacity AIs in separate git worktrees.
          </small>
        </div>
      )}

      {!loading && branches.length > 0 && (
        <div className="branches-list">
          {branches.map((b) => {
            const isConfirmingMerge =
              confirming?.type === "merge" && confirming.repo === b.repo && confirming.agent === b.agent;
            const isConfirmingDiscard =
              confirming?.type === "discard" && confirming.repo === b.repo && confirming.agent === b.agent;

            return (
              <div key={`${b.repo}-${b.agent}`} className="branch-card">
                <div className="branch-card-header">
                  <div>
                    <span className="branch-agent-badge">{senderName(b.agent, agents)}</span>
                    <strong className="branch-repo-name">{b.repo}</strong>
                  </div>
                  <code className="branch-name-tag">{b.branch}</code>
                </div>

                <div className="branch-metrics">
                  <span className={`metric-pill ${b.ahead > 0 ? "metric-ahead" : ""}`}>
                    {b.ahead} ahead
                  </span>
                  <span className="metric-pill">
                    {b.behind} behind
                  </span>
                  {b.uncommitted > 0 && (
                    <span className="metric-pill metric-uncommitted">
                      {b.uncommitted} uncommitted
                    </span>
                  )}
                </div>

                {b.commits.length > 0 && (
                  <div className="branch-commits">
                    <span className="muted small">Commits to merge:</span>
                    <ul>
                      {b.commits.map((c, i) => (
                        <li key={i}>
                          <code>{c}</code>
                        </li>
                      ))}
                    </ul>
                  </div>
                )}

                {isConfirmingMerge && (
                  <div className="branch-confirm-box">
                    <p className="small">
                      Merge <b>{b.branch}</b> ({b.ahead} commit{b.ahead === 1 ? "" : "s"}) into your main checkout?
                    </p>
                    <div className="confirm-actions">
                      <button
                        className="btn small-btn approve"
                        disabled={busy}
                        onClick={() => void handleMerge(b.repo, b.agent, b.branch)}
                      >
                        {busy ? "Merging…" : "Confirm Merge"}
                      </button>
                      <button
                        className="btn small-btn ghost"
                        disabled={busy}
                        onClick={() => setConfirming(null)}
                      >
                        Cancel
                      </button>
                    </div>
                  </div>
                )}

                {isConfirmingDiscard && (
                  <div className="branch-confirm-box">
                    <p className="small danger-text">
                      Discard <b>{b.branch}</b> and delete its worktree? All unmerged changes will be permanently removed.
                    </p>
                    <div className="confirm-actions">
                      <button
                        className="btn small-btn danger"
                        disabled={busy}
                        onClick={() => void handleDiscard(b.repo, b.agent, b.branch)}
                      >
                        {busy ? "Discarding…" : "Confirm Discard"}
                      </button>
                      <button
                        className="btn small-btn ghost"
                        disabled={busy}
                        onClick={() => setConfirming(null)}
                      >
                        Cancel
                      </button>
                    </div>
                  </div>
                )}

                {!isConfirmingMerge && !isConfirmingDiscard && (
                  <div className="branch-actions">
                    <button
                      className="btn small-btn"
                      disabled={b.ahead === 0 || busy}
                      title={b.ahead === 0 ? "No new commits to merge" : "Merge this branch into the main checkout"}
                      onClick={() => setConfirming({ type: "merge", repo: b.repo, agent: b.agent, branch: b.branch })}
                    >
                      Merge into Main
                    </button>
                    <button
                      className="btn small-btn ghost danger"
                      disabled={busy}
                      title="Discard this worktree and branch"
                      onClick={() => setConfirming({ type: "discard", repo: b.repo, agent: b.agent, branch: b.branch })}
                    >
                      Discard
                    </button>
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </aside>
  );
}
