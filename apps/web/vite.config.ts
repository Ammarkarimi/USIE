import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// In development the API runs separately (default :8080) and is proxied.
const api = process.env.USIE_API_URL ?? "http://127.0.0.1:8080";

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: { "/api": { target: api, changeOrigin: false }, "/healthz": api },
  },
  build: {
    target: "es2022",
    sourcemap: true,
    chunkSizeWarningLimit: 4000,
    rollupOptions: {
      output: {
        manualChunks: { react: ["react", "react-dom", "react-router-dom"], cytoscape: ["cytoscape"] },
      },
    },
  },
  worker: { format: "es" },
});
