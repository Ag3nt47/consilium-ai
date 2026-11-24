export const SOURCE_LIMIT: number;
export const supportedSourceExtensions: ReadonlySet<string>;
export type SourceOutline = { rows: string[]; omitted: number };
export function outlineSourceText(file: string, text: string, functionName?: string, filter?: string): SourceOutline;
