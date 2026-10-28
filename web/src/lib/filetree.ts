// Folder transfer helpers: relative-path sanitising (same rules as the agent, see
// docs/PROTOCOL.md "Folders and file management"), a STORE-only ZIP writer and
// drag-and-drop folder walking.

const enc = new TextEncoder();

/**
 * Validates a `/`- or `\`-separated relative path from the other side. Returns the cleaned
 * components, or null if the path is empty, absolute, has a drive/UNC prefix or contains
 * `.`/`..`. Characters invalid on Windows become `_`; components are cut to 255 bytes.
 */
export function sanitizeRel(rel: string): string[] | null {
	if (!rel || rel.startsWith('/') || rel.startsWith('\\') || /^[a-zA-Z]:/.test(rel)) return null;
	const parts = rel.split(/[/\\]/);
	const out: string[] = [];
	for (const p of parts) {
		if (p === '' || p === '.' || p === '..') return null;
		// eslint-disable-next-line no-control-regex
		let c = p.replace(/[<>:"|?*\u0000-\u001f\u007f]/g, '_');
		while (enc.encode(c).length > 255) c = c.slice(0, -1);
		if (!c) return null;
		out.push(c);
	}
	return out.length ? out : null;
}

/** A single path component (folder or file name typed by the user). */
export function validName(name: string): boolean {
	const n = name.trim();
	return !!n && n !== '.' && n !== '..' && !/[/\\]/.test(n);
}

// --- ZIP (STORE, no compression, no zip64) ---

const CRC_TABLE = (() => {
	const t = new Uint32Array(256);
	for (let n = 0; n < 256; n++) {
		let c = n;
		for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
		t[n] = c >>> 0;
	}
	return t;
})();

export function crc32Update(crc: number, data: Uint8Array): number {
	let c = crc ^ 0xffffffff;
	for (let i = 0; i < data.length; i++) c = CRC_TABLE[(c ^ data[i]) & 0xff] ^ (c >>> 8);
	return (c ^ 0xffffffff) >>> 0;
}

/** Largest archive we write without zip64. */
export const ZIP_LIMIT = 0xffff_ffff - 64 * 1024 * 1024;

interface ZipEntry {
	name: Uint8Array;
	crc: number;
	size: number;
	offset: number;
	dir: boolean;
}

/** Streams entries into Blob parts; files are added chunk by chunk (CRC computed on the fly). */
export class ZipWriter {
	private parts: BlobPart[] = [];
	private entries: ZipEntry[] = [];
	private offset = 0;
	private cur: { entry: ZipEntry; chunks: Uint8Array<ArrayBuffer>[] } | null = null;
	private names = new Set<string>();

	private push(b: Uint8Array<ArrayBuffer>) {
		this.parts.push(b);
		this.offset += b.length;
	}

	private unique(path: string): string {
		if (!this.names.has(path)) {
			this.names.add(path);
			return path;
		}
		const slash = path.lastIndexOf('/');
		const dir = path.slice(0, slash + 1);
		const base = path.slice(slash + 1);
		const dot = base.lastIndexOf('.');
		const [stem, ext] = dot > 0 ? [base.slice(0, dot), base.slice(dot)] : [base, ''];
		for (let n = 1; ; n++) {
			const p = `${dir}${stem} (${n})${ext}`;
			if (!this.names.has(p)) {
				this.names.add(p);
				return p;
			}
		}
	}

	addDir(path: string) {
		this.endFile();
		const p = path.endsWith('/') ? path : path + '/';
		if (this.names.has(p)) return;
		this.names.add(p);
		const entry: ZipEntry = { name: enc.encode(p), crc: 0, size: 0, offset: this.offset, dir: true };
		this.entries.push(entry);
		this.push(this.localHeader(entry));
	}

	beginFile(path: string) {
		this.endFile();
		const entry: ZipEntry = { name: enc.encode(this.unique(path)), crc: 0, size: 0, offset: 0, dir: false };
		this.cur = { entry, chunks: [] };
	}

	write(data: Uint8Array<ArrayBuffer>) {
		if (!this.cur) return;
		this.cur.entry.crc = crc32Update(this.cur.entry.crc, data);
		this.cur.entry.size += data.length;
		this.cur.chunks.push(data);
	}

	/** Current archive size including the open file. */
	get size(): number {
		return this.offset + (this.cur ? this.cur.entry.size + 30 + this.cur.entry.name.length : 0);
	}

	/** Closes the open file (if any); later chunks are ignored until the next beginFile. */
	endFile() {
		if (!this.cur) return;
		const { entry, chunks } = this.cur;
		this.cur = null;
		entry.offset = this.offset;
		this.entries.push(entry);
		this.push(this.localHeader(entry));
		for (const c of chunks) this.push(c);
	}

	private localHeader(e: ZipEntry): Uint8Array<ArrayBuffer> {
		const h = new Uint8Array(30 + e.name.length);
		const v = new DataView(h.buffer);
		v.setUint32(0, 0x04034b50, true);
		v.setUint16(4, 20, true); // version needed
		v.setUint16(6, 0x0800, true); // UTF-8 names
		v.setUint16(8, 0, true); // STORE
		v.setUint16(10, 0, true);
		v.setUint16(12, 0x21, true); // 1980-01-01
		v.setUint32(14, e.crc, true);
		v.setUint32(18, e.size, true);
		v.setUint32(22, e.size, true);
		v.setUint16(26, e.name.length, true);
		h.set(e.name, 30);
		return h;
	}

	finish(): Blob {
		this.endFile();
		const start = this.offset;
		for (const e of this.entries) {
			const h = new Uint8Array(46 + e.name.length);
			const v = new DataView(h.buffer);
			v.setUint32(0, 0x02014b50, true);
			v.setUint16(4, 0x031e, true); // made by: unix, 3.0
			v.setUint16(6, 20, true);
			v.setUint16(8, 0x0800, true);
			v.setUint16(10, 0, true);
			v.setUint16(12, 0, true);
			v.setUint16(14, 0x21, true);
			v.setUint32(16, e.crc, true);
			v.setUint32(20, e.size, true);
			v.setUint32(24, e.size, true);
			v.setUint16(28, e.name.length, true);
			v.setUint32(38, e.dir ? ((0o40755 << 16) | 0x10) >>> 0 : (0o100644 << 16) >>> 0, true);
			v.setUint32(42, e.offset, true);
			h.set(e.name, 46);
			this.push(h);
		}
		const end = new Uint8Array(22);
		const v = new DataView(end.buffer);
		v.setUint32(0, 0x06054b50, true);
		v.setUint16(8, this.entries.length, true);
		v.setUint16(10, this.entries.length, true);
		v.setUint32(12, this.offset - start, true);
		v.setUint32(16, start, true);
		this.push(end);
		return new Blob(this.parts, { type: 'application/zip' });
	}
}

// --- local folders (drag and drop / webkitdirectory) ---

export interface LocalTree {
	/** Top-level folder name. */
	name: string;
	/** Files with their path relative to the folder (`/`-separated). */
	files: { rel: string; file: File }[];
	/** Sub-folders with nothing inside (relative paths). */
	emptyDirs: string[];
}

function readAll(reader: FileSystemDirectoryReader): Promise<FileSystemEntry[]> {
	return new Promise((resolve, reject) => {
		const out: FileSystemEntry[] = [];
		const next = () =>
			reader.readEntries((batch) => {
				if (!batch.length) resolve(out);
				else {
					out.push(...batch);
					next();
				}
			}, reject);
		next();
	});
}

const fileOf = (e: FileSystemFileEntry) => new Promise<File>((resolve, reject) => e.file(resolve, reject));

/** Walks a dropped directory entry. */
export async function walkEntry(root: FileSystemDirectoryEntry): Promise<LocalTree> {
	const tree: LocalTree = { name: root.name, files: [], emptyDirs: [] };
	async function walk(dir: FileSystemDirectoryEntry, prefix: string) {
		const entries = await readAll(dir.createReader());
		if (!entries.length && prefix) tree.emptyDirs.push(prefix.slice(0, -1));
		for (const e of entries) {
			if (e.isDirectory) await walk(e as FileSystemDirectoryEntry, prefix + e.name + '/');
			else if (e.isFile) tree.files.push({ rel: prefix + e.name, file: await fileOf(e as FileSystemFileEntry) });
		}
	}
	await walk(root, '');
	return tree;
}

/** Groups files picked with `<input webkitdirectory>` (one top-level folder per tree). */
export function treesFromPicker(files: Iterable<File>): LocalTree[] {
	const trees = new Map<string, LocalTree>();
	for (const file of files) {
		const path = file.webkitRelativePath || file.name;
		const i = path.indexOf('/');
		const top = i > 0 ? path.slice(0, i) : '';
		const rel = i > 0 ? path.slice(i + 1) : path;
		let t = trees.get(top);
		if (!t) trees.set(top, (t = { name: top || 'folder', files: [], emptyDirs: [] }));
		t.files.push({ rel, file });
	}
	return [...trees.values()];
}
