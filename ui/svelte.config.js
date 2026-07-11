import adapter from '@sveltejs/adapter-static';

/** @type {import('@sveltejs/kit').Config} */
export default {
	kit: {
		adapter: adapter({ fallback: undefined }),
		// Pinned so rebuilding the same sources reproduces the committed build/.
		version: { name: '0.1.0' }
	}
};
