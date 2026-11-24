export function gib(value: number): string;
export function modelFit(
  devices: { name: string; memory: string | number }[],
  bits: number,
  overhead: string | number,
  customWeights?: string | number,
  preferSplit?: boolean,
  distribution?: "balanced" | "capacity",
): { name: string; weights: number; fit: string; allocation?: { name: string; allocated: number }[] }[];

export function equalSplitMemoryGiB(weights: number, count: number, overhead: number | string): number;

export function weightBudgets(
  devices: { name: string; memory: string | number }[],
  overhead: string | number,
): { name: string; capacity: number }[];

export function ggufShardError(names: string[]): string | undefined;
