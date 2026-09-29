import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import {
    mayReceiveCredentials,
    mayShareContents,
    parseRepo,
    resolveConnection,
    resolveStudent,
    StudentInputs,
} from './settings';

describe('resolveConnection', () => {
    it("prefers the course's backend and token over the student's own", () => {
        const c = resolveConnection(
            { backend: 'https://course.example/', token: 'course-token' },
            { serverUrl: 'https://mine.example', token: 'my-token' },
            'env-token',
        );
        assert.equal(c.serverUrl, 'https://course.example'); // trailing slash trimmed
        assert.equal(c.token, 'course-token');
    });

    it('falls back through settings, the environment, then localhost', () => {
        assert.equal(resolveConnection({}, { token: 'mine' }, 'env').token, 'mine');
        assert.equal(resolveConnection({}, {}, 'env').token, 'env');
        assert.equal(resolveConnection({}, {}, undefined).serverUrl, 'http://localhost:8080');
    });

    it('an untrusted workspace has no say in where activity goes', () => {
        // The reporter hands over `{}` for the course in that case.
        const c = resolveConnection({}, { serverUrl: 'https://mine.example' }, undefined);
        assert.equal(c.serverUrl, 'https://mine.example');
        assert.equal(c.token, '');
    });

    it('either side can withhold file contents, and neither can force them out', () => {
        const share = (course: boolean | undefined, user: boolean | undefined) =>
            resolveConnection({ shareFileContents: course }, { shareFileContents: user }, '')
                .shareFileContents;
        assert.equal(share(undefined, undefined), true);
        assert.equal(share(true, true), true);
        assert.equal(share(false, true), false); // the course's policy
        assert.equal(share(true, false), false); // the student's veto
    });
});

describe('resolveStudent', () => {
    const inputs = (over: Partial<StudentInputs> = {}): StudentInputs => ({
        explicit: '',
        githubUser: '',
        gitEmail: async () => '',
        osUser: () => 'os-user',
        ...over,
    });

    it('takes the explicit name first when nothing says otherwise', async () => {
        const r = await resolveStudent(
            inputs({ explicit: 'ada', githubUser: 'ada-gh', gitEmail: async () => 'a@x' }),
        );
        assert.deepEqual(r, { value: 'ada', source: 'config' });
    });

    it('never falls back to the OS user on its own', async () => {
        assert.deepEqual(await resolveStudent(inputs()), { value: 'unknown', source: 'unknown' });
    });

    it('follows the teacher-chosen source, with the explicit name as its only fallback', async () => {
        const base = { explicit: 'typed', githubUser: 'gh', gitEmail: async () => 'e@x' };
        assert.equal((await resolveStudent(inputs({ ...base, preference: 'github' }))).value, 'gh');
        assert.equal(
            (await resolveStudent(inputs({ ...base, preference: 'git-email' }))).value,
            'e@x',
        );
        assert.equal((await resolveStudent(inputs({ ...base, preference: 'env' }))).value, 'typed');
        assert.equal(
            (await resolveStudent(inputs({ ...base, preference: 'os' }))).source,
            'os',
        );
        assert.equal(
            (await resolveStudent(inputs({ explicit: 'typed', preference: 'github' }))).source,
            'config',
        );
    });

    it('asks git for the email only when a source that needs it is reached', async () => {
        let asked = 0;
        const gitEmail = async () => (asked++, 'e@x');
        await resolveStudent(inputs({ explicit: 'ada', gitEmail }));
        assert.equal(asked, 0);
        await resolveStudent(inputs({ gitEmail }));
        assert.equal(asked, 1);
    });

    it('an unrecognised preference means the automatic order', async () => {
        const r = await resolveStudent(inputs({ preference: 'constructor', explicit: 'ada' }));
        assert.deepEqual(r, { value: 'ada', source: 'config' });
    });
});

describe('parseRepo', () => {
    it('reads owner/name from https and ssh remotes', () => {
        assert.equal(parseRepo('https://github.com/acme/course.git\n'), 'acme/course');
        assert.equal(parseRepo('git@github.com:acme/course.git'), 'acme/course');
        assert.equal(parseRepo('https://github.com/acme/course'), 'acme/course');
    });
    it('is empty for anything else', () => {
        assert.equal(parseRepo(''), '');
        assert.equal(parseRepo('not a remote'), '');
    });
});

describe('mayReceiveCredentials', () => {
    it('allows https anywhere and http only to this machine', () => {
        assert.equal(mayReceiveCredentials('https://hermione.school.edu'), true);
        assert.equal(mayReceiveCredentials('http://localhost:8080'), true);
        assert.equal(mayReceiveCredentials('http://127.0.0.1:8080'), true);
        assert.equal(mayReceiveCredentials('http://hermione.school.edu'), false);
        assert.equal(mayReceiveCredentials('ftp://localhost'), false);
        assert.equal(mayReceiveCredentials('not a url'), false);
    });
    it('is not fooled by a host that merely starts like localhost', () => {
        assert.equal(mayReceiveCredentials('http://localhost.evil.example'), false);
        assert.equal(mayReceiveCredentials('http://evil.example/localhost'), false);
    });
});

describe('mayShareContents', () => {
    it('shares ordinary files in the workspace', () => {
        assert.equal(mayShareContents('ex1/main.py', true), true);
        assert.equal(mayShareContents('notes/environment.md', true), true);
    });
    it('never shares a file outside every workspace folder', () => {
        assert.equal(mayShareContents('/home/ada/notes.txt', false), false);
    });
    it('never shares files that hold credentials, wherever they sit', () => {
        for (const p of [
            '.env',
            'app/.env.local',
            '.npmrc',
            'certs/server.PEM',
            'deploy/id_rsa',
            'keys/id_ed25519.pub',
            'secrets/tls.key',
            'vault.kdbx',
            '.ssh/config',
            'home/.aws/credentials',
            'a\\.gnupg\\pubring.kbx',
        ]) {
            assert.equal(mayShareContents(p, true), false, p);
        }
    });
});
