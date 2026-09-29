import { structuredPatch } from 'diff';
import * as vscode from 'vscode';

/**
 * What the student's editor answers a snapshot request with: the buffer's text,
 * where the cursor is, and how it differs from their last commit.
 *
 * Unlike file *events*, which are reported continuously, a report is only ever
 * built when the backend asks for one — which it only does while a teacher has
 * the student's file open. Nothing here is sent unprompted.
 *
 * This mirrors `crates/server/src/snapshots/report.rs`, and is a union rather
 * than a bag of optional fields for the same reason: an editor that declines to
 * share has no `content` to send, so a `declined` report cannot carry one, and a
 * file cannot be both untracked and diffed against its last commit.
 */
export type Report = { student: string } & (
    | { state: 'declined' }
    | { state: 'empty' }
    | OpenFile
);

export interface OpenFile {
    state: 'file';
    path: string;
    relativePath: string;
    /** VSCode's language id for the document. */
    language: string;
    exercise?: string;
    /** Both 1-based. Absent when no editor has the file focused. */
    cursor?: { line: number; column: number };
    /** The buffer has unsaved changes. */
    dirty: boolean;
    /** The buffer as the student sees it this instant. */
    content: string;
    /** `content` was cut short before sending. */
    truncated?: boolean;
    baseline: Baseline;
}

/**
 * What the buffer is compared against. Counts a unified diff also carries
 * (lines added, lines removed, a hunk's line counts) are left to the server:
 * they follow from the lines, and one that disagreed with them would be a lie
 * we had no way to notice.
 */
export type Baseline =
    | { kind: 'head'; hunks: Hunk[] }
    | { kind: 'untracked' }
    /** Nothing to compare with: a notebook cell, or no git. */
    | { kind: 'none' };

export interface Hunk {
    /** 1-based line in the committed file this hunk begins at. */
    oldStart: number;
    /** 1-based line in the buffer this hunk begins at. */
    newStart: number;
    /** Unified-diff lines: ' ' context, '-' removed, '+' added. */
    lines: string[];
}

/**
 * Enough of a file to be worth reading on a projector. Past this the buffer is
 * cut short (and flagged), which keeps one enormous generated file from
 * flooding the backend.
 */
const MAX_CONTENT_CHARS = 200_000;

/** Lines of unchanged context kept around each change. */
const DIFF_CONTEXT = 3;

// The git extension's API, narrowed to the two things we need. Declared
// structurally rather than depending on the extension's own typings, which
// aren't published as a package.
interface GitRepository {
    /** The file's contents at a ref. Rejects when the ref has no such file. */
    show(ref: string, path: string): Promise<string>;
}

interface GitApi {
    getRepository(uri: vscode.Uri): GitRepository | null;
}

interface GitExtension {
    getAPI(version: 1): GitApi;
}

let gitApi: Promise<GitApi | undefined> | undefined;

/**
 * The built-in git extension's API, activated on first use.
 *
 * Reusing it means the baseline for a diff is read by git itself — the real
 * object store, honouring the student's actual HEAD — rather than anything we
 * reimplement. Absent (or a workspace that isn't a repo) simply means no diff.
 *
 * The *promise* is what's cached, so callers that arrive during activation
 * wait for it rather than being told there is no git.
 */
function git(): Promise<GitApi | undefined> {
    return (gitApi ??= (async () => {
        try {
            const ext = vscode.extensions.getExtension<GitExtension>('vscode.git');
            if (!ext) {
                return undefined;
            }
            const exports = ext.isActive ? ext.exports : await ext.activate();
            return exports.getAPI(1);
        } catch (_) {
            return undefined;
        }
    })());
}

/**
 * The committed text of a file, or undefined when there isn't one (untracked,
 * or not a git working tree at all).
 */
async function committedText(uri: vscode.Uri): Promise<string | undefined> {
    const api = await git();
    if (!api) {
        return undefined;
    }
    try {
        const repo = api.getRepository(uri);
        if (!repo) {
            return undefined;
        }
        return await repo.show('HEAD', uri.fsPath);
    } catch (_) {
        // No committed version of this file (or git errored) — either way there
        // is nothing to diff against.
        return undefined;
    }
}

/**
 * Diffs the student's live buffer against their last commit.
 *
 * The baseline comes from git and the comparison from jsdiff, so that what the
 * teacher sees includes edits the student hasn't saved yet — `git diff` alone
 * compares what's on disk, and the interesting moment is usually the one before
 * anyone hits save.
 */
function diffAgainst(baseline: string, current: string, name: string): Hunk[] {
    const patch = structuredPatch(name, name, baseline, current, '', '', {
        context: DIFF_CONTEXT,
    });
    return patch.hunks.map((h) => ({
        oldStart: h.oldStart,
        newStart: h.newStart,
        // jsdiff marks a missing trailing newline with a `\` line, which is
        // noise in a live view of someone's editor.
        lines: h.lines.filter((l) => !l.startsWith('\\')),
    }));
}

/** The document the student is looking at, and the editor showing it. */
export interface SnapshotTarget {
    /** Absent for a notebook with no cells: a file, but no buffer to read. */
    doc?: vscode.TextDocument;
    editor?: vscode.TextEditor;
    /** The `file:` URI the document belongs to. */
    uri: vscode.Uri;
    /** True for a notebook cell, whose buffer is not the file on disk. */
    cell: boolean;
}

/**
 * Builds the report for a file the student has open. `content` is the buffer as
 * the student sees it this instant, including unsaved edits.
 */
export async function buildOpenFile(
    target: SnapshotTarget & { doc: vscode.TextDocument },
    where: { relativePath: string; exercise?: string },
): Promise<OpenFile> {
    const text = target.doc.getText();
    const truncated = text.length > MAX_CONTENT_CHARS;
    const cursor = target.editor?.selection.active;

    return {
        state: 'file',
        path: target.uri.fsPath,
        relativePath: where.relativePath,
        language: target.doc.languageId,
        exercise: where.exercise,
        cursor: cursor && { line: cursor.line + 1, column: cursor.character + 1 },
        dirty: target.doc.isDirty,
        content: truncated ? text.slice(0, MAX_CONTENT_CHARS) : text,
        truncated: truncated || undefined,
        baseline: await baselineFor(target, text, where.relativePath),
    };
}

async function baselineFor(
    target: SnapshotTarget,
    text: string,
    relativePath: string,
): Promise<Baseline> {
    // A notebook cell's buffer is one cell of a JSON file, so diffing it
    // against the committed `.ipynb` would compare a Python fragment with a
    // JSON document. Report the cell, and no baseline.
    if (target.cell) {
        return { kind: 'none' };
    }
    const committed = await committedText(target.uri);
    return committed === undefined
        ? { kind: 'untracked' }
        : { kind: 'head', hunks: diffAgainst(committed, text, relativePath) };
}
