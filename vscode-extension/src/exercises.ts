import type { CourseFile } from './course';

/**
 * Maps files to exercises using the `exercises` list of the optional
 * `.hermione.json` at each workspace root:
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

    /** Replaces the rules with those declared by the given course files. */
    load(files: CourseFile[]): void {
        this.rules = [];
        for (const file of files) {
            for (const ex of file.exercises ?? []) {
                const raw = Array.isArray(ex.match) ? ex.match : [ex.match];
                this.rules.push({
                    name: ex.name,
                    patterns: raw.filter((m): m is string => !!m).map(globToRegExp),
                });
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
