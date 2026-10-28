import tailwindcss from '@tailwindcss/vite';
import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

// Dev: `bun run dev` proxies API and WebSocket traffic to the Go server on :21114.
// Start the server with AEGIRON_ALLOWED_ORIGINS=localhost:5173 so relay sockets are accepted.
const server = process.env.AEGIRON_SERVER ?? 'http://localhost:21114';

export default defineConfig({
	plugins: [
		tailwindcss(),
		sveltekit({
			compilerOptions: {
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true
			},
			// Single-page app served by aegiron-server (falls back to index.html for client routes).
			adapter: adapter({ fallback: 'index.html' })
		})
	],
	server: {
		proxy: {
			'/api': { target: server },
			'/ws': { target: server, ws: true }
		}
	}
});
