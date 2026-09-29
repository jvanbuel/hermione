import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { BaselineCache, DiffMemo, diffHunks } from './baselines';

/** A loader that counts how many times it really ran. */
function loader(result: string | null) {
    const calls = { n: 0 };
    return { calls, load: async () => (calls.n++, result) };
}

describe('BaselineCache', () => {
    it('asks git once per commit, however often it is asked', async () => {
        const cache = new BaselineCache();
        const l = loader('committed text');
        for (let i = 0; i < 10; i++) {
            assert.equal(await cache.get('/w/a.py', 'abc', l.load), 'committed text');
        }
        assert.equal(l.calls.n, 1);
    });

    it('asks again when HEAD moves', async () => {
        const cache = new BaselineCache();
        const l = loader('x');
        await cache.get('/w/a.py', 'abc', l.load);
        await cache.get('/w/a.py', 'def', l.load);
        assert.equal(l.calls.n, 2);
    });

    it('keeps files apart', async () => {
        const cache = new BaselineCache();
        const l = loader('x');
        await cache.get('/w/a.py', 'abc', l.load);
        await cache.get('/w/b.py', 'abc', l.load);
        assert.equal(l.calls.n, 2);
    });

    it('never caches an unborn branch, which has no commit to key on', async () => {
        const cache = new BaselineCache();
        const l = loader(null);
        await cache.get('/w/a.py', undefined, l.load);
        await cache.get('/w/a.py', undefined, l.load);
        assert.equal(l.calls.n, 2);
    });

    it('believes "no such file" only briefly, and a found file for as long as the commit stands', async () => {
        let clock = 0;
        const cache = new BaselineCache(() => clock);
        const absent = loader(null);
        const found = loader('text');

        await cache.get('/w/new.py', 'abc', absent.load);
        clock += 9_000;
        await cache.get('/w/new.py', 'abc', absent.load);
        assert.equal(absent.calls.n, 1, 'still believed');
        clock += 2_000;
        await cache.get('/w/new.py', 'abc', absent.load);
        assert.equal(absent.calls.n, 2, 'no longer believed');

        await cache.get('/w/old.py', 'abc', found.load);
        clock += 3_600_000;
        await cache.get('/w/old.py', 'abc', found.load);
        assert.equal(found.calls.n, 1, 'a found file is exact for its commit');
    });

    it('caches nothing when git throws, so the failure is not remembered', async () => {
        const cache = new BaselineCache();
        let calls = 0;
        const flaky = async () => {
            if (calls++ === 0) {
                throw new Error('index.lock exists');
            }
            return 'text';
        };
        await assert.rejects(cache.get('/w/a.py', 'abc', flaky));
        assert.equal(await cache.get('/w/a.py', 'abc', flaky), 'text');
        assert.equal(calls, 2);
    });

    it('evicts the least recently used file past its limit', async () => {
        const cache = new BaselineCache(Date.now, 2);
        const l = loader('x');
        await cache.get('/a', 'c', l.load);
        await cache.get('/b', 'c', l.load);
        await cache.get('/a', 'c', l.load); // /a is now the most recent
        await cache.get('/c', 'c', l.load); // evicts /b
        assert.equal(l.calls.n, 3);
        await cache.get('/a', 'c', l.load);
        assert.equal(l.calls.n, 3, '/a survived');
        await cache.get('/b', 'c', l.load);
        assert.equal(l.calls.n, 4, '/b was evicted');
    });
});

describe('DiffMemo', () => {
    it('reuses a diff for the same file, commit and buffer', () => {
        const memo = new DiffMemo();
        let computed = 0;
        const compute = () => (computed++, []);
        memo.get('/a', 'c', 'text', compute);
        memo.get('/a', 'c', 'text', compute);
        assert.equal(computed, 1);
    });

    it('recomputes when any of them changes', () => {
        const memo = new DiffMemo();
        let computed = 0;
        const compute = () => (computed++, []);
        memo.get('/a', 'c1', 'text', compute);
        memo.get('/a', 'c2', 'text', compute); // a new commit
        memo.get('/a', 'c2', 'text!', compute); // an edit
        memo.get('/b', 'c2', 'text!', compute); // another file
        assert.equal(computed, 4);
    });
});

describe('diffHunks', () => {
    it('reports where each hunk starts and its lines, signed', () => {
        const committed = 'a\nb\nc\nd\ne\nf\ng\nh\n';
        const buffer = 'a\nb\nc\nd\nE\nf\ng\nh\n';
        const [hunk] = diffHunks(committed, buffer, 'x.py');
        assert.equal(hunk.oldStart, 2);
        assert.equal(hunk.newStart, 2);
        assert.deepEqual(hunk.lines, [' b', ' c', ' d', '-e', '+E', ' f', ' g', ' h']);
    });

    it('drops the "no newline at end of file" marker', () => {
        const [hunk] = diffHunks('a\nb', 'a\nB', 'x.py');
        assert.ok(hunk.lines.every((l) => !l.startsWith('\\')), JSON.stringify(hunk.lines));
    });

    it('is empty when nothing changed', () => {
        assert.deepEqual(diffHunks('same\n', 'same\n', 'x.py'), []);
    });
});
