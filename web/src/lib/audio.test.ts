/// <reference types="bun" />
// bun test src/lib/audio.test.ts
//
// RecordedAudio against fakes of WebCodecs and Web Audio. The fake AudioDecoder stamps its output
// like Chrome's does: the first chunk's timestamp, then counting on by decoded samples and
// ignoring the timestamps of later chunks.

import { beforeEach, expect, test } from 'bun:test';
import { RecordedAudio, type AudioPacket } from './audio';

let now = 0;
const started: { when: number; offset: number }[] = [];
const pending: (() => void)[] = [];

class FakeAudioDecoder {
	state = 'unconfigured';
	private next: number | null = null;
	constructor(private init: { output: (d: unknown) => void; error: (e: unknown) => void }) {}
	configure() {
		this.state = 'configured';
	}
	decode(chunk: { timestamp: number }) {
		const ts = (this.next ??= chunk.timestamp);
		this.next += 20_000;
		pending.push(() => {
			if (this.state === 'closed') return;
			this.init.output({
				timestamp: ts,
				numberOfFrames: 960,
				numberOfChannels: 2,
				sampleRate: 48000,
				copyTo: (p: Float32Array) => p.fill(0.1),
				close: () => {}
			});
		});
	}
	async flush() {
		drain();
	}
	close() {
		this.state = 'closed';
	}
}

class FakeAudioContext {
	state = 'running';
	outputLatency = 0;
	destination = {};
	get currentTime() {
		return now / 1000;
	}
	createGain() {
		return { gain: { value: 1 }, connect() {} };
	}
	createBuffer(_ch: number, len: number, rate: number) {
		return { duration: len / rate, copyToChannel() {} };
	}
	createBufferSource() {
		return {
			buffer: null,
			playbackRate: { value: 1 },
			onended: null,
			connect() {},
			stop() {},
			start: (when: number, offset = 0) => started.push({ when, offset })
		};
	}
	resume() {
		return Promise.resolve();
	}
	close() {
		return Promise.resolve();
	}
}

function drain() {
	while (pending.length) pending.shift()!();
}

Object.assign(globalThis, {
	AudioDecoder: FakeAudioDecoder,
	AudioContext: FakeAudioContext,
	EncodedAudioChunk: class {
		timestamp: number;
		constructor(init: { timestamp: number }) {
			this.timestamp = init.timestamp;
		}
	}
});

beforeEach(() => {
	now = 0;
	started.length = 0;
	pending.length = 0;
});

const packets = (from: number, n: number): AudioPacket[] => Array.from({ length: n }, (_, i) => ({ t: from + i * 20, data: new Uint8Array([1]) }));

test('audio after a gap (muted or dropped) plays at its own time', () => {
	// 0–180 ms, nothing until 1000 ms (the viewer muted), then 1000–1180 ms.
	const audio = new RecordedAudio([...packets(0, 10), ...packets(1000, 10)]);
	audio.unlock();
	audio.reset(0, () => now, 1);
	audio.tick();
	drain();
	expect(started.length).toBe(10);
	now = 900;
	audio.tick();
	drain();
	expect(started.length).toBe(20);
	// Due 100 ms after the clock's 900 ms, i.e. at context time 1.0 s and on.
	started.slice(10).forEach((s, i) => expect(s.when).toBeCloseTo(1 + i * 0.02, 5));
});

test('a seek restarts at the packet under the new position', () => {
	const audio = new RecordedAudio(packets(0, 100));
	audio.unlock();
	now = 0;
	audio.reset(1000, () => 1000 + now, 1);
	audio.tick();
	drain();
	expect(started[0].when).toBeCloseTo(0, 5);
	expect(started.length).toBe(21); // 1000–1400 ms
});
