import { defineConfig, loadEnv } from "vite";
import react, { reactCompilerPreset } from "@vitejs/plugin-react";
import babel from "@rolldown/plugin-babel";
import path from "path";
import { precompression } from "./build/precompression";

// https://vite.dev/config/
export default defineConfig(({ mode }) => {
  // Load env file based on `mode` in the current working directory.
  const env = loadEnv(mode, process.cwd(), "");

  const apiTarget = env.VITE_API_URL || "http://localhost:8080";

  return {
    plugins: [react(), babel({ presets: [reactCompilerPreset({ compilationMode: "annotation" })] }), precompression()],

    resolve: {
      alias: {
        "@": path.resolve(__dirname, "src"),
      },
    },

    server: {
      port: 3000,
      host: true, // Listen on all addresses
      proxy: {
        // API proxy - forwards all /api requests to backend
        "/api": {
          target: apiTarget,
          changeOrigin: true,
          secure: false,
        },
        // WebSocket proxy for streaming
        "/ws": {
          target: apiTarget.replace("http", "ws"),
          ws: true,
          changeOrigin: true,
        },
      },
    },

    build: {
      outDir: "dist",
      manifest: true,
      sourcemap: mode === "development",
      // Optimize chunk splitting
      rolldownOptions: {
        output: {
          codeSplitting: {
            groups: [{
              name: "vendor",
              test: /[\\/]node_modules[\\/](react|react-dom|react-router|react-router-dom)[\\/]/,
            }],
          },
        },
      },
    },

    // Define environment variables available in the app
    define: {
      __APP_VERSION__: JSON.stringify(process.env.npm_package_version || "2.0.0"),
    },
  };
});
