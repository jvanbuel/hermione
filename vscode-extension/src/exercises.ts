import * as vscode from 'vscode';

/**
 * Maps files to exercises using an optional `.hermione.json` at each workspace
 * root:
 *
 * {
 *   "exercises": [
 *     { "name": "ex1", "match": "ex1/**" },
 *     { "name": "ex2", "match": ["ex2/**", "solutions/ex2/*"] }
 *   ]
 * }
 *
 * Patterns are matched against the workspace-relative path. If no config is
 * present or nothing matches, the file simply has no exercise (the teacher can
 * still map it later from the dashboard).
 */
interface ExerciseRule {
    name: string;
    patterns: RegExp[];
}

export class ExerciseMap {
    private rules: ExerciseRule[] = [];

    async load(): Promise<void> {
        this.rules = [];
        const folders = vscode.workspace.workspaceFolders ?? [];
        for (const folder of folders) {
            const uri = vscode.Uri.joinPath(folder.uri, '.hermione.json');
            try {
                const bytes = await vscode.workspace.fs.readFile(uri);
                const config = JSON.parse(Buffer.from(bytes).toString('utf8'));
                for (const ex of config.exercises ?? []) {
                    const raw = Array.isArray(ex.match) ? ex.match : [ex.match];
                    this.rules.push({
                        name: ex.name,
                        patterns: raw.filter(Boolean).map(globToRegExp),
                    });
                }
            } catch {
                // No config (or invalid) for this folder — that's fine.
            }
        }
    }

    resolve(relativePath: string): string | undefined {
        const normalized = relativePath.replace(/\\/g, '/');
        for (const rule of this.rules) {
            if (rule.patterns.some((re) => re.test(normalized))) {
                return rule.name;
            }
        }
        return undefined;
    }
}

/** Minimal glob → RegExp supporting `**`, `*`, and `?`. */
function globToRegExp(glob: string): RegExp {
    let re = '';
    for (let i = 0; i < glob.length; i++) {
        const c = glob[i];
        if (c === '*') {
            if (glob[i + 1] === '*') {
                re += '.*';
                i++;
                if (glob[i + 1] === '/') {
                    i++;
                }
            } else {
                re += '[^/]*';
            }
        } else if (c === '?') {
            re += '[^/]';
        } else if ('\\^$.|+()[]{}'.includes(c)) {
            re += '\\' + c;
        } else {
            re += c;
        }
    }
    return new RegExp('^' + re + '$');
}
