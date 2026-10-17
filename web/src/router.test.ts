import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import test from "node:test";
import vm from "node:vm";
import * as React from "react";
import * as jsxRuntime from "react/jsx-runtime";
import { renderToReadableStream, renderToString } from "react-dom/server";
import * as router from "react-router-dom";
import ts from "typescript";

const require = createRequire(import.meta.url);
const statusModule = "@/components/SystemStatusContent";
type Session = {
  hydrated: boolean;
  isAuthenticated: boolean;
  sessionValidation: "idle" | "checking" | "validated";
  user: { roles: string[] } | null;
  permissions: string[];
};

// Execute the real TSX router, guards, status pages, Logo and Button with React's
// own lazy/Suspense and server renderer. Only the session, translation hook and
// unrelated route modules are doubled. This is not browser/navigation evidence.
function fixture(initial: Partial<Session> = {}, enabled = true, namespaceReady: Promise<void> = Promise.resolve()) {
  const session: Session = {
    hydrated: true, isAuthenticated: false, sessionValidation: "validated",
    user: null, permissions: [], ...initial,
  };
  const modules = new Map<string, Record<string, unknown>>();
  const loads: string[] = [];
  const redirects: Record<string, unknown>[] = [];
  const namespaceLoads: string[] = [];
  let locale = "en-US";
  const translations = new Map(["en-US", "zh-CN"].map(language => [language,
    JSON.parse(readFileSync(new URL(`./i18n/locales/${language}.json`, import.meta.url), "utf8")),
  ]));
  const state = {
    ...session,
    hasPermission: (permission: string) => session.permissions.includes(permission),
    hasAnyPermission: (permissions: string[]) => permissions.some(permission => session.permissions.includes(permission)),
    hasAllPermissions: (permissions: string[]) => permissions.every(permission => session.permissions.includes(permission)),
  };
  const jsx = React.createElement;
  const doubles: Record<string, unknown> = {
    react: React,
    "react/jsx-runtime": jsxRuntime,
    "react-router-dom": {
      ...router,
      Navigate: (props: Record<string, unknown>) => { redirects.push(props); return null; },
    },
    "react-i18next": { useTranslation: () => ({
      t: (key: string) => key.split(".").reduce((value, part) => value?.[part], translations.get(locale)) ?? key,
    }) },
    "@/store/useAuthStore": { useAuthStore: (select: (value: typeof state) => unknown) => select({ ...state, ...session }) },
    "@/pages/assistant/components/ConnectorsPanel": { __esModule: true, default: ({ open, onClose }: { open: boolean; onClose: () => void }) => jsx("main", { "data-page": "PersonalConnections", "data-open": String(open) }, jsx("button", { onClick: onClose }, "Close")) },
    "@/hooks/useAuthenticatedReadScope": { useAuthenticatedReadScope: () => ({ id: "route-scope", isCurrent: () => session.isAuthenticated }) },
    "@/config/runtime": { isAgentStudioEnabled: () => enabled },
    "@/i18n": { loadTranslationNamespaces: async (...namespaces: string[]) => { namespaceLoads.push(...namespaces); await namespaceReady; } },
  };
  function load(name: string): Record<string, unknown> {
    if (name === "./SystemStatusContent") name = statusModule;
    if (name in doubles) return doubles[name] as Record<string, unknown>;
    const cached = modules.get(name);
    if (cached) return cached;
    if (!name.startsWith("@/")) return require(name) as Record<string, unknown>;
    loads.push(name);
    const exports: Record<string, unknown> = {};
    modules.set(name, exports);
    if (name === "@/layouts/AppLayout") {
      exports.AppLayout = () => jsx("section", { "data-page": "AppLayout" }, jsx(router.Outlet));
    } else if (name === "@/pages/knowledge/DatasetCreate") {
      exports.__esModule = true;
      exports.default = () => jsx("main", { "data-page": "DatasetCreatePage" }, "DatasetCreatePage");
    } else if (name.startsWith("@/pages/") && name !== "@/pages/Connections") {
      for (const component of ["LoginPage", "SharePage", "QuizPage", "WorkbenchPage", "AssistantPage", "AgentListPage", "AgentHostedPage", "KnowledgeDatasetsPage", "KnowledgeDatasetDetailPage", "EvalPage", "ConnectorsSettingsPage"]) {
        exports[component] = () => jsx("main", { "data-page": component }, component);
      }
    } else {
      const extension = name === "@/lib/utils" ? ".ts" : ".tsx";
      const source = readFileSync(new URL(`./${name.slice(2)}${extension}`, import.meta.url), "utf8");
      vm.runInNewContext(ts.transpileModule(source, { compilerOptions: {
        module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX,
      } }).outputText, { exports, require: (specifier: string) => load(specifier.startsWith(".")
        ? `@/${path.posix.normalize(path.posix.join(path.posix.dirname(name.slice(2)), specifier))}` : specifier) });
    }
    return exports;
  }
  const { AppRouter } = load("@/router") as { AppRouter: React.ComponentType };
  function tree(path: string) {
    return jsx(router.MemoryRouter, { initialEntries: [path] }, jsx(AppRouter));
  }
  return {
    loads, redirects, namespaceLoads, session,
    setLocale(language: string) { locale = language; },
    shell(path: string) { return renderToString(tree(path)); },
    async render(path: string) {
      const stream = await renderToReadableStream(tree(path));
      await stream.allReady;
      return new Response(stream).text();
    },
    guard(props: Record<string, unknown>) {
      const { ProtectedRoute } = load("@/components/ProtectedRoute") as { ProtectedRoute: React.ComponentType<Record<string, unknown>> };
      return renderToString(jsx(router.MemoryRouter, { initialEntries: ["/assistant?from=test"] },
        jsx(ProtectedRoute, props, jsx("main", null, "authorized child"))));
    },
  };
}

test("router and eager auth guard do not evaluate status-specific presentation until a status route renders", async () => {
  const f = fixture();
  assert.equal(f.loads.includes(statusModule), false);
  assert.match(await f.render("/login"), /data-page="LoginPage"/);
  assert.equal(f.loads.includes(statusModule), false);
  assert.deepEqual(f.namespaceLoads, []);
});

test("forbidden and unknown routes retain translated landmarks, icons and return links", async () => {
  for (const [path, kind, code] of [["/403", "forbidden", "403"], ["/missing/path", "notFound", "404"]]) {
    const f = fixture();
    assert.match(f.shell(path), /Loading\.\.\./);
    const html = await f.render(path);
    assert.match(html, new RegExp(`aria-labelledby="${kind}-title"`));
    assert.match(html, new RegExp(`id="${kind}-title"`));
    assert.match(html, new RegExp(`>${code}</h1>`));
    assert.match(html, /alt="AI Platform Logo"/);
    assert.match(html, /aria-hidden="true"/);
    assert.match(html, /href="\/login"/);
    assert.equal(html.includes("systemPages."), false);
    assert.equal(f.loads.filter(name => name === statusModule).length, 1);
    f.setLocale("zh-CN");
    const translated = await f.render(path);
    assert.notEqual(translated, html);
    assert.equal(translated.includes("systemPages."), false);
    assert.equal(f.loads.filter(name => name === statusModule).length, 1);
  }
});

test("switching between status routes shares the module and preserves each presentation", async () => {
  const f = fixture();
  for (const [path, kind] of [["/403", "forbidden"], ["/unknown", "notFound"], ["/403", "forbidden"]]) {
    assert.match(await f.render(path), new RegExp(`aria-labelledby="${kind}-title"`));
  }
  assert.equal(f.loads.filter(name => name === statusModule).length, 1);
});

test("status return destinations preserve permission priority and guest fallback", async () => {
  for (const [permissions, destination] of [
    [["console:dashboard:view", "conversation:playground:access"], "/dashboard"],
    [["conversation:playground:access"], "/playground"],
    [[], "/login"],
  ] as const) {
    const f = fixture({ isAuthenticated: true, permissions: [...permissions] });
    assert.match(await f.render("/403"), new RegExp(`href="${destination}"`));
  }
});

test("protected navigation still loads only the selected page and its unchanged namespace", async () => {
  const f = fixture({ isAuthenticated: true, permissions: ["conversation:playground:access"] });
  assert.match(await f.render("/assistant"), /data-page="AssistantPage"/);
  assert.equal(f.loads.includes(statusModule), false);
  assert.deepEqual(f.namespaceLoads, []);
  assert.match(await f.render("/agents"), /data-page="AgentListPage"/);
  assert.deepEqual(f.namespaceLoads, ["agents"]);
  assert.equal(f.loads.includes(statusModule), false);
});

test("disabled Agent Studio uses the same lazy not-found presentation without loading the feature", async () => {
  const f = fixture({ isAuthenticated: true }, false);
  assert.match(await f.render("/agents"), /aria-labelledby="notFound-title"/);
  assert.equal(f.loads.includes("@/pages/agents"), false);
  assert.deepEqual(f.namespaceLoads, []);
});

test("hydration and session checking still suppress protected children", () => {
  for (const session of [{ hydrated: false }, { sessionValidation: "checking" as const }]) {
    const f = fixture({ isAuthenticated: true, ...session });
    const html = f.guard({});
    assert.match(html, /Loading\.\.\./);
    assert.equal(html.includes("authorized child"), false);
    assert.equal(f.loads.includes(statusModule), false);
    assert.deepEqual(f.redirects, []);
  }
});

test("guest redirects keep the original location and replace history without loading status UI", () => {
  const f = fixture();
  assert.equal(f.guard({}), "");
  const redirect = f.redirects.at(-1)!;
  assert.equal(redirect.to, "/login");
  assert.equal(redirect.replace, true);
  const from = (redirect.state as { from: { pathname: string; search: string } }).from;
  assert.equal(from.pathname, "/assistant");
  assert.equal(from.search, "?from=test");
  assert.equal(f.loads.includes(statusModule), false);
});

test("role and any/all permission gates preserve their redirect and child-render behavior", () => {
  const f = fixture({ isAuthenticated: true, user: { roles: ["model_tester"] }, permissions: ["one"] });
  assert.equal(f.guard({ blockOnlyRole: "model_tester", blockRedirectTo: "/playground" }), "");
  assert.equal(f.redirects.at(-1)?.to, "/playground");
  assert.equal(f.guard({ requiredPermission: "two" }), "");
  assert.equal(f.redirects.at(-1)?.to, "/403");
  assert.match(f.guard({ requiredPermissions: ["one", "two"] }), /authorized child/);
  assert.equal(f.guard({ requiredPermissions: ["one", "two"], requireAll: true }), "");
  assert.equal(f.redirects.at(-1)?.to, "/403");
  f.session.user = { roles: ["model_tester", "admin"] };
  assert.match(f.guard({ blockOnlyRole: "model_tester" }), /authorized child/);
  assert.equal(f.loads.includes(statusModule), false);
});


test("Knowledge and Eval routes await all required namespaces while keeping the existing loading fallback", async () => {
  for (const [path, component, namespaces, permission] of [
    ["/knowledge/create", "DatasetCreatePage", ["knowledge"], "knowledge:dataset:create"],
    ["/knowledge/example", "KnowledgeDatasetDetailPage", ["knowledge", "eval"], "knowledge:dataset:view"],
    ["/eval", "EvalPage", ["eval", "knowledge"], "console:eval:view"],
  ] as const) {
    let release!: () => void;
    const gate = new Promise<void>(resolve => { release = resolve; });
    const f = fixture({ isAuthenticated: true, permissions: [permission] }, true, gate);
    assert.match(f.shell(path), /Loading\.\.\./);
    const rendering = f.render(path);
    await new Promise<void>(resolve => setImmediate(resolve));
    assert.deepEqual(f.namespaceLoads, [...namespaces]);
    assert.match(f.shell(path), /Loading\.\.\./);
    assert.equal(f.shell(path).includes(`data-page="${component}"`), false);
    release();
    assert.match(await rendering, new RegExp(`data-page="${component}"`));
    assert.match(await f.render(path), new RegExp(`data-page="${component}"`));
    assert.deepEqual(f.namespaceLoads, [...namespaces]);
  }
});

test("unauthorized Knowledge and Eval routes do not request deferred translations", async () => {
  for (const path of ["/knowledge", "/knowledge/create", "/knowledge/example", "/eval"]) {
    for (const isAuthenticated of [false, true]) {
      const f = fixture({ isAuthenticated });
      await f.render(path);
      assert.deepEqual(f.namespaceLoads, []);
      assert.equal(f.redirects.at(-1)?.to, isAuthenticated ? "/403" : "/login");
    }
  }
});


test("the Knowledge list renders with its eager translations without requesting deferred detail resources", async () => {
  const neverDelivered = new Promise<void>(() => {});
  const f = fixture({ isAuthenticated: true, permissions: ["knowledge:dataset:view"] }, true, neverDelivered);
  assert.match(await f.render("/knowledge"), /data-page="KnowledgeDatasetsPage"/);
  assert.deepEqual(f.namespaceLoads, []);
});


test("OAuth returns let ordinary authenticated members inspect personal connections without settings permission", async () => {
  for (const path of ["/connections", "/connections?connected=google%20drive&status=connected", "/connections?connected=forged&status=activated"]) {
    const f = fixture({ isAuthenticated: true, user: { roles: ["member"] }, permissions: [] });
    const html = await f.render(path);
    assert.match(html, /data-page="PersonalConnections"/);
    assert.match(html, /data-open="true"/);
    assert.equal(f.redirects.length, 0);
    assert.equal(f.loads.includes("@/pages/settings/ConnectorsSettings"), false);
    assert.equal(html.includes("forged"), false);
    assert.equal(html.includes("activated"), false);
  }
});

test("personal connections retain session validation and provider settings retain their permission gate", async () => {
  const anonymous = fixture();
  assert.doesNotMatch(await anonymous.render("/connections?status=connected"), /PersonalConnections/);
  assert.equal(anonymous.redirects[0]?.to, "/login");
  const hydrating = fixture({ hydrated: false });
  assert.doesNotMatch(await hydrating.render("/connections"), /PersonalConnections/);
  const checking = fixture({ isAuthenticated: true, sessionValidation: "checking" });
  assert.doesNotMatch(await checking.render("/connections"), /PersonalConnections/);
  const member = fixture({ isAuthenticated: true, user: { roles: ["member"] }, permissions: [] });
  assert.doesNotMatch(await member.render("/settings/connectors?connected=forged"), /ConnectorsSettingsPage/);
  assert.equal(member.redirects[0]?.to, "/403");
  const admin = fixture({ isAuthenticated: true, user: { roles: ["admin"] }, permissions: ["console:settings:view"] });
  assert.match(await admin.render("/settings/connectors"), /ConnectorsSettingsPage/);
});
