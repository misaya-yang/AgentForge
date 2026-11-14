import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import vm from "node:vm";
import * as React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import ts from "typescript";

const require = createRequire(import.meta.url);
type Props = Record<string, unknown>;
type Adapter = React.ComponentType<Props>;
class TestKeyboardEvent extends Event {}
function harness(realPrimitives = false) {
  const captured: Record<string, Props> = {};
  const components = Object.fromEntries(["Select", "Tabs", "Switch", "Modal", "Dropdown", "Menu", "Tooltip", "Button", "Alert"].map(name => [name, (props: Props) => {
    captured[name] = props;
    if (name === "Dropdown") return React.createElement(React.Fragment, null, props.children as React.ReactNode, (props.popupRender as () => React.ReactNode)());
    return React.createElement(React.Fragment, null, props.children as React.ReactNode);
  }]));
  const radix = (prefix: string, names: string[]) => Object.fromEntries(names.map(name => [name, (props: Props) => {
    captured[`${prefix}${name}${typeof props.value === "string" ? `:${props.value}` : ""}`] = props;
    return React.createElement(React.Fragment, null, props.children as React.ReactNode);
  }]));
  const tabs = radix("Tabs", ["Root", "List", "Trigger", "Content"]);
  const menu = radix("Menu", ["Root", "Trigger", "Portal", "Content", "Item", "CheckboxItem", "RadioItem", "RadioGroup", "Label", "Separator", "ItemIndicator", "Group", "Sub", "SubTrigger", "SubContent"]);
  const cache: Record<string, Record<string, Adapter>> = {};
  function load(name: string): Record<string, Adapter> {
    if (cache[name]) return cache[name];
    const exports: Record<string, Adapter> = {};
    cache[name] = exports;
    const code = ts.transpileModule(readFileSync(new URL(`./${name}.tsx`, import.meta.url), "utf8"), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 },
    }).outputText;
    vm.runInNewContext(code, { exports, Event, KeyboardEvent: TestKeyboardEvent, require: (id: string) => {
      if (id.startsWith("./")) return load(id.slice(2));
      if (id === "antd") return components;
      if (id === "@radix-ui/react-tabs" && !realPrimitives) return tabs;
      if (id === "@radix-ui/react-dropdown-menu" && !realPrimitives) return menu;
      if (id === "@/components/ui/native-modal") return { Modal: components.Modal };
      if (id === "@/lib/utils") return { cn: (...items: unknown[]) => items.filter(Boolean).join(" ") };
      return require(id);
    } });
    return exports;
  }
  return { captured, load, render: (node: React.ReactNode) => renderToStaticMarkup(node) };
}
const el = React.createElement;

test("Select preserves controlled value, disabled options, labels, callbacks and popup ownership", () => {
  const host = harness(); const ui = host.load("select"); let selected = "";
  host.render(el(ui.Select, { value: "one", onValueChange: (value: string) => { selected = value; } },
    el(ui.SelectTrigger, { id: "model", "aria-label": "Model", "data-testid": "model-choice" }, el(ui.SelectValue, { placeholder: "Choose" })),
    el(ui.SelectContent, { disablePortal: true }, el(React.Fragment, null,
      el(ui.SelectItem, { value: "one" }, "One"), el(ui.SelectItem, { value: "two", disabled: true }, "Two")))));
  const props = host.captured.Select;
  assert.equal(props.value, "one"); assert.equal(props.placeholder, "Choose"); assert.equal(props.id, "model");
  assert.equal(props["data-testid"], "model-choice"); assert.equal(props["aria-label"], "Model");
  assert.equal((props.options as Props[])[1].disabled, true);
  (props.onChange as (value: string) => void)("two"); assert.equal(selected, "two");
  const parent = {}; assert.equal((props.getPopupContainer as (node: unknown) => unknown)({ parentElement: parent }), parent);
});

test("Tabs finds a wrapped tab list and keeps force-mounted settings panels", () => {
  const host = harness(); const ui = host.load("tabs"); let next = "";
  host.render(el(ui.Tabs, { defaultValue: "auth", onValueChange: (value: string) => { next = value; } },
    el("div", { className: "scroll-container" }, "Settings", el(ui.TabsList, null,
      el(ui.TabsTrigger, { value: "auth" }, "Authentication"), el(ui.TabsTrigger, { value: "keys", disabled: true }, "Keys"))),
    el(ui.TabsContent, { value: "auth", forceMount: true }, "Unsaved form"), el(ui.TabsContent, { value: "keys" }, "Keys form")));
  const props = host.captured["TabsRoot:auth"];
  assert.equal(props.value, "auth");
  assert.equal(host.captured["TabsContent:auth"].forceMount, true);
  assert.equal(host.captured["TabsContent:auth"].hidden, false);
  assert.equal(host.captured["TabsContent:auth"].tabIndex, 0);
  assert.equal(host.captured["TabsTrigger:keys"].disabled, true);
  assert.equal(host.captured["TabsContent:keys"].hidden, true);
  assert.equal(host.captured["TabsContent:keys"].tabIndex, -1);
  (props.onValueChange as (value: string) => void)("keys"); assert.equal(next, "keys");
});

test("Dropdown preserves nested provider entries, radio changes and cancellable selection", () => {
  const host = harness(); const ui = host.load("dropdown-menu"); let selected = ""; let clicks = 0;
  host.render(el(ui.DropdownMenu, null, el(ui.DropdownMenuTrigger, { asChild: true }, el("button", null, "Models")),
    el(ui.DropdownMenuContent, { align: "end" }, el("div", null,
      el(ui.DropdownMenuItem, { onSelect: (event: Event) => { clicks++; event.preventDefault(); } }, "Action")),
      el(ui.DropdownMenuRadioGroup, { value: "auto", onValueChange: (value: string) => { selected = value; } },
        el(ui.DropdownMenuRadioItem, { value: "high" }, "High")))));
  const selection = new Event("select", { cancelable: true });
  (host.captured.MenuItem.onSelect as (event: Event) => void)(selection);
  assert.equal(clicks, 1); assert.equal(selection.defaultPrevented, true); assert.equal(selected, "");
  const radio = host.captured["MenuRadioGroup:auto"];
  assert.equal(host.captured["MenuRadioItem:high"].value, "high");
  (radio.onValueChange as (value: string) => void)("high");
  assert.equal(selected, "high"); assert.equal(host.captured.MenuContent.align, "end");
  assert.equal(host.captured.MenuContent.sideOffset, 4);
});

test("asChild composes child and adapter handlers and respects cancellation", () => {
  const host = harness(); const composition = host.load("composition") as unknown as { childWithProps: (child: React.ReactNode, props: Props) => React.ReactElement<Props> };
  const calls: string[] = [];
  const node = composition.childWithProps(el("button", { className: "child", onClick: (event: Event) => { calls.push("child"); event.preventDefault(); } }), {
    className: "adapter", onClick: () => calls.push("adapter"),
  });
  (node.props.onClick as (event: Event) => void)(new Event("click", { cancelable: true }));
  assert.deepEqual(calls, ["child"]); assert.equal(node.props.className, "child adapter");
});

test("dependency manifest and lint rule prevent retired design-system imports", () => {
  const packageJson = JSON.parse(readFileSync(new URL("../../../package.json", import.meta.url), "utf8"));
  const config = readFileSync(new URL("../../../eslint.config.js", import.meta.url), "utf8");
  for (const name of ["tailwind-merge", "class-variance-authority", "date-fns", "marked", "react-grid-layout", "react-resizable", "@langchain/core", "@langchain/langgraph-sdk", "framer-motion"]) assert.equal(packageJson.dependencies[name], undefined, name);
  assert.ok(packageJson.dependencies["@radix-ui/react-toggle-group"]);
  assert.ok(!Object.keys(packageJson.dependencies).some(name => name.startsWith("@blocknote/xl-")));
  assert.match(config, /no-restricted-imports/); assert.match(config, /date-fns\/\*/);
});


test("Dialog cancellation honors protected password changes and uses one accessible dialog", () => {
  const host = harness(); const ui = host.load("dialog"); let closed = 0;
  host.render(el(ui.Dialog, { open: true, onOpenChange: () => { closed++; } },
    el(ui.DialogContent, { onEscapeKeyDown: (event: Event) => event.preventDefault(), onInteractOutside: (event: Event) => event.preventDefault() },
      el(ui.DialogTitle, null, "Change password"), el(ui.DialogDescription, null, "Required"))));
  const props = host.captured.Modal;
  const cancel = props.onCancel as (event: unknown) => void;
  cancel({ nativeEvent: new TestKeyboardEvent("keydown", { cancelable: true }) });
  cancel({ nativeEvent: new Event("click", { cancelable: true }), target: { closest: () => null } });
  assert.equal(closed, 0);
  cancel({ nativeEvent: new Event("click", { cancelable: true }), target: { closest: () => ({}) } });
  assert.equal(closed, 1);
  const attributes: Record<string, string> = {};
  (props.panelRef as (node: unknown) => void)({ setAttribute: (name: string, value: string) => { attributes[name] = value; } });
  assert.equal(attributes.role, undefined); assert.ok(attributes["aria-labelledby"].endsWith("-title"));
  assert.equal(props.modalRender, undefined);
});

test("AlertDialog requires an explicit choice and exposes alertdialog semantics", () => {
  const host = harness(); const ui = host.load("alert-dialog");
  host.render(el(ui.AlertDialog, { open: true }, el(ui.AlertDialogContent, null, el(ui.AlertDialogTitle, null, "Delete?"))));
  const props = host.captured.Modal;
  assert.equal(props.closable, false); assert.equal(props.keyboard, false); assert.equal((props.mask as Props).closable, false);
  let role = "";
  (props.panelRef as (node: unknown) => void)({ setAttribute: (name: string, value: string) => { if (name === "role") role = value; } });
  assert.equal(role, "alertdialog");
});


test("Switch keeps checked state, form value and change callbacks", () => {
  const host = harness(); const ui = host.load("switch"); let changed = false;
  const html = host.render(el(ui.Switch, { name: "enabled", value: "yes", checked: true, onCheckedChange: (value: boolean) => { changed = value; } }));
  assert.match(html, /name="enabled"/); assert.match(html, /value="yes"/); assert.match(html, /checked/);
  assert.equal(host.captured.Switch.checked, true);
  (host.captured.Switch.onChange as (value: boolean) => void)(true); assert.equal(changed, true);
});

test("ESLint rejects both static and lazy imports of retired dependencies", async () => {
  const { ESLint } = require("eslint");
  const eslint = new ESLint({ cwd: new URL("../../../", import.meta.url).pathname });
  for (const source of ['import "tailwind-merge";', 'void import("tailwind-merge");', 'import "date-fns/locale";', 'void import("@langchain/core");', 'void import("marked");', 'import "framer-motion";', 'void import("framer-motion");']) {
    const [result] = await eslint.lintText(source, { filePath: "src/design-system-import-probe.ts" });
    assert.ok(result.messages.some((message: { ruleId: string }) => message.ruleId === "no-restricted-imports" || message.ruleId === "no-restricted-syntax"), source);
  }
});

test("Button asChild renders a single real router link with its original label", () => {
  const host = harness(); const { Button } = host.load("button");
  const { Link, MemoryRouter } = require("react-router-dom");
  const html = host.render(el(MemoryRouter, null, el(Button, { asChild: true },
    el(Link, { to: "/dashboard", "aria-label": "Go" }, el("span", null, "Go")))));
  assert.equal((html.match(/<a\b/g) ?? []).length, 1, html);
  assert.equal((html.match(/href="\/dashboard"/g) ?? []).length, 1, html);
  assert.match(html, /<span>Go<\/span><\/a>/);
});

test("asChild retains original children while composing styles, events and both refs", () => {
  const host = harness();
  const { childWithProps } = host.load("composition") as unknown as {
    childWithProps: (child: React.ReactNode, props: Props) => React.ReactElement<Props>;
  };
  const calls: string[] = []; let attached: unknown; let childCleaned = false;
  const outerRef = React.createRef<unknown>();
  const label = el("span", null, "Original label");
  const child = el("a", {
    href: "/dashboard", style: { color: "red", padding: 1 },
    ref: (node: unknown) => { attached = node; return () => { childCleaned = true; }; },
    onClick: () => calls.push("child"),
  }, label);
  const node = childWithProps(child, {
    children: child, ref: outerRef, style: { padding: 2 }, onClick: () => calls.push("slot"),
  });
  assert.equal(node.props.children, label);
  assert.deepEqual({ ...(node.props.style as Props) }, { color: "red", padding: 2 });
  (node.props.onClick as (event: Event) => void)(new Event("click", { cancelable: true }));
  assert.deepEqual(calls, ["child", "slot"]);
  const element = {};
  const cleanup = (node.props.ref as (node: unknown) => () => void)(element);
  assert.equal(attached, element); assert.equal(outerRef.current, element);
  cleanup(); assert.equal(childCleaned, true); assert.equal(outerRef.current, null);
});


test("real Radix tabs choose the first tab and keep inactive forced panels hidden", () => {
  const host = harness(true); const ui = host.load("tabs");
  const html = host.render(el(ui.Tabs, null,
    el("div", null, el(ui.TabsList, { "aria-label": "Configuration" },
      el(ui.TabsTrigger, { value: "first" }, "First"), el(ui.TabsTrigger, { value: "second", disabled: true }, "Second"))),
    el(ui.TabsContent, { value: "first" }, "Visible form"), el(ui.TabsContent, { value: "second", forceMount: true }, "Retained form")));
  assert.match(html, /role="tablist"/); assert.match(html, /aria-selected="true"/);
  assert.match(html, /aria-disabled|disabled/); assert.match(html, /Visible form/);
  assert.match(html, /<div[^>]*role="tabpanel"[^>]*hidden=""[^>]*>Retained form<\/div>/);
});

test("real Radix tooltips render a single named trigger without requiring a surrounding provider", () => {
  const host = harness(true); const ui = host.load("tooltip");
  const html = host.render(el(ui.Tooltip, null,
    el(ui.TooltipTrigger, { asChild: true }, el("button", { "aria-label": "Explain retrieval" }, "?")),
    el(ui.TooltipContent, null, "Searches the selected knowledge bases")));
  assert.equal((html.match(/<button\b/g) ?? []).length, 1);
  assert.match(html, /aria-label="Explain retrieval"/);
});
