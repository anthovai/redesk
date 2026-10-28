import type { Provider } from './api.js';

export interface ProviderInfo {
	label: string;
	short: string;
	color: string; // badge background (with matching text) — readable in light and dark
	kind: 'vendor' | 'protocol';
	addressLabel: string;
	addressHint: string;
	gateway: boolean; // can open in the browser through the ReDesk gateway
}

export const providers: Record<Provider, ProviderInfo> = {
	anydesk: {
		label: 'AnyDesk',
		short: 'AD',
		color: 'bg-red-500/15 text-red-600 dark:text-red-400',
		kind: 'vendor',
		addressLabel: 'AnyDesk ID หรือ alias',
		addressHint: '123456789 หรือ office-pc@ad',
		gateway: false
	},
	teamviewer: {
		label: 'TeamViewer',
		short: 'TV',
		color: 'bg-blue-500/15 text-blue-600 dark:text-blue-400',
		kind: 'vendor',
		addressLabel: 'TeamViewer ID',
		addressHint: '123 456 789',
		gateway: false
	},
	rustdesk: {
		label: 'RustDesk',
		short: 'RD',
		color: 'bg-orange-500/15 text-orange-600 dark:text-orange-400',
		kind: 'vendor',
		addressLabel: 'RustDesk ID',
		addressHint: '123456789',
		gateway: false
	},
	rdp: {
		label: 'Windows RDP',
		short: 'RDP',
		color: 'bg-sky-500/15 text-sky-700 dark:text-sky-300',
		kind: 'protocol',
		addressLabel: 'โฮสต์หรือ IP',
		addressHint: 'srv01.corp.local',
		gateway: true
	},
	vnc: {
		label: 'VNC',
		short: 'VNC',
		color: 'bg-violet-500/15 text-violet-700 dark:text-violet-300',
		kind: 'protocol',
		addressLabel: 'โฮสต์หรือ IP',
		addressHint: '192.168.1.20',
		gateway: true
	},
	ssh: {
		label: 'SSH',
		short: 'SSH',
		color: 'bg-zinc-500/15 text-zinc-700 dark:text-zinc-300',
		kind: 'protocol',
		addressLabel: 'โฮสต์หรือ IP',
		addressHint: 'server.example.com',
		gateway: true
	}
};

export const defaultPorts: Partial<Record<Provider, number>> = { rdp: 3389, vnc: 5900, ssh: 22 };
