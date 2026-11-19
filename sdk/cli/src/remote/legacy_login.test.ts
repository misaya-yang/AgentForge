import { describe,it,expect,vi } from "vitest";
import { runLegacyGatewayCli } from "../legacy_gateway.js";
describe("legacy API-key argument removal",()=>{
  it.each([[["--api-key","must-not-print"]],[["-k","must-not-print"]],[["--api-key=must-not-print"]]])("refuses secret arguments before parsing or persistence",async args=>{
    const spy=vi.spyOn(console,"error").mockImplementation(()=>undefined);
    try{expect(await runLegacyGatewayCli(args)).toBe(2);expect(spy.mock.calls.flat().join(" ")).not.toContain("must-not-print");expect(spy.mock.calls.flat().join(" ")).toContain("not accepted in arguments");}finally{spy.mockRestore()}
  });
});
