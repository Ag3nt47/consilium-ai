import ts from "typescript-ast";
import { outlineSourceText, SOURCE_LIMIT, supportedSourceExtensions } from "../scripts/source-outline.mjs";

export type OutlineRequest = { file: File; functionName: string; filter: string };
export type OutlineReply = { rows: string[]; omitted: number } | { error: string };

self.onmessage = async ({ data }: MessageEvent<OutlineRequest>) => {
  try {
    const { file, functionName, filter } = data;
    const extension = /\.[^.]+$/.exec(file.name)?.[0]?.toLowerCase() ?? "";
    if (!supportedSourceExtensions.has(extension)) throw new Error("Choose a JavaScript or TypeScript file.");
    if (file.size > SOURCE_LIMIT) throw new Error("Choose a source file up to 1 MiB.");
    const result: OutlineReply = outlineSourceText(ts, file.name, await file.text(), functionName || undefined, filter || undefined);
    self.postMessage(result);
  } catch (failure) {
    const result: OutlineReply = { error: failure instanceof Error ? failure.message : "Could not read the source file." };
    self.postMessage(result);
  }
};
