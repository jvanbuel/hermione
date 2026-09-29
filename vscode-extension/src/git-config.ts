import { execFile } from 'child_process';

/**
 * One `git config` value, or `''` when git is missing, slow, or has no such key.
 * Asynchronous and time-limited: this runs when settings change, on the same
 * thread as the student's typing, and a hung git must not freeze the editor.
 */
export function gitConfig(args: string[], cwd?: string): Promise<string> {
    return new Promise((resolve) => {
        execFile(
            'git',
            ['config', ...args],
            { cwd, encoding: 'utf8', timeout: 3_000 },
            (err, stdout) => resolve(err ? '' : stdout.trim()),
        );
    });
}
