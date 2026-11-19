import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const web = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const escape = value => value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
function children(dir) {
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap(entry => entry.isDirectory() ? children(path.join(dir, entry.name)) : /\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name) ? [path.join(dir, entry.name)] : []);
}
function declaration(name, use) {
  for (let scope = use.parent; scope; scope = scope.parent) {
    if (ts.isFunctionLike(scope)) {
      const parameter = scope.parameters.find(item => item.name.getText() === name);
      if (parameter) return parameter;
    }
    for (const statement of scope.statements ?? []) {
      if (ts.isVariableStatement(statement)) for (const item of statement.declarationList.declarations) if (item.name.getText() === name && item.initializer) return item.initializer;
      if (ts.isFunctionDeclaration(statement) && statement.name?.text === name) return statement;
      if (ts.isTypeAliasDeclaration(statement) && statement.name.text === name) return statement.type;
    }
  }
}
const unique = values => [...new Set(values)];
function product(left, right) {
  if (left.length * right.length > 256) throw new Error("API URL expansion exceeds 256 alternatives");
  return unique(left.flatMap(a => right.map(b => a + b)));
}
/** Enumerate conditional URLs and literal action unions; scalar IDs remain slots. */
export function patterns(node, bindings = {}, depth = 0) {
  if (!node || depth > 20) return ["@"];
  if (ts.isStringLiteralLike(node)) return [node.text];
  if (ts.isParenthesizedExpression(node) || ts.isAsExpression(node) || ts.isNonNullExpression(node) || ts.isLiteralTypeNode(node)) return patterns(node.expression ?? node.literal, bindings, depth + 1);
  if (ts.isUnionTypeNode(node)) return unique(node.types.flatMap(type => patterns(type, bindings, depth + 1)));
  if (ts.isTypeReferenceNode(node)) return patterns(declaration(node.typeName.getText(), node), bindings, depth + 1);
  if (ts.isParameter(node)) return patterns(node.type, bindings, depth + 1);
  if (ts.isTemplateExpression(node)) return node.templateSpans.reduce((values, span) => product(product(values, patterns(span.expression, bindings, depth + 1)), [span.literal.text]), [node.head.text]);
  if (ts.isConditionalExpression(node)) return unique([...patterns(node.whenTrue, bindings, depth + 1), ...patterns(node.whenFalse, bindings, depth + 1)]);
  if (ts.isIdentifier(node)) {
    if (Object.hasOwn(bindings, node.text)) return bindings[node.text];
    const value = declaration(node.text, node);
    if (value && !ts.isFunctionDeclaration(value)) return patterns(value, bindings, depth + 1);
  }
  if (ts.isBinaryExpression(node) && node.operatorToken.kind === ts.SyntaxKind.PlusToken) return product(patterns(node.left, bindings, depth + 1), patterns(node.right, bindings, depth + 1));
  if (ts.isCallExpression(node) && ts.isIdentifier(node.expression)) {
    if (node.expression.text === "encodeURIComponent") return patterns(node.arguments[0], bindings, depth + 1).map(value => value === "@" ? value : encodeURIComponent(value));
    const fn = declaration(node.expression.text, node);
    if (fn && (ts.isFunctionDeclaration(fn) || ts.isArrowFunction(fn) || ts.isFunctionExpression(fn))) {
      const body = fn.body && ts.isBlock(fn.body) ? fn.body.statements.find(ts.isReturnStatement)?.expression : fn.body;
      if (body) return patterns(body, { ...bindings, ...Object.fromEntries(fn.parameters.map((parameter, index) => [parameter.name.getText(), patterns(node.arguments[index], bindings, depth + 1)])) }, depth + 1);
    }
  }
  return ["@"];
}
function field(node, name) {
  return node && ts.isObjectLiteralExpression(node) ? node.properties.find(item => ts.isPropertyAssignment(item) && item.name.getText().replaceAll(/["']/g, "") === name)?.initializer : undefined;
}
export function operationMatches(method, url, operation) {
  const split = operation.indexOf(" "); if (operation.slice(0, split) !== method) return false;
  // A catch-all router is a dispatch boundary, not documentation for every
  // conceivable Console sub-operation below it. Concrete owner declarations
  // must establish that a screen's action actually exists.
  if (/\{\*?(?:path|rest|tail|adapter_schema)\}/.test(operation)) return false;
  const expected = operation.slice(split + 1).split("/"), actual = url.split("/");
  if (expected.length !== actual.length) return false;
  return expected.every((segment, index) => {
    const value = actual[index];
    // An unresolved ID can match a declared path parameter, but must never
    // silently stand in for a literal action such as /approve or /reindex.
    if (value.includes("@")) return new RegExp(`^${escape(value).replaceAll("@", "\\{[^}]+\\}")}$`).test(segment);
    return new RegExp(`^${escape(segment).replace(/\\\{[^}]+\\\}/g, "[^/]+")}$`).test(value);
  });
}
export function scanSource(source, published, bodySchemas, filename = "fixture.ts") {
  const issues = [], schemaGaps = [], checked = [];
  function visit(node) {
    if (ts.isCallExpression(node)) {
      let methods, urls, sendsBody = false;
      if (ts.isPropertyAccessExpression(node.expression) && ["api", "apiClient"].includes(node.expression.expression.getText(source))) {
        const method = node.expression.name.text;
        if (/^(get|post|put|patch|delete|head|options)$/.test(method)) {
          methods = [method.toUpperCase()]; urls = patterns(node.arguments[0]);
          sendsBody = ["post", "put", "patch"].includes(method) ? Boolean(node.arguments[1]) && node.arguments[1].getText(source) !== "undefined" : Boolean(field(node.arguments[1], "data"));
        } else if (method === "request") {
          methods = patterns(field(node.arguments[0], "method")).map(value => value.toUpperCase()); urls = patterns(field(node.arguments[0], "url")); sendsBody = Boolean(field(node.arguments[0], "data"));
        }
      } else if (ts.isIdentifier(node.expression) && node.expression.text === "request") {
        const keys = patterns(node.arguments[0]);
        if (keys.some(key => /^[A-Z]+ \/(?:api|v1)\//.test(key))) {
          for (const key of keys) {
            const split = key.indexOf(" "); check(key.slice(0, split), key.slice(split + 1), Boolean(field(node.arguments[1], "body")), node);
          }
        }
      } else if ((ts.isIdentifier(node.expression) && /^(fetch|sseFetch|sseFetchEvents)$/.test(node.expression.text)) || (ts.isPropertyAccessExpression(node.expression) && node.expression.name.text === "fetch")) {
        urls = patterns(node.arguments[0]); const method = field(node.arguments[1], "method"); methods = method ? patterns(method) : ["GET"]; sendsBody = Boolean(field(node.arguments[1], "body"));
      }
      for (const method of methods ?? []) for (const url of urls ?? []) check(method, url, sendsBody, node);
    }
    ts.forEachChild(node, visit);
  }
  function check(method, url, sendsBody, node) {
    const clean = url.replace(/^@(?=\/api\/)/, "").split("?")[0], location = `${filename}:${source.getLineAndCharacterOfPosition(node.getStart(source)).line + 1}`;
    if (/^\/(api|v1)\//.test(clean)) {
      const matches = published.filter(operation => operationMatches(method, clean, operation));
      const entry = { location, operation: `${method} ${clean}`, matches };
      checked.push(entry); if (!matches.length) issues.push(entry);
      else if (sendsBody && matches.every(operation => ["never", "unknown", undefined].includes(bodySchemas.get(operation)))) schemaGaps.push(entry);
    } else if (url === "@" && ts.isPropertyAccessExpression(node.expression) && ["api", "apiClient"].includes(node.expression.expression.getText(source))) issues.push({ location, operation: `${method} <unresolved>`, matches: [] });
  }
  visit(source); return { checked, issues, schemaGaps };
}
function bodyKind(node, aliases, stack = new Set()) {
  if (!node) return "unknown";
  if (node.kind === ts.SyntaxKind.AnyKeyword || node.kind === ts.SyntaxKind.UnknownKeyword) return "unknown";
  if (node.kind === ts.SyntaxKind.NeverKeyword || node.kind === ts.SyntaxKind.VoidKeyword || node.kind === ts.SyntaxKind.UndefinedKeyword) return "never";
  if (ts.isParenthesizedTypeNode(node)) return bodyKind(node.type, aliases, stack);
  if (ts.isTypeReferenceNode(node)) {
    const name = node.typeName.getText();
    if (!aliases.has(name) || stack.has(name)) return "typed";
    return bodyKind(aliases.get(name), aliases, new Set([...stack, name]));
  }
  if (ts.isUnionTypeNode(node)) {
    const kinds = node.types.map(type => bodyKind(type, aliases, stack));
    return kinds.includes("unknown") ? "unknown" : kinds.every(kind => kind === "never") ? "never" : "typed";
  }
  return "typed";
}
export function checkConsoleContract(generatedText, sources) {
  const ast = ts.createSourceFile("generated.ts", generatedText, ts.ScriptTarget.Latest, true);
  const operations = ast.statements.find(node => ts.isInterfaceDeclaration(node) && node.name.text === "ApiOperations");
  if (!operations) throw new Error("Generated ApiOperations missing; run the native OpenAPI producer first");
  const published = operations.members.map(node => node.name.text);
  const aliases = new Map();
  function collectAliases(nodes, prefix = "") {
    for (const node of nodes) {
      if (ts.isTypeAliasDeclaration(node)) aliases.set(prefix + node.name.text, node.type);
      if (ts.isModuleDeclaration(node) && node.body && ts.isModuleBlock(node.body)) collectAliases(node.body.statements, `${prefix}${node.name.text}.`);
    }
  }
  collectAliases(ast.statements);
  const bodySchemas = new Map(operations.members.map(node => [node.name.text, bodyKind(node.type.members.find(field => field.name.getText(ast) === "body")?.type, aliases)]));
  const report = { published: published.length, checked: [], issues: [], schemaGaps: [] };
  for (const [filename, text] of sources) {
    const source = ts.createSourceFile(filename, text, ts.ScriptTarget.Latest, true);
    const result = scanSource(source, published, bodySchemas, filename);
    for (const key of ["checked", "issues", "schemaGaps"]) report[key].push(...result[key]);
  }
  return { ...report, checked: report.checked.length };
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  // Generic adapters retain the existing BFF authority. Their public callers
  // are constrained by ApiRequest<K>, and dynamic binary URLs by SDK metadata.
  const sources = children(path.join(web, "src")).filter(file => !["/api/generatedTransport.ts", "/lib/api.ts", "/lib/sse.ts"].some(suffix => file.endsWith(suffix))).map(file => [path.relative(web, file), fs.readFileSync(file, "utf8")]);
  const report = checkConsoleContract(fs.readFileSync(path.join(web, "../sdk/typescript/generated.ts"), "utf8"), sources);
  if (process.argv.includes("--json")) process.stdout.write(JSON.stringify(report, null, 2) + "\n");
  else {
    for (const issue of report.issues) process.stderr.write(`${issue.location}: undocumented ${issue.operation}\n`);
    for (const gap of report.schemaGaps) process.stderr.write(`${gap.location}: request body schema unavailable for ${gap.operation}\n`);
    process.stdout.write(`Console API contract: ${report.checked} source calls, ${report.published} generated operations, ${report.issues.length} unresolved/undocumented calls, ${report.schemaGaps.length} missing request schemas\n`);
  }
  if (report.issues.length || report.schemaGaps.length) process.exitCode = 1;
}
