export function parseUpgradeProgress(ledger) {
  const documents = new Map();
  let document;

  for (const line of ledger.split(/\r?\n/)) {
    if (line.startsWith("## ")) {
      const heading = /^## Document (\d+): (.+)$/.exec(line);
      document = heading ? { title: heading[2], done: 0, total: 0, active: 0, excluded: 0 } : undefined;
      if (heading) {
        if (documents.has(heading[1])) throw new Error(`Repeated document number ${heading[1]}`);
        documents.set(heading[1], document);
      }
    }
    if (!document || !line.startsWith("|")) continue;
    const [, item, status] = line.split("|", 3).map((cell) => cell.trim());
    if (status === "Status" || /^:?-+:?$/.test(status)) continue;
    if (status === "⛔") {
      document.excluded++;
      continue;
    }
    if (!["✅", "🔨", "🟡", "⏭", "🕓"].includes(status)) {
      throw new Error(`Unknown status for "${item}" in "${document.title}"`);
    }
    document.total++;
    if (status === "✅") document.done++;
    if (status === "🔨" || status === "🟡") document.active++;
  }

  if (documents.size !== 6 || [...documents.values()].some(({ total }) => total === 0)) {
    throw new Error("Expected six document sections with at least one feasible item each.");
  }
  return [...documents].map(([number, document]) => ({
    number,
    ...document,
    percentage: Math.round(100 * document.done / document.total),
    activePercentage: Math.round(100 * document.active / document.total),
  }));
}
