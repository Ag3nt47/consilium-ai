export function gib(value: number): string;
type DeviceMemory = { name: string; memory: string | number; reserve?: string | number };
export function modelFit(
  devices: DeviceMemory[],
  bits: number,
  overhead: string | number,
  customWeights?: string | number,
  preferSplit?: boolean,
  distribution?: "balanced" | "capacity",
): { name: string; weights: number; fit: string; allocation?: { name: string; allocated: number }[] }[];

export function equalSplitMemoryGiB(weights: number, count: number, overhead: number | string): number;
export function effectiveReserve(override: string | number | undefined, fallback: string | number): number;

export function weightBudgets(
  devices: DeviceMemory[],
  overhead: string | number,
): { name: string; capacity: number }[];

export function ggufShardError(names: string[]): string | undefined;
