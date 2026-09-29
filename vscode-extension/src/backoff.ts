/**
 * How long to wait before retry number `attempt` (0 for the first): doubling
 * from `base` up to `max`, then spread over its upper half so a class whose
 * server just restarted does not all reconnect on the same tick.
 */
export function backoffMs(
    attempt: number,
    base = 5_000,
    max = 60_000,
    random: () => number = Math.random,
): number {
    const ceiling = Math.min(max, base * 2 ** Math.min(attempt, 30));
    return Math.round(ceiling * (0.5 + random() * 0.5));
}
