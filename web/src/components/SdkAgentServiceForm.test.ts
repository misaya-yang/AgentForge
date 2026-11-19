import ICU from "i18next-icu";
/* eslint-disable @typescript-eslint/no-explicit-any -- Controlled component/transport fixtures. */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { createInstance } from "i18next";

type Element = { type: unknown; props: Record<string, any> };
type Locale = "en-US" | "zh-CN";
function host(status?: number, locale?: Locale) {
  const i18n = createInstance().use(ICU);
  if (locale) {
    // Real locale resolution catches namespace mistakes hidden by t(key) => key.
    void i18n.init({
      lng: locale, fallbackLng: false, initAsync: false,
      resources: { [locale]: { translation: JSON.parse(readFileSync(new URL(`../i18n/locales/${locale}.json`, import.meta.url), "utf8")) } },
    });
  }
  const writes: { url: string; body: Record<string, any> }[] = [];
  const slots: any[] = []; let cursor = 0; let registered = 0;
  let perform = async () => { if (status) throw { response: { status, data: "fixture-secret" }, message: "fixture-secret" }; };
  const element = (type: unknown, props: Record<string, any>) => ({ type, props });
  const modules: Record<string, any> = {
    react: { useRef: (value: any) => { const i = cursor++; return slots[i] ?? (slots[i] = { current: value }); },
      useState: (value: any) => { const i = cursor++; if (!(i in slots)) slots[i] = value; return [slots[i], (next: any) => { slots[i] = next; }]; },
      useEffect: () => {} },
    "react/jsx-runtime": { jsx: element, jsxs: element },
    "react-i18next": { useTranslation: () => ({ t: (key: string, options?: Record<string, any>) => locale ? i18n.t(key, options) : key }) },
    "@/lib/api": { api: { post: async (url: string, body: Record<string, any>) => {
      writes.push({ url, body: structuredClone(body) }); await perform(); return { data: { service_id: "sdk-test", status: "registered" } };
    } } },
    "@/store/useAuthStore": { useAuthStore: (select: (value: any) => any) => select({ hasPermission: () => true }) },
    "@/components/SdkKnowledgePicker": { SdkKnowledgePicker: "SdkKnowledgePicker" },
    "@/components/SdkCapabilityPicker": { SdkCapabilityPicker: "SdkCapabilityPicker" },
    "@/lib/sdkKnowledgeSelection": { validateSdkKnowledgeSelection: () => [] },
    "@/lib/sdkCapabilitySelection": { validateSdkCapabilitySelection: () => [] },
  };
  for (const [name, component] of [["button", "Button"], ["input", "Input"], ["label", "Label"], ["textarea", "Textarea"]]) modules[`@/components/ui/${name}`] = { [component]: component };
  const evaluate = (path: string, transform = (source: string) => source) => {
    const exports: Record<string, any> = {};
    vm.runInNewContext(ts.transpileModule(transform(readFileSync(new URL(path, import.meta.url), "utf8")), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
    }).outputText, { exports, Error, require: (name: string) => { assert.ok(modules[name], name); return modules[name]; } });
    return exports;
  };
  const gateway = evaluate("../api/gateway.ts"); modules["@/api/gateway"] = gateway;
  modules["@/lib/sdkAgentConfig"] = evaluate("../lib/sdkAgentConfig.ts");
  const form = evaluate("./SdkAgentServiceForm.tsx", source => source.replace("function SdkAgentServiceFields(", "export function SdkAgentServiceFields("));
  function render() { cursor = 0; return form.SdkAgentServiceFields({ onRegistered() { registered++; }, onCancel() {}, onBusyChange() {} }) as Element; }
  function nodes() {
    const all: Element[] = []; const visit = (node: any) => { if (Array.isArray(node)) node.forEach(visit);
      else if (node?.props) { all.push(node); visit(node.props.children); } }; visit(render());
    return all;
  }
  function find(predicate: (node: Element) => boolean) {
    const found = nodes().find(predicate); assert.ok(found); return found;
  }
  const change = (id: string, value: string) => find(node => node.props.id === id).props.onChange({ target: { value } });
  change("sdk-service-id", "sdk-test"); change("sdk-service-name", "SDK test"); change("sdk-model", "model-a"); change("sdk-provider-key", "fixture-provider-key");
  return { writes, gateway, render, nodes, find, change, get registered() { return registered; },
    submit: () => find(node => node.type === "form").props.onSubmit({ preventDefault() {} }),
    setPerform(fn: typeof perform) { perform = fn; } };
}
for (const framework of ["openai_agents", "claude_agent_sdk"]) {
  test(`${framework} typed form creates once only on atomic endpoint`, async () => {
    const h = host();
    h.find(node => node.props.type === "radio" && node.props.value === framework).props.onChange();
    h.change("sdk-model", "model-a"); h.change("sdk-provider-key", "fixture-provider-key");
    await h.submit(); await h.submit();
    assert.equal(h.registered, 1); assert.equal(h.writes.length, 1);
    assert.equal(h.writes[0].url, "/api/v1/service-registrations");
    assert.equal(h.writes[0].body.metadata.adapter_type, framework);
    assert.equal(h.find(node => node.props.id === "sdk-provider-key").props.value, "");
  });
}
for (const status of [400, 401, 403, 404, 405, 408, 409, 422, 500, 501]) {
  test(`HTTP ${status} never falls back or replays and retains a sanitized failure`, async () => {
    const h = host(status); await h.submit(); await h.submit();
    assert.equal(h.writes.length, 1); assert.equal(h.writes[0].url, "/api/v1/service-registrations");
    const expected = [404, 405, 501].includes(status) ? "registrationUnavailable" : status === 409 ? "registrationConflict" : "registerFailed";
    assert.equal(h.find(node => node.props.role === "alert").props.children, `services.sdkAgent.${expected}`);
    assert.equal(h.find(node => node.props.type === "submit").props.disabled, true);
    assert.equal(JSON.stringify(h.render()).includes("fixture-secret"), false);
  });
}
test("transport failure and duplicate pending submits are fenced without fallback", async () => {
  const h = host(); let reject!: (reason: Error) => void;
  h.setPerform(() => new Promise<void>((_resolve, no) => { reject = no; }));
  const first = h.submit(); await h.submit(); assert.equal(h.writes.length, 1);
  reject(new Error("fixture-secret")); await first; await h.submit();
  assert.equal(h.writes.length, 1); assert.equal(JSON.stringify(h.render()).includes("fixture-secret"), false);
});
test("legacy registration retains its existing route", async () => {
  const h = host(); await h.gateway.registerService({ service_id: "legacy" });
  assert.equal(h.writes[0].url, "/api/v1/services");
});

for (const locale of ["en-US", "zh-CN"] as const) {
  test(`${locale} shows native Gateway prerequisites before provider-key entry`, () => {
    const h = host(undefined, locale);
    const nodes = h.nodes();
    const hintIndex = nodes.findIndex(node => node.props.id === "sdk-runtime-hint");
    const keyIndex = nodes.findIndex(node => node.props.id === "sdk-provider-key");
    assert.ok(hintIndex >= 0 && hintIndex < keyIndex);
    const hint = nodes[hintIndex].props.children;
    assert.equal(typeof hint, "string");
    assert.ok(hint.includes(locale === "en-US" ? "native Gateway" : "原生 Gateway"));
    assert.ok(hint.includes("Python Gateway"));
    assert.ok(hint.includes("Python Knowledge"));
    assert.ok(hint.includes(locale === "en-US" ? "worker alone is insufficient" : "仅启用 Worker 并不足够"));
    assert.equal(nodes[keyIndex].props["aria-describedby"], "sdk-runtime-hint sdk-key-hint");
    assert.equal(h.writes.length, 0);
    assert.equal(hint.includes("services.sdkAgent."), false);
  });

  for (const status of [404, 405, 409, 500, 501]) {
    test(`${locale} SDK HTTP ${status} resolves the actual locale and exact recovery ID`, async () => {
      const h = host(status, locale); await h.submit(); await h.submit();
      const message = h.find(node => node.props.role === "alert").props.children;
      assert.equal(typeof message, "string");
      assert.ok(message.includes("sdk-test"), "The attempted ID must survive every SDK error.");
      assert.equal(message.includes("services.sdkAgent."), false, "Missing locale keys must not reach the form.");
      assert.equal(message.includes("{{serviceId}}"), false);
      if ([404, 405, 501].includes(status)) {
        assert.ok(message.includes(locale === "en-US" ? "native Gateway" : "原生 Gateway"));
        assert.ok(message.includes(locale === "en-US" ? "deployment and routing" : "部署与路由"));
        assert.ok(message.includes(locale === "en-US" ? "No legacy fallback or automatic retry" : "未尝试旧接口，也未自动重试"));
      } else if (status === 409) {
        assert.ok(message.includes(locale === "en-US" ? "conflict" : "冲突"));
      } else {
        assert.ok(message.includes(locale === "en-US" ? "unconfirmed" : "无法确认"));
        assert.ok(message.includes(locale === "en-US" ? "may still complete" : "仍可能完成"));
        assert.ok(message.includes(locale === "en-US" ? "will not be replayed" : "不会重复发送"));
      }
      assert.equal(h.writes.length, 1);
      assert.equal(h.writes[0].url, "/api/v1/service-registrations");
      assert.equal(h.find(node => node.props.id === "sdk-provider-key").props.value, "");
      assert.equal(h.find(node => node.props.type === "submit").props.disabled, true);
      assert.equal(JSON.stringify(h.render()).includes("fixture-secret"), false);
    });
  }
}
