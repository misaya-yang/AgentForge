//! Real installed-wheel/tenant filesystem acceptance, explicitly not run by
//! default. Execute inside the exact Python 3.14 sandbox image after delegation.
use super::*;
use crate::python_package_policy::TenantPackagePolicy;

#[test]
#[ignore = "requires the exact Linux CPython3.14 curated-wheel image and delegated cgroup subtree"]
fn allowed_tenant_science_outputs_work_and_denied_tenant_cannot_read_packages() {
    let catalog = fs::read("/opt/agentforge/python-packages/catalog.json").unwrap();
    let root = std::env::temp_dir().join(format!(
        "agentforge-package-acceptance-{}",
        uuid::Uuid::now_v7()
    ));
    fs::create_dir(&root).unwrap();
    let file = root.join("policy.json");
    fs::write(&file, serde_json::to_vec(&json!({
        "schema_version":"python-tenant-package-policy/v1", "catalog_sha256":format!("sha256:{}",hex::encode(Sha256::digest(&catalog))),
        "tenants":{"allowed":["numpy","pandas","scipy","matplotlib","pillow","openpyxl"]}
    })).unwrap()).unwrap();
    let mut config = LocalPythonSandboxConfig::default();
    if let Some(path) = std::env::var_os("AGENTFORGE_SANDBOX_TEST_CGROUP_ROOT") {
        config.cgroup_root = path.into();
    }
    let policy = TenantPackagePolicy::load(&file, &config.python_binary).unwrap();
    config.package_roots = policy.paths_for("allowed").unwrap();
    let allowed = r#"
import numpy as np
import pandas as pd
import scipy.linalg
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
from PIL import Image
import openpyxl
assert int(np.array([1,2]).sum()) == 3
assert int(pd.DataFrame({'x':[1,2]}).x.sum()) == 3
assert int(round(scipy.linalg.det(np.eye(2)))) == 1
plt.plot([1,2],[3,4]); plt.savefig('output/plot.png'); plt.close()
Image.new('RGB',(8,8),'red').save('output/image.png')
book=openpyxl.Workbook(); book.active['A1']=42; book.save('output/data.xlsx')
print('curated-stack-ok')
"#;
    let result = supervisor::run_isolated(
        &config,
        &request(allowed, "allowed"),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(result.status, "succeeded", "{}", result.stderr);
    assert!(result.stdout.contains("curated-stack-ok"));
    assert_eq!(result.output_files.len(), 3);
    config.package_roots = policy.paths_for("denied").unwrap();
    assert!(config.package_roots.is_empty());
    let denied = r#"
import sys
sys.path.insert(0,'/opt/agentforge/python-packages/numpy')
try:
    open('/opt/agentforge/python-packages/numpy/numpy/__init__.py').read()
except OSError:
    pass
else:
    raise AssertionError('other-tenant package files readable')
try:
    import numpy
except (ImportError,OSError):
    print('tenant-package-denied')
else:
    raise AssertionError('other-tenant package import allowed')
"#;
    let result =
        supervisor::run_isolated(&config, &request(denied, "denied"), &AtomicBool::new(false))
            .unwrap();
    assert_eq!(result.status, "succeeded", "{}", result.stderr);
    assert!(result.stdout.contains("tenant-package-denied"));
    fs::remove_dir_all(root).unwrap();
}
fn request(code: &str, tenant: &str) -> PythonCodeExecutionRequest {
    let hash = ai_platform_capability_contract::canonical_json_hash(&json!({"code":code})).unwrap();
    PythonCodeExecutionRequest {
        lease: RuntimeCapabilityLeaseV1 {
            schema_version:
                ai_platform_capability_contract::RUNTIME_CAPABILITY_LEASE_SCHEMA_VERSION.into(),
            lease_id: uuid::Uuid::now_v7().to_string(),
            tenant_id: tenant.into(),
            user_id: "test".into(),
            session_id: "test".into(),
            run_id: uuid::Uuid::now_v7().to_string(),
            tool_call_id: "test".into(),
            attempt_id: "test".into(),
            capability_id: "execute_python_code".into(),
            capability_revision: 1,
            arguments_hash: hash.clone(),
            effect: ai_platform_capability_contract::CapabilityEffect::Write,
            approval_id: None,
            issued_at_epoch_ms: 1,
            expires_at_epoch_ms: 120001,
            nonce: "test-nonce".into(),
            signature: String::new(),
        },
        arguments_hash: hash,
        code: code.into(),
        inputs: vec![],
        limits: PythonSandboxLimits {
            timeout_seconds: 30,
            cpu_millis: 30000,
            ..Default::default()
        },
    }
}
