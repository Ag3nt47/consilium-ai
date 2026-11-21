import { readFileSync } from "node:fs";
import { parseUpgradeProgress } from "../src/upgrade-progress.mjs";

const ledger = readFileSync(new URL("../docs/UPGRADE_STATUS.md", import.meta.url), "utf8");
console.log("| Document | Complete | In progress | Excluded |");
console.log("|---|---:|---:|---:|");
for (const { number, title, done, total, active, excluded, percentage } of parseUpgradeProgress(ledger)) {
  console.log(`| ${number}. ${title} | ${percentage}% (${done}/${total}) | ${active} | ${excluded} |`);
}
