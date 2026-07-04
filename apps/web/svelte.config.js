import adapter from '@sveltejs/adapter-static';

/** @type {import('@sveltejs/kit').Config} */
const config = {
  kit: {
    adapter: adapter({
      fallback: 'index.html'
    }),
    // Relative asset/base URLs so ONE static build works both at a GitHub Pages
    // subpath (/<repo>/, the Viewer) and at the local Ground Station daemon root
    // (/). The gcs/* capability probe is resolved relative to the document too.
    paths: {
      relative: true
    }
  }
};

export default config;

