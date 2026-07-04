import { fileURLToPath } from 'node:url';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';
import { viteStaticCopy } from 'vite-plugin-static-copy';

const cesiumSource = fileURLToPath(new URL('../../node_modules/cesium/Build/Cesium', import.meta.url));
const cesiumBaseUrl = 'cesium';

// In dev, proxy the Ground Station API to the local daemon so the SAME hot-
// reloading dev server (:5173) runs as Ground Station: `gcs/health` succeeds
// (proxied), unlocking hardware panels, while every edit still live-reloads.
// Run `npm run ground-station` alongside `npm run dev`. (Change this if the
// daemon runs on a non-default address.)
const gcsTarget = 'http://127.0.0.1:8790';

export default defineConfig({
  define: {
    CESIUM_BASE_URL: JSON.stringify(`/${cesiumBaseUrl}`)
  },
  server: {
    proxy: {
      // ws: true so the live joystick inspector WebSocket (gcs/joystick) is
      // proxied too, not just HTTP.
      '/gcs': { target: gcsTarget, changeOrigin: true, ws: true }
    }
  },
  plugins: [
    sveltekit(),
    viteStaticCopy({
      targets: [
        { src: `${cesiumSource}/Workers`, dest: cesiumBaseUrl },
        { src: `${cesiumSource}/ThirdParty`, dest: cesiumBaseUrl },
        { src: `${cesiumSource}/Assets`, dest: cesiumBaseUrl },
        { src: `${cesiumSource}/Widgets`, dest: cesiumBaseUrl }
      ]
    })
  ],
  worker: {
    format: 'es'
  }
});
