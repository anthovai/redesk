import { api, type User } from './api';

// Logged-in user, shared across the app.
export const auth = $state<{ user: User | null; loaded: boolean }>({ user: null, loaded: false });

export async function loadMe() {
	try {
		auth.user = (await api.get<{ user: User }>('/api/auth/me')).user;
	} catch {
		auth.user = null;
	}
	auth.loaded = true;
}

export async function logout() {
	await api.post('/api/auth/logout').catch(() => {});
	auth.user = null;
}
