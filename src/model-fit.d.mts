export function gib(value: number): string;
export function modelFit(
  devices: { name: string; memory: string | number }[],
  bits: number,
  overhead: string | number,
  customWeights?: string | number,
): { name: string; weights: number; fit: string }[];

export function ggufShardError(names: string[]): string | undefined;
