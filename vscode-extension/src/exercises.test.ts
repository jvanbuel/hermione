import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { ExerciseMap } from './exercises';

describe('ExerciseMap', () => {
    const map = new ExerciseMap();
    map.load([
        {
            exercises: [
                { name: 'ex1', match: 'ex1/**' },
                { name: 'ex2', match: ['ex2/*.py', 'solutions/ex2/*'] },
                { name: 'unmatched' },
            ],
        },
    ]);

    it('maps a path to the first exercise whose pattern matches', () => {
        assert.equal(map.resolve('ex1/deep/er/main.py'), 'ex1');
        assert.equal(map.resolve('ex2/a.py'), 'ex2');
        assert.equal(map.resolve('solutions/ex2/a.py'), 'ex2');
    });

    it('does not let * cross a directory, and tolerates Windows separators', () => {
        assert.equal(map.resolve('ex2/sub/a.py'), undefined);
        assert.equal(map.resolve('ex1\\main.py'), 'ex1');
    });

    it('an exercise with no pattern matches nothing rather than everything', () => {
        assert.equal(map.resolve('unmatched'), undefined);
        assert.equal(map.resolve('anything/else.py'), undefined);
    });

    it('loading again replaces the rules', () => {
        const m = new ExerciseMap();
        m.load([{ exercises: [{ name: 'a', match: 'a/**' }] }]);
        m.load([{ exercises: [{ name: 'b', match: 'b/**' }] }]);
        assert.equal(m.resolve('a/x'), undefined);
        assert.equal(m.resolve('b/x'), 'b');
    });
});
