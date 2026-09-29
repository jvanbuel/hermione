import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { backoffMs } from './backoff';

describe('backoffMs', () => {
    it('doubles from the base up to the cap', () => {
        const at = (n: number) => backoffMs(n, 1000, 10_000, () => 1);
        assert.deepEqual([0, 1, 2, 3, 4, 5, 20].map(at), [1000, 2000, 4000, 8000, 10_000, 10_000, 10_000]);
    });

    it('is spread over the upper half, so a class does not reconnect in step', () => {
        assert.equal(backoffMs(2, 1000, 10_000, () => 0), 2000); // half of 4000
        assert.equal(backoffMs(2, 1000, 10_000, () => 1), 4000);
        assert.equal(backoffMs(2, 1000, 10_000, () => 0.5), 3000);
    });

    it('does not overflow for a very long outage', () => {
        assert.equal(backoffMs(10_000, 1000, 60_000, () => 1), 60_000);
    });
});
