import adapter from '@sveltejs/adapter-static';

// LAPX_UI_OUT lets `npm test` build to a temp dir for byte-comparison
// against the committed build/ without touching the canonical artifacts.
const out = process.env.LAPX_UI_OUT ?? 'build';

/** @type {import('@sveltejs/kit').Config} */
export default {
	kit: {
		adapter: adapter({ pages: out, assets: out, fallback: undefined }),
		// Pinned so rebuilding the same sources reproduces the committed build/.
		version: { name: '0.1.0' }
	}
};
