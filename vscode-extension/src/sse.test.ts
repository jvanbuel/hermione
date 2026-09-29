import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { sseEvents } from './sse';

/** A body that delivers the given chunks, then ends (or fails). */
function body(chunks: string[], failWith?: Error): ReadableStream<Uint8Array> {
    const enc = new TextEncoder();
    let i = 0;
    return new ReadableStream({
        pull(controller) {
            if (i < chunks.length) {
                controller.enqueue(enc.encode(chunks[i++]));
            } else if (failWith) {
                controller.error(failWith);
            } else {
                controller.close();
            }
        },
    });
}

async function collect(stream: ReadableStream<Uint8Array>) {
    const out = [];
    for await (const e of sseEvents(stream)) {
        out.push(e);
    }
    return out;
}

describe('sseEvents', () => {
    it('reads named events and defaults the name to "message"', async () => {
        const events = await collect(
            body(['event: status\ndata: {"text":"thinking"}\n\n', 'data: {"text":"hi"}\n\n']),
        );
        assert.deepEqual(events, [
            { event: 'status', data: '{"text":"thinking"}' },
            { event: 'message', data: '{"text":"hi"}' },
        ]);
    });

    it('reassembles an event split across chunks, even mid-line', async () => {
        const events = await collect(body(['event: mess', 'age\nda', 'ta: {"a":1}\n', '\n']));
        assert.deepEqual(events, [{ event: 'message', data: '{"a":1}' }]);
    });

    it('ignores keep-alives and handles CRLF', async () => {
        const events = await collect(body([': keep-alive\r\n\r\nevent: done\r\ndata: {}\r\n\r\n']));
        assert.deepEqual(events, [{ event: 'done', data: '{}' }]);
    });

    it('joins multi-line data', async () => {
        const events = await collect(body(['data: one\ndata: two\n\n']));
        assert.deepEqual(events, [{ event: 'message', data: 'one\ntwo' }]);
    });

    it('delivers an event the stream ended in the middle of', async () => {
        // No trailing blank line, and not even a trailing newline.
        const events = await collect(body(['event: message\ndata: {"text":"last"}']));
        assert.deepEqual(events, [{ event: 'message', data: '{"text":"last"}' }]);
    });

    it('reports a connection that dies mid-stream to the reader', async () => {
        await assert.rejects(
            collect(body(['data: {"text":"a"}\n\n'], new Error('socket hang up'))),
            /socket hang up/,
        );
    });

    it('releases the stream when the reader stops early', async () => {
        let cancelled = false;
        const enc = new TextEncoder();
        const stream = new ReadableStream<Uint8Array>({
            pull(c) {
                c.enqueue(enc.encode('data: 1\n\n'));
            },
            cancel() {
                cancelled = true;
            },
        });
        for await (const _ of sseEvents(stream)) {
            break;
        }
        assert.equal(cancelled, true);
    });
});
