import { defineConfig } from "astro/config";

// Fully static, zero-JS-by-default output. CSS is inlined into the HTML so the
// first paint needs exactly one request (plus the two preloaded font files).
export default defineConfig({
  site: "https://ntulearn.krtk.app",
  output: "static",
  compressHTML: true,
  build: {
    inlineStylesheets: "always",
    assets: "_assets",
  },
});
