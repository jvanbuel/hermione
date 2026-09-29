/**
 * Everything the reporter decides from its configuration, kept free of VSCode
 * so it can be tested without an editor: where to connect, who the student is,
 * whether a credential may be sent to a server, and which files may be shared.
 */

/** What the committed course file (`.hermione.json`) may set. */
export interface CourseSettings {
    backend?: string;
    token?: string;
    identity?: string;
    authProvider?: string;
    shareFileContents?: boolean;
}

/** What the student's own VSCode settings and environment provide. */
export interface UserSettings {
    serverUrl?: string;
    token?: string;
    shareFileContents?: boolean;
}

export interface Connection {
    serverUrl: string;
    token: string;
    authProvider: string;
    shareFileContents: boolean;
}

/** The first of `values` that is set to something, or `''`. */
export function firstNonEmpty(...values: (string | undefined | null)[]): string {
    return values.find((v): v is string => !!v) ?? '';
}

/**
 * Where to connect and how. The course file wins over the student's settings
 * for the backend and its token — it is what the teacher controls — except that
 * withholding file contents needs only one side to ask: the course sets the
 * policy and the student keeps a veto over their own buffer, so the more
 * restrictive of the two applies.
 *
 * Pass `{}` for `course` when the workspace is not trusted: a cloned repository
 * must not choose where a student's activity, file contents and sign-in are sent.
 */
export function resolveConnection(
    course: CourseSettings,
    user: UserSettings,
    envToken: string | undefined,
): Connection {
    return {
        serverUrl: firstNonEmpty(course.backend, user.serverUrl, 'http://localhost:8080').replace(
            /\/$/,
            '',
        ),
        token: firstNonEmpty(course.token, user.token, envToken),
        authProvider: firstNonEmpty(course.authProvider, 'github'),
        shareFileContents: course.shareFileContents !== false && (user.shareFileContents ?? true),
    };
}

/** Where a student's name may come from, and which one produced it. */
export type StudentSource = 'github' | 'config' | 'git-email' | 'os';

export interface StudentInputs {
    /** `.hermione.json`'s `identity`: which source the teacher wants used. */
    preference?: string;
    /** `hermione.student` or `$HERMIONE_STUDENT`. */
    explicit: string;
    /** `$GITHUB_USER`. */
    githubUser: string;
    /** Looked up only if a source that needs it is reached. */
    gitEmail: () => Promise<string>;
    osUser: () => string;
}

/**
 * The student identity and which signal produced it (provenance). The automatic
 * order does not fall back to the OS username — it collides in shared
 * devcontainers — so an unresolved identity is reported as "unknown", where it is
 * visible, rather than as somebody else.
 */
export async function resolveStudent(
    i: StudentInputs,
): Promise<{ value: string; source: StudentSource | 'unknown' }> {
    const order = ((): StudentSource[] => {
        switch (i.preference) {
            case 'github':
                return ['github', 'config'];
            case 'git-email':
                return ['git-email', 'config'];
            case 'env':
                return ['config'];
            case 'os':
                return ['os'];
            default:
                return ['config', 'github', 'git-email'];
        }
    })();
    for (const source of order) {
        const value = await {
            github: async () => i.githubUser,
            config: async () => i.explicit,
            'git-email': i.gitEmail,
            os: async () => i.osUser(),
        }[source]();
        if (value) {
            return { value, source };
        }
    }
    return { value: 'unknown', source: 'unknown' };
}

/** `owner/name` from a git remote URL, or `''` when it isn't one. */
export function parseRepo(remoteUrl: string): string {
    const m = remoteUrl.trim().match(/[:/]([^/]+\/[^/]+?)(?:\.git)?$/);
    return m ? m[1] : '';
}

/**
 * Whether a sign-in credential may be sent to `serverUrl`: only over https, or
 * to this machine. The GitHub token is exchanged at the backend, and a plain-text
 * connection to somebody else's host would hand it to anyone on the path.
 */
export function mayReceiveCredentials(serverUrl: string): boolean {
    let url: URL;
    try {
        url = new URL(serverUrl);
    } catch {
        return false;
    }
    if (url.protocol === 'https:') {
        return true;
    }
    return url.protocol === 'http:' && ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname);
}

/** Basenames that name a secret however the file is spelled. */
const SECRET_NAME = /^(\.env(\..*)?|\.npmrc|\.netrc|\.pgpass|credentials|id_(rsa|dsa|ecdsa|ed25519)(\.pub)?|.*\.(pem|key|p12|pfx|kdbx))$/i;
/** Directories whose whole contents are secrets. */
const SECRET_DIRS = new Set(['.ssh', '.aws', '.gnupg', '.kube']);

/**
 * Whether the contents of a file may be shown to a teacher: it lives in a
 * workspace folder (a file elsewhere on the machine is not part of the course)
 * and is not the kind of file that holds credentials. `relativePath` is what
 * VSCode reports, which is absolute for a file outside every workspace folder.
 */
export function mayShareContents(relativePath: string, insideWorkspace: boolean): boolean {
    if (!insideWorkspace) {
        return false;
    }
    const parts = relativePath.replace(/\\/g, '/').split('/').filter(Boolean);
    const name = parts[parts.length - 1] ?? '';
    return !SECRET_NAME.test(name) && !parts.slice(0, -1).some((p) => SECRET_DIRS.has(p));
}
