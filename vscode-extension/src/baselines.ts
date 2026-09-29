import { structuredPatch } from 'diff';

/**
 * What a snapshot needs from git, kept apart from the editor so it can be
 * tested without one.
 *
 * While a teacher watches, a snapshot is built every few hundred milliseconds —
 * on every keystroke and every cursor move. Nearly all of them ask git the same
 * question (what did the last commit say about this file?) and get the same
 * answer, and each ask is a `git` child process on the student's laptop. Asking
 * once per commit, and diffing once per distinct buffer, makes a cursor move
 * cost a `getText()` and a POST.
 */

/** One run of changes. Mirrors `Hunk` in `snapshots/report.rs`. */
export interface Hunk {
    /** 1-based line in the committed file this hunk begins at. */
    oldStart: number;
    /** 1-based line in the buffer this hunk begins at. */
    newStart: number;
    /** Unified-diff lines: ' ' context, '-' removed, '+' added. */
    lines: string[];
}

/** Lines of unchanged context kept around each change. */
const DIFF_CONTEXT = 3;

/**
 * Diffs a buffer against a committed file.
 *
 * The baseline comes from git and the comparison from jsdiff, so that what the
 * teacher sees includes edits the student hasn't saved yet — `git diff` alone
 * compares what's on disk, and the interesting moment is usually the one before
 * anyone hits save.
 */
export function diffHunks(committed: string, buffer: string, name: string): Hunk[] {
    const patch = structuredPatch(name, name, committed, buffer, '', '', { context: DIFF_CONTEXT });
    return patch.hunks.map((h) => ({
        oldStart: h.oldStart,
        newStart: h.newStart,
        // jsdiff marks a missing trailing newline with a `\` line, which is
        // noise in a live view of someone's editor.
        lines: h.lines.filter((l) => !l.startsWith('\\')),
    }));
}

/**
 * How long "git has no such file" is believed for. Git failing *is* how we hear
 * that a file was never committed, but it is also how we'd hear about a lock or
 * a race, and believing a transient failure for the life of a commit would show
 * a tracked file as untracked until the next one. A found file is exact for its
 * commit; an absent one is only trusted briefly.
 */
const ABSENT_TTL_MS = 10_000;

interface Entry {
    commit: string;
    /** The committed text, or `null` when the commit has no such file. */
    committed: string | null;
    at: number;
}

/**
 * The committed text of each file, loaded at most once per commit.
 *
 * Keyed on the commit, which is exact: staging and working-tree edits never
 * change what HEAD holds, and every operation that does — commit, amend,
 * checkout, reset, rebase — changes the commit.
 */
export class BaselineCache {
    // A Map iterates in insertion order, which makes it a ready-made LRU: a hit
    // is re-inserted at the end, and the oldest is the first key.
    private readonly entries = new Map<string, Entry>();

    constructor(
        private readonly now: () => number = Date.now,
        private readonly limit = 32,
    ) {}

    /**
     * The file's text at `commit`, or `null` if that commit has none. `load`
     * runs only when this isn't already known; if it throws, nothing is cached.
     *
     * An unborn branch has no commit to key on, so it is never cached.
     */
    async get(
        file: string,
        commit: string | undefined,
        load: () => Promise<string | null>,
    ): Promise<string | null> {
        if (commit === undefined) {
            return load();
        }
        const hit = this.entries.get(file);
        if (hit && hit.commit === commit && this.believed(hit)) {
            this.entries.delete(file);
            this.entries.set(file, hit);
            return hit.committed;
        }

        const committed = await load();
        this.entries.delete(file);
        this.entries.set(file, { commit, committed, at: this.now() });
        if (this.entries.size > this.limit) {
            const oldest = this.entries.keys().next().value;
            if (oldest !== undefined) {
                this.entries.delete(oldest);
            }
        }
        return committed;
    }

    private believed(entry: Entry): boolean {
        return entry.committed !== null || this.now() - entry.at < ABSENT_TTL_MS;
    }
}

/**
 * The last diff computed, reused while it is still the answer.
 *
 * A cursor move sends the same buffer again, and a diff is a pure function of
 * (committed text, buffer). One entry is enough: only the file being watched is
 * ever asked about, and the moment it changes the old one is no use.
 */
export class DiffMemo {
    private last?: { file: string; commit: string; buffer: string; hunks: Hunk[] };

    get(file: string, commit: string, buffer: string, compute: () => Hunk[]): Hunk[] {
        const last = this.last;
        if (last && last.file === file && last.commit === commit && last.buffer === buffer) {
            return last.hunks;
        }
        const hunks = compute();
        this.last = { file, commit, buffer, hunks };
        return hunks;
    }
}
