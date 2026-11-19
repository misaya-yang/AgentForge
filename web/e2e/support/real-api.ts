import { test as base } from "@playwright/test";

export * from "@playwright/test";

/** API bearer setup has its own cookie-free context; pages retain BFF storage. */
export const test = base.extend({
  request: async ({ playwright, baseURL, extraHTTPHeaders }, provide) => {
    const request = await playwright.request.newContext({
      baseURL,
      extraHTTPHeaders,
      storageState: { cookies: [], origins: [] },
      ignoreHTTPSErrors: false,
    });
    try {
      await provide(request);
    } finally {
      await request.dispose();
    }
  },
});
