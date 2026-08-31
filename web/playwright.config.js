import { defineConfig, devices } from "@playwright/test";

// Fixed so that `serve.mjs`, the base URL and the CI job all name the same port.
const PORT = 4173;

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  reporter: "list",
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    trace: "on-first-retry",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: `node serve.mjs ${PORT}`,
    url: `http://127.0.0.1:${PORT}/index.html`,
    // Locally the server is often already up from `make web`; CI always starts its own.
    reuseExistingServer: !process.env.CI,
    stdout: "pipe",
  },
});
