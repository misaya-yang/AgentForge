-- UNNUMBERED PROPOSAL ONLY. Do not apply directly or alter frozen migrations.
-- Optional successor to the deployed Local Node request-PoP schema. Activation
-- must atomically add every object below; partial activation fails closed.
-- Client hardware names are CLAIMS. Only a separately enrolled verifier which
-- validates genuine TPM2 Certify evidence may issue the signed assertion stored
-- here by the database authority. No Gateway/client enrollment endpoint exists.
-- Retain the native evidence for independent verifier audit; a self-reported
-- key class, screenshot, software signature, or device certificate alone is not
-- key-specific hardware attestation. macOS SE remains unverified in this version.
-- Gateway additionally parses TPMS_ATTEST/TPMT_PUBLIC, verifies the exact key
-- name, hardware-generated/fixed attributes, nonce, and P-256 AK signature.
-- Verifier enrollment must root that restricted AK in genuine platform trust;
-- retaining arbitrary bytes plus signing an assertion is not sufficient.

ALTER TABLE gateway.local_node_request_keys
    ADD COLUMN algorithm text NOT NULL DEFAULT 'ed25519';
ALTER TABLE gateway.local_node_request_keys
    DROP CONSTRAINT local_node_request_keys_public_key_check,
    DROP CONSTRAINT local_node_request_keys_key_class_check;
ALTER TABLE gateway.local_node_request_keys
    ADD CONSTRAINT local_node_request_keys_algorithm_class_format_check CHECK (
      (algorithm='ed25519' AND key_class='os-vault-software-ed25519' AND public_key ~ '^[A-Za-z0-9_-]{43}$') OR
      (algorithm='ES256' AND key_class IN ('macos-secure-enclave-p256','windows-tpm-cng-p256','linux-tpm2-p256') AND public_key ~ '^[A-Za-z0-9_-]{87}$')
    );

COMMENT ON COLUMN gateway.local_node_request_keys.key_class IS 'Signed client-reported class only. Never a hardware verification flag; assurance is re-evaluated against tenant policy and authority evidence.';

CREATE TABLE gateway.local_node_identity_policy (
    tenant_id varchar(128) PRIMARY KEY,
    allow_software boolean NOT NULL DEFAULT true,
    allow_unverified_p256 boolean NOT NULL DEFAULT true,
    require_verified_hardware boolean NOT NULL DEFAULT false,
    allowed_hardware_classes text[] NOT NULL DEFAULT ARRAY['macos-secure-enclave-p256','windows-tpm-cng-p256','linux-tpm2-p256'],
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (allowed_hardware_classes <@ ARRAY['macos-secure-enclave-p256','windows-tpm-cng-p256','linux-tpm2-p256']::text[])
);
CREATE TABLE gateway.local_node_attestation_verifiers (
    tenant_id varchar(128) NOT NULL,
    verifier_id varchar(128) NOT NULL,
    signing_public_key varchar(43) NOT NULL CHECK(signing_public_key ~ '^[A-Za-z0-9_-]{43}$'),
    allowed_hardware_classes text[] NOT NULL,
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(tenant_id,verifier_id),
    CHECK(cardinality(allowed_hardware_classes)>0 AND allowed_hardware_classes <@ ARRAY['windows-tpm-cng-p256','linux-tpm2-p256']::text[])
);
CREATE TABLE gateway.local_node_hardware_attestations (
    attestation_id uuid PRIMARY KEY,
    tenant_id varchar(128) NOT NULL,
    user_id varchar(128) NOT NULL,
    device_id varchar(255) NOT NULL,
    public_key_sha256 char(64) NOT NULL CHECK(public_key_sha256 ~ '^[0-9a-f]{64}$'),
    hardware_class text NOT NULL CHECK(hardware_class IN ('windows-tpm-cng-p256','linux-tpm2-p256')),
    verifier_id varchar(128) NOT NULL,
    assertion jsonb NOT NULL CHECK(jsonb_typeof(assertion)='object' AND octet_length(assertion::text)<=16384),
    signature varchar(86) NOT NULL CHECK(signature ~ '^[A-Za-z0-9_-]{86}$'),
    platform_evidence bytea NOT NULL CHECK(octet_length(platform_evidence) BETWEEN 16 AND 65536),
    revoked_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY(tenant_id,verifier_id) REFERENCES gateway.local_node_attestation_verifiers(tenant_id,verifier_id) ON DELETE RESTRICT,
    CHECK(assertion->>'attestation_id'=attestation_id::text AND assertion->>'tenant_id'=tenant_id AND assertion->>'user_id'=user_id AND assertion->>'device_id'=device_id AND assertion->>'public_key_sha256'=public_key_sha256 AND assertion->>'hardware_class'=hardware_class AND assertion->>'verifier_id'=verifier_id)
);
CREATE INDEX local_node_hardware_attestations_identity ON gateway.local_node_hardware_attestations(tenant_id,user_id,device_id,public_key_sha256,hardware_class,created_at DESC) WHERE revoked_at IS NULL;

REVOKE ALL ON gateway.local_node_identity_policy,gateway.local_node_attestation_verifiers,gateway.local_node_hardware_attestations FROM PUBLIC;
DO $grant$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner'));
BEGIN
 IF prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN RAISE EXCEPTION 'invalid authority role prefix'; END IF;
 -- Gateway only READS policy/trust/evidence; it cannot attest its own clients,
 -- enroll a verifier, weaken tenant policy or revoke/rewrite audit evidence.
 EXECUTE format('REVOKE ALL ON gateway.local_node_identity_policy,gateway.local_node_attestation_verifiers,gateway.local_node_hardware_attestations FROM %I',prefix||'gateway');
 EXECUTE format('GRANT SELECT ON gateway.local_node_identity_policy,gateway.local_node_attestation_verifiers,gateway.local_node_hardware_attestations TO %I',prefix||'gateway');
END $grant$;
