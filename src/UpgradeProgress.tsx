import ledger from "../docs/UPGRADE_STATUS.md?raw";
import { parseUpgradeProgress } from "./upgrade-progress.mjs";

const documents = parseUpgradeProgress(ledger);

export default function UpgradeProgress() {
  return (
    <details className="small">
      <summary>Upgrade document progress</summary>
      <p className="muted">Status from this build. Percentages count completed items, excluding infeasible items.</p>
      <table className="usage-table">
        <thead>
          <tr>
            <th scope="col">Document</th>
            <th scope="col">Complete</th>
            <th scope="col">In progress</th>
          </tr>
        </thead>
        <tbody>
          {documents.map(({ number, title, done, total, active, percentage }) => (
            <tr key={number}>
              <th scope="row">{title}</th>
              <td>{percentage}% ({done}/{total})</td>
              <td>{active}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </details>
  );
}
