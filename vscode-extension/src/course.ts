import * as vscode from 'vscode';
import type { CourseSettings } from './settings';

/** One exercise as `.hermione.json` declares it. */
export interface ExerciseSpec {
    name: string;
    match?: string | string[];
}

/** The committed course file: connection settings plus the exercise list. */
export interface CourseFile extends CourseSettings {
    exercises?: ExerciseSpec[];
}

export interface CourseFiles {
    /** Every workspace folder's course file that could be read, in folder order. */
    files: CourseFile[];
    /** Files that exist but could not be used, for the student to be told about. */
    problems: string[];
}

/**
 * Reads `.hermione.json` from each workspace folder. A folder without one is
 * normal and says nothing; a file that is there but unreadable or not JSON is a
 * problem — otherwise a typo in it silently sends a student's activity to the
 * default backend and maps no file to any exercise, with nobody the wiser.
 */
export async function readCourseFiles(): Promise<CourseFiles> {
    const files: CourseFile[] = [];
    const problems: string[] = [];
    for (const folder of vscode.workspace.workspaceFolders ?? []) {
        const uri = vscode.Uri.joinPath(folder.uri, '.hermione.json');
        let bytes: Uint8Array;
        try {
            bytes = await vscode.workspace.fs.readFile(uri);
        } catch (e) {
            if (!(e instanceof vscode.FileSystemError && e.code === 'FileNotFound')) {
                problems.push(`${folder.name}/.hermione.json could not be read: ${String(e)}`);
            }
            continue;
        }
        try {
            const parsed: unknown = JSON.parse(Buffer.from(bytes).toString('utf8'));
            if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
                throw new Error('expected a JSON object');
            }
            files.push(parsed as CourseFile);
        } catch (e) {
            problems.push(`${folder.name}/.hermione.json is not valid: ${(e as Error).message}`);
        }
    }
    return { files, problems };
}
