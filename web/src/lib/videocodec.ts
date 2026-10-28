// WebCodecs setup for the video codecs in 0x04 frames (docs/PROTOCOL.md, Video codecs), shared
// by the live viewer and recording playback.

export const VIDEO_H264 = 1;
export const VIDEO_AV1 = 2;

// Constrained Baseline (what every agent encoder emits); Annex B, so no avcC description is needed.
const H264: VideoDecoderConfig = { codec: 'avc1.42E033', optimizeForLatency: true, hardwareAcceleration: 'no-preference' };

/** AV1 Main profile, 8-bit, with the lowest level that fits the picture (keyframes carry the sequence header). */
function av1(w: number, h: number): VideoDecoderConfig {
	const level = w * h <= 2048 * 1088 ? '08' : w * h <= 4096 * 2176 ? '12' : '16';
	return { codec: `av01.0.${level}M.08`, optimizeForLatency: true, hardwareAcceleration: 'no-preference' };
}

/** Decoder config for a 0x04 codec id, or null for an unknown codec. */
export function decoderConfig(codec: number, w: number, h: number): VideoDecoderConfig | null {
	if (codec === VIDEO_H264) return H264;
	if (codec === VIDEO_AV1) return av1(w, h);
	return null;
}

export type VideoCodec = 'h264' | 'av1';

/** Video codecs this browser decodes with WebCodecs (JPEG always works and is not listed). */
export async function decodableCodecs(): Promise<VideoCodec[]> {
	const out: VideoCodec[] = [];
	if (typeof VideoDecoder === 'undefined') return out;
	for (const [name, cfg] of [
		['h264', H264],
		['av1', av1(1920, 1080)]
	] as const) {
		try {
			if ((await VideoDecoder.isConfigSupported(cfg)).supported) out.push(name);
		} catch {
			// not supported
		}
	}
	return out;
}
