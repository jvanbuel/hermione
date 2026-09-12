import { structuredPatch } from 'diff';
import * as vscode from 'vscode';

/**
 * A snapshot of what the student has on screen: the buffer's text, where the
 * cursor is, and how it differs from their last commit.
 *
 * Unlike file *events*, which are reported continuously, a snapshot is only
 * ever built when the backend asks for one — which it only does while a teacher
 * has the student's file open. Nothing here is sent unprompted.
 */
export interface FileSnapshot {
    student: string;
    path?: string;
    relativePath?: string;
    language?: string;
    exercise?: string;
    /** 1-based cursor line. */
    line?: number;
    /** 1-based cursor column. */
    column?: number;
    /** The buffer has unsaved changes. */
    dirty?: boolean;
    content?: string;
    truncated?: boolean;
    /** `head`, `untracked`, or `none` — what `diff` is measured against. */
    base?: 'head' | 'untracked' | 'none';
    diff?: SnapshotDiff;
    /** Set when this student's configuration forbids sharing file contents. */
    declined?: boolean;
    atUnixMs: number;
}

export interface SnapshotDiff {
    added: number;
    removed: number;
    hunks: SnapshotHunk[];
    truncated?: boolean;
}

export interface SnapshotHunk {
    oldStart: number;
    oldLines: number;
    newStart: number;
    newLines: number;
    /** Unified-diff lines: ' ' context, '-' removed, '+' added. */
    lines: string[];
}

/**
 * Enough of a file to be worth reading on a projector. Past this the buffer is
 * cut short (and flagged), which keeps one enormous generated file from
 * flooding the backend.
 */
const MAX_CONTENT_CHARS = 200_000;

/** Diff lines kept per snapshot, so a wholesale rewrite stays bounded. */
const MAX_DIFF_LINES = 2000;

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

let gitApi: GitApi | undefined;
let gitTried = false;

/**
 * The built-in git extension's API, activated on first use.
 *
 * Reusing it means the baseline for a diff is read by git itself — the real
 * object store, honouring the student's actual HEAD — rather than anything we
 * reimplement. Absent (or a workspace that isn't a repo) simply means no diff.
 */
async function git(): Promise<GitApi | undefined> {
    if (gitTried) {
        return gitApi;
    }
    gitTried = true;
    try {
        const ext = vscode.extensions.getExtension<GitExtension>('vscode.git');
        if (!ext) {
            return undefined;
        }
        const exports = ext.isActive ? ext.exports : await ext.activate();
        gitApi = exports.getAPI(1);
    } catch (_) {
        gitApi = undefined;
    }
    return gitApi;
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
function diffAgainst(baseline: string, current: string, name: string): SnapshotDiff {
    const patch = structuredPatch(name, name, baseline, current, '', '', {
        context: DIFF_CONTEXT,
    });

    let added = 0;
    let removed = 0;
    let budget = MAX_DIFF_LINES;
    let truncated = false;
    const hunks: SnapshotHunk[] = [];

    for (const h of patch.hunks) {
        // jsdiff marks a missing trailing newline with a `\` line, which is
        // noise in a live view of someone's editor.
        const lines = h.lines.filter((l) => !l.startsWith('\\'));
        for (const l of lines) {
            if (l.startsWith('+')) {
                added++;
            } else if (l.startsWith('-')) {
                removed++;
            }
        }
        if (lines.length > budget) {
            truncated = true;
            continue;
        }
        budget -= lines.length;
        hunks.push({
            oldStart: h.oldStart,
            oldLines: h.oldLines,
            newStart: h.newStart,
            newLines: h.newLines,
            lines,
        });
    }

    return truncated ? { added, removed, hunks, truncated } : { added, removed, hunks };
}

/** The document the student is looking at, and the editor showing it. */
export interface SnapshotTarget {
    doc: vscode.TextDocument;
    editor?: vscode.TextEditor;
    /** The `file:` URI the document belongs to. */
    uri: vscode.Uri;
    /** True for a notebook cell, whose buffer is not the file on disk. */
    cell: boolean;
}

/**
 * Builds the snapshot for a target. `content` is the buffer as the student sees
 * it this instant, including unsaved edits.
 */
export async function buildSnapshot(
    target: SnapshotTarget,
    base: { student: string; relativePath?: string; exercise?: string },
): Promise<FileSnapshot> {
    const text = target.doc.getText();
    const truncated = text.length > MAX_CONTENT_CHARS;
    const cursor = target.editor?.selection.active;

    const snapshot: FileSnapshot = {
        student: base.student,
        path: target.uri.fsPath,
        relativePath: base.relativePath,
        language: target.doc.languageId,
        exercise: base.exercise,
        line: cursor ? cursor.line + 1 : undefined,
        column: cursor ? cursor.character + 1 : undefined,
        dirty: target.doc.isDirty,
        content: truncated ? text.slice(0, MAX_CONTENT_CHARS) : text,
        truncated: truncated || undefined,
        atUnixMs: Date.now(),
    };

    // A notebook cell's buffer is one cell of a JSON file, so diffing it
    // against the committed `.ipynb` would compare a Python fragment with a
    // JSON document. Report the cell, and no baseline.
    if (target.cell) {
        snapshot.base = 'none';
        return snapshot;
    }

    const baseline = await committedText(target.uri);
    if (baseline === undefined) {
        snapshot.base = 'untracked';
        return snapshot;
    }
    snapshot.base = 'head';
    snapshot.diff = diffAgainst(baseline, text, base.relativePath || target.uri.fsPath);
    return snapshot;
}
