export interface SseEvent {
    /** The `event:` name, `message` when none was given. */
    event: string;
    /** The `data:` lines, joined with newlines. */
    data: string;
}

/**
 * Reads a Server-Sent Events body: an event ends at a blank line, `:` lines are
 * keep-alives, and an event still open when the stream ends is delivered rather
 * than dropped (a server that closes without a trailing blank line is common).
 * The stream is released however the caller stops reading.
 */
export async function* sseEvents(body: ReadableStream<Uint8Array>): AsyncGenerator<SseEvent> {
    const reader = body.getReader();
    const decoder = new TextDecoder();
    let buffer = '';
    let event = 'message';
    let data: string[] = [];
    const pending = (): SseEvent | undefined => {
        const out = data.length > 0 ? { event, data: data.join('\n') } : undefined;
        event = 'message';
        data = [];
        return out;
    };
    const line = (raw: string): SseEvent | undefined => {
        const text = raw.replace(/\r$/, '');
        if (text === '') {
            return pending();
        }
        if (text.startsWith(':')) {
            return undefined;
        }
        const colon = text.indexOf(':');
        const field = colon < 0 ? text : text.slice(0, colon);
        const value = colon < 0 ? '' : text.slice(colon + 1).replace(/^ /, '');
        if (field === 'event') {
            event = value;
        } else if (field === 'data') {
            data.push(value);
        }
        return undefined;
    };
    try {
        for (;;) {
            const { done, value } = await reader.read();
            if (done) {
                break;
            }
            buffer += decoder.decode(value, { stream: true });
            let nl: number;
            while ((nl = buffer.indexOf('\n')) >= 0) {
                const out = line(buffer.slice(0, nl));
                buffer = buffer.slice(nl + 1);
                if (out) {
                    yield out;
                }
            }
        }
        buffer += decoder.decode();
        if (buffer) {
            line(buffer);
        }
        const last = pending();
        if (last) {
            yield last;
        }
    } finally {
        await reader.cancel().catch(() => undefined);
    }
}
