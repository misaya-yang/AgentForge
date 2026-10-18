import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import test from "node:test";
import { IntlMessageFormat } from "intl-messageformat";
import { createInstance } from "i18next";
import ICU from "i18next-icu";
test("every eager/deferred locale message parses as ICU, including literal braces and apostrophes", () => {
  const visit = (value: unknown, language: string) => {
    if (typeof value === "string") assert.doesNotThrow(() => new IntlMessageFormat(value, language), value);
    else if (value && typeof value === "object") Object.values(value).forEach(item => visit(item, language));
  };
  for (const file of readdirSync(new URL("./locales", import.meta.url)).filter(name => name.endsWith(".json"))) {
    visit(JSON.parse(readFileSync(new URL(`./locales/${file}`, import.meta.url), "utf8")), file.includes("zh-CN") ? "zh-CN" : "en-US");
  }
});
test("production ICU plugin renders plural, escaping-safe variable values and locale dates", async () => {
  const instance = createInstance().use(ICU);
  await instance.init({ lng: "en-US", resources: { "en-US": { translation: JSON.parse(readFileSync(new URL("./locales/en-US.json", import.meta.url), "utf8")) } } });
  assert.equal(instance.t("knowledgeKeys.days", { count: 1 }), "1 day");
  assert.equal(instance.t("knowledgeKeys.days", { count: 3 }), "3 days");
  assert.ok(instance.t("common.formattedDate", { value: new Date("2026-10-17T12:00:00Z") }).includes("2026"));
  assert.doesNotMatch(instance.t("knowledgeKeys.days", { count: 2 }), /\{count\}/);
});
