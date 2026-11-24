import type { ConnectorStatus, SignalRelayStatus } from "./api";
export type SignalStatus = ConnectorStatus & { mode: "direct" | "relay" | "multiple" | "unavailable" };
export function signalStatus(direct: ConnectorStatus[] | null | undefined, relay: SignalRelayStatus | null, expiresAt: number, now: number): SignalStatus;
export function relayExpiry(report: SignalRelayStatus | null, requestStarted: number): number;
