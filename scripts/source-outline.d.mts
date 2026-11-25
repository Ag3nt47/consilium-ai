export const SOURCE_LIMIT: number;
export const supportedSourceExtensions: ReadonlySet<string>;
export type SourceOutline = { rows: string[]; omitted: number };
export function outlineSourceText(parser: typeof import("typescript-ast"), file: string, text: string, functionName?: string, filter?: string): SourceOutline;
export function createOutlineSession(parser: typeof import("typescript-ast")): (request: {
  file?: Pick<File, "name" | "size" | "text">;
  functionName?: string;
  filter?: string;
}) => Promise<SourceOutline>;
