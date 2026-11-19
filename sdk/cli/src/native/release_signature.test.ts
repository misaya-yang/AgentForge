import { generateKeyPairSync,sign } from "node:crypto";
import { describe,it,expect } from "vitest";
import { canonical,verifyReleaseSignature } from "./release_signature.mjs";
describe("pinned native release signatures",()=>{
  it("accepts canonical signed receipts and refuses changed target/hash/version or another key",()=>{
    const pair=generateKeyPairSync("ed25519");const other=generateKeyPairSync("ed25519");
    const pins={release:pair.publicKey.export({format:"pem",type:"spki"}).toString()};
    const payload={binary:"codex",sha256:"a".repeat(64),release_version:"1.5.0",target_system:"linux",materials:{NOTICE:"b".repeat(64)}};
    const receipt={...payload,signature:{algorithm:"Ed25519",key_id:"release",value:sign(null,Buffer.from(canonical(payload)),pair.privateKey).toString("base64url")}};
    expect(()=>verifyReleaseSignature(receipt,pins)).not.toThrow();
    for(const changed of [{...receipt,sha256:"c".repeat(64)},{...receipt,target_system:"darwin"},{...receipt,release_version:"older"},{...receipt,signature:{...receipt.signature,value:sign(null,Buffer.from(canonical(payload)),other.privateKey).toString("base64url")}}])expect(()=>verifyReleaseSignature(changed,pins)).toThrow();
    expect(()=>verifyReleaseSignature(receipt,{})).toThrow(/pinned key/);
    expect(()=>verifyReleaseSignature(payload,pins)).toThrow(/signature/);
  });
});
