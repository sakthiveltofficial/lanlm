// Pure helpers for the cluster page.

export type ParsedWorker = { endpoint: string; ramMb: number };

/**
 * Reads what the user typed or pasted into "Add another PC".
 * Accepts `host:port` (RAM comes from `ramGb`) or a pairing code `host:port@ramMb`.
 */
export function parseWorkerInput(raw: string, ramGb: number): ParsedWorker | { error: string } {
  const m = /^([\w.-]+:\d{2,5})(?:@(\d+))?$/.exec(raw.trim());
  if (!m) return { error: "Use the form 192.168.1.20:50052 (or paste a pairing code)." };
  const port = Number(m[1].slice(m[1].lastIndexOf(":") + 1));
  if (port < 1 || port > 65535) return { error: "The port must be between 1 and 65535." };
  const ramMb = m[2] ? Number(m[2]) : Math.round(ramGb * 1024);
  if (!(ramMb > 0)) return { error: "Enter how much free RAM that PC can lend." };
  return { endpoint: m[1], ramMb };
}

/** How much of a worker's reported free RAM the planner treats as lendable. */
export const LENDABLE_FRACTION = 0.75;

/** Memory a worker lends, in MB. */
export const lendableMb = (freeRamMb: number): number => freeRamMb * LENDABLE_FRACTION;
