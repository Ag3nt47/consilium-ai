export function parseUpgradeProgress(ledger: string): Array<{
  number: string;
  title: string;
  done: number;
  total: number;
  active: number;
  excluded: number;
  percentage: number;
  activePercentage: number;
}>;
