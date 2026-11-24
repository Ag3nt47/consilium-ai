import { extname } from "node:path";
import { parseSource, sourceParser, supportedSourceExtensions } from "./parse-source.mjs";
import { outlineSource } from "./source-outline.mjs";

try {
  const [file, scopeArg, filter, ...extra] = process.argv.slice(2);
  const filterOnly = scopeArg === "--filter";
  if (!file || extra.length || (filterOnly && !filter?.trim()) || !supportedSourceExtensions.has(extname(file).toLowerCase())) {
    throw new Error("Usage: node scripts/symbol-map.mjs <JS or TS file> [top-level function | --filter] [symbol filter]");
  }
  const functionName = filterOnly ? undefined : scopeArg;
  const source = parseSource(file);
  const { rows, omitted } = outlineSource(sourceParser(), source, functionName, filter);
  console.log(`${JSON.stringify(file)} — ${rows.length} rows`);
  for (const row of rows) console.log(row);
  if (omitted) console.log(`${omitted} additional rows omitted.`);
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
