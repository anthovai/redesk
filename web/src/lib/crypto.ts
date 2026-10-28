// Viewer side of the end-to-end session crypto. Must stay byte-compatible with agent/src/crypto.rs:
//   transcript = "redesk-kx-v1" || sid || viewer_epk || agent_epk
//   agent signs transcript with its Ed25519 identity (pinned by the server at registration)
//   keys = HKDF-SHA256(ikm = X25519 shared, salt = SHA-256(transcript), info = "redesk v1 <dir>")
//   AES-256-GCM, nonce = 4 zero bytes || u64 big-endian message counter (per direction)

const enc = new TextEncoder();

export function b64decode(s: string): Uint8Array<ArrayBuffer> {
	return Uint8Array.from(atob(s), (c) => c.charCodeAt(0));
}

export function b64encode(b: Uint8Array): string {
	let s = '';
	for (const x of b) s += String.fromCharCode(x);
	return btoa(s);
}

function concat(...parts: Uint8Array[]): Uint8Array<ArrayBuffer> {
	const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
	let o = 0;
	for (const p of parts) {
		out.set(p, o);
		o += p.length;
	}
	return out;
}

export function supported(): boolean {
	return typeof crypto !== 'undefined' && !!crypto.subtle;
}

export interface ViewerKeys {
	pair: CryptoKeyPair;
	epk: Uint8Array<ArrayBuffer>;
}

export async function newViewerKeys(): Promise<ViewerKeys> {
	const pair = (await crypto.subtle.generateKey({ name: 'X25519' }, false, ['deriveBits'])) as CryptoKeyPair;
	const epk = new Uint8Array(await crypto.subtle.exportKey('raw', pair.publicKey));
	return { pair, epk };
}

export async function completeHandshake(
	keys: ViewerKeys,
	sid: string,
	agentEpk: Uint8Array<ArrayBuffer>,
	signature: Uint8Array<ArrayBuffer>,
	devicePublicKey: Uint8Array<ArrayBuffer>
): Promise<Channel> {
	const transcript = concat(enc.encode('redesk-kx-v1'), enc.encode(sid), keys.epk, agentEpk);

	const identity = await crypto.subtle.importKey('raw', devicePublicKey, { name: 'Ed25519' }, false, ['verify']);
	if (!(await crypto.subtle.verify({ name: 'Ed25519' }, identity, signature, transcript))) {
		throw new Error('ลายเซ็นของเครื่องปลายทางไม่ถูกต้อง — อาจมีการดักกลางทาง (MITM)');
	}

	const agentPub = await crypto.subtle.importKey('raw', agentEpk, { name: 'X25519' }, false, []);
	const shared = await crypto.subtle.deriveBits({ name: 'X25519', public: agentPub }, keys.pair.privateKey, 256);
	const ikm = await crypto.subtle.importKey('raw', shared, 'HKDF', false, ['deriveBits']);
	const salt = await crypto.subtle.digest('SHA-256', transcript);
	const derive = async (info: string, usage: KeyUsage) => {
		const bits = await crypto.subtle.deriveBits({ name: 'HKDF', hash: 'SHA-256', salt, info: enc.encode(info) }, ikm, 256);
		return crypto.subtle.importKey('raw', bits, { name: 'AES-GCM' }, false, [usage]);
	};
	return new Channel(await derive('redesk v1 viewer->agent', 'encrypt'), await derive('redesk v1 agent->viewer', 'decrypt'));
}

function nonce(ctr: number): Uint8Array<ArrayBuffer> {
	const n = new Uint8Array(12);
	new DataView(n.buffer).setBigUint64(4, BigInt(ctr));
	return n;
}

/** Counter-nonce AEAD channel. Callers must seal/open strictly in transport order. */
export class Channel {
	private sendCtr = 0;
	private recvCtr = 0;

	constructor(
		private sendKey: CryptoKey,
		private recvKey: CryptoKey
	) {}

	seal(plain: Uint8Array<ArrayBuffer>): Promise<ArrayBuffer> {
		return crypto.subtle.encrypt({ name: 'AES-GCM', iv: nonce(this.sendCtr++) }, this.sendKey, plain);
	}

	async open(cipher: ArrayBuffer): Promise<Uint8Array<ArrayBuffer>> {
		const iv = nonce(this.recvCtr++);
		return new Uint8Array(await crypto.subtle.decrypt({ name: 'AES-GCM', iv }, this.recvKey, cipher));
	}
}
