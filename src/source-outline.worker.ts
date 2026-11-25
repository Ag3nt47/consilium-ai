import ts from "typescript-ast";
import { createOutlineSession } from "../scripts/source-outline.mjs";

export type OutlineRequest = { file?: File; functionName: string; filter: string };
export type OutlineReply = { rows: string[]; omitted: number } | { error: string };

const outline = createOutlineSession(ts);

self.onmessage = async ({ data }: MessageEvent<OutlineRequest>) => {
  try {
    const result: OutlineReply = await outline(data);
    self.postMessage(result);
  } catch (failure) {
    const result: OutlineReply = { error: failure instanceof Error ? failure.message : "Could not read the source file." };
    self.postMessage(result);
  }
};
