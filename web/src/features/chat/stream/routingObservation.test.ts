import assert from "node:assert/strict";
import test from "node:test";
import { readReportedRouting } from "./routingObservation.ts";
const selected = { status: "selected", provider_id: "vendor-a", model_id: "vendor/model-v2", reason: "timeout" };
test("selection retains only bounded reported identity and known reason", () => {
 assert.deepEqual(readReportedRouting({...selected, api_key: "secret", base_url: "https://private.test/?token=secret", detail: { credential: "secret" }}), {...selected, source: "upstream_reported"});
 assert.equal(readReportedRouting({...selected, reason: "https://private.test/?token=secret"})?.reason, undefined);
});
test("invalid or unknown identities cannot be rendered or persisted", () => {
 for (const value of [null, [], {}, {...selected,status:"exhausted"}, {...selected,model_id:"https://private.test"}, {...selected,provider_id:"sk-secret"}, {...selected,model_id:"a".repeat(129)}, {...selected,model_id:"model?api_key=secret"}, {...selected,provider_id:"a\nb"}]) assert.equal(readReportedRouting(value), undefined);
});
test("historic unknown data stays unknown and parser sanitizes restoration", () => {
 assert.equal(readReportedRouting(undefined), undefined);
 assert.deepEqual(readReportedRouting({...selected,source:"verified_execution",cost:1}), {...selected,source:"upstream_reported"});
});

test("recognized credential-shaped identifiers are excluded even after a vendor prefix", () => {
 for (const model_id of ["vendor/sk-fakefixture", "ghp_FAKEFIXTURE12345", "vendor/github_pat_FAKE", "eyJhbGci.eyJzdWI.signature", "vendor/xoxb-FAKE", "AKIAFAKEFIXTURE"]) assert.equal(readReportedRouting({...selected,model_id}),undefined);
});
