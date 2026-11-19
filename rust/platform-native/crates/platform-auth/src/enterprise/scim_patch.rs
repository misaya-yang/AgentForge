//! Bounded SCIM PatchOp support; unsupported paths never silently disappear.
// SPDX-License-Identifier: Apache-2.0
use super::*;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub schemas: Vec<String>,
    #[serde(rename = "Operations")]
    pub operations: Vec<Operation>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub op: String,
    pub path: Option<String>,
    pub value: Option<Value>,
}
fn validate(p: &Patch) -> Result<()> {
    if p.schemas != ["urn:ietf:params:scim:api:messages:2.0:PatchOp"]
        || p.operations.is_empty()
        || p.operations.len() > 32
    {
        return Err(EnterpriseError::Invalid);
    }
    Ok(())
}
fn apply_user(resource: &mut Value, patch: Patch) -> Result<()> {
    validate(&patch)?;
    for op in patch.operations {
        let verb = op.op.to_ascii_lowercase();
        if !matches!(verb.as_str(), "add" | "replace" | "remove") {
            return Err(EnterpriseError::Invalid);
        }
        if let Some(path) = op.path {
            if !matches!(
                path.as_str(),
                "active" | "userName" | "displayName" | "emails" | "externalId"
            ) {
                return Err(EnterpriseError::Invalid);
            }
            let value = if verb == "remove" {
                match path.as_str() {
                    "displayName" => Value::Null,
                    "emails" => json!([]),
                    _ => return Err(EnterpriseError::Invalid),
                }
            } else {
                op.value.ok_or(EnterpriseError::Invalid)?
            };
            resource[&path] = value;
        } else {
            if verb == "remove" {
                return Err(EnterpriseError::Invalid);
            }
            let values = op.value.ok_or(EnterpriseError::Invalid)?;
            let values = values.as_object().ok_or(EnterpriseError::Invalid)?;
            for (key, value) in values {
                if !matches!(
                    key.as_str(),
                    "active" | "userName" | "displayName" | "emails" | "externalId"
                ) {
                    return Err(EnterpriseError::Invalid);
                }
                resource[key] = value.clone();
            }
        }
    }
    Ok(())
}
fn strip(resource: &mut Value) {
    for field in ["id", "meta"] {
        resource.as_object_mut().unwrap().remove(field);
    }
}
impl EnterpriseService {
    pub(in crate::enterprise) async fn scim_patch_user(
        &self,
        a: &ScimActor,
        id: &str,
        patch: Patch,
        expected: Option<&str>,
    ) -> Result<Value> {
        a.require("scim.users.write")?;
        let mut current = self.scim_user(a, id).await?;
        let version = expected
            .map(str::to_owned)
            .unwrap_or_else(|| current["meta"]["version"].as_str().unwrap_or("").to_owned());
        apply_user(&mut current, patch)?;
        strip(&mut current);
        let input = serde_json::from_value(current).map_err(|_| EnterpriseError::Invalid)?;
        self.scim_replace_user(a, id, input, &version).await
    }
    pub(in crate::enterprise) async fn scim_patch_group(
        &self,
        a: &ScimActor,
        id: &str,
        patch: Patch,
        expected: Option<&str>,
    ) -> Result<Value> {
        a.require("scim.groups.write")?;
        validate(&patch)?;
        let mut current = self.scim_group(a, id).await?;
        let version = expected
            .map(str::to_owned)
            .unwrap_or_else(|| current["meta"]["version"].as_str().unwrap_or("").to_owned());
        for op in patch.operations {
            let verb = op.op.to_ascii_lowercase();
            if !matches!(verb.as_str(), "add" | "replace" | "remove") {
                return Err(EnterpriseError::Invalid);
            }
            let path = op.path.ok_or(EnterpriseError::Invalid)?;
            if path == "members" {
                let members = current["members"]
                    .as_array_mut()
                    .ok_or(EnterpriseError::Invalid)?;
                if verb == "remove" && op.value.is_none() {
                    members.clear();
                    continue;
                }
                let values = op.value.ok_or(EnterpriseError::Invalid)?;
                let values = values.as_array().ok_or(EnterpriseError::Invalid)?;
                if verb == "replace" {
                    *members = values.clone();
                } else if verb == "add" {
                    for member in values {
                        if !members.iter().any(|old| old["value"] == member["value"]) {
                            members.push(member.clone());
                        }
                    }
                } else {
                    members
                        .retain(|old| !values.iter().any(|value| old["value"] == value["value"]));
                }
            } else if let Some(user) = path
                .strip_prefix("members[value eq \"")
                .and_then(|s| s.strip_suffix("\"]"))
            {
                if verb != "remove" || user.contains(['"', '\\']) {
                    return Err(EnterpriseError::Invalid);
                }
                current["members"]
                    .as_array_mut()
                    .ok_or(EnterpriseError::Invalid)?
                    .retain(|m| m["value"] != user);
            } else if matches!(path.as_str(), "displayName" | "externalId") && verb != "remove" {
                current[path] = op.value.ok_or(EnterpriseError::Invalid)?;
            } else {
                return Err(EnterpriseError::Invalid);
            }
        }
        strip(&mut current);
        let input = serde_json::from_value(current).map_err(|_| EnterpriseError::Invalid)?;
        self.scim_write_group(a, Some(id), input, Some(&version))
            .await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn patch_deactivation_is_explicit_and_authorization_fields_are_rejected() {
        let mut resource = json!({"active":true});
        let p:Patch=serde_json::from_value(json!({"schemas":["urn:ietf:params:scim:api:messages:2.0:PatchOp"],"Operations":[{"op":"Replace","path":"active","value":false}]})).unwrap();
        apply_user(&mut resource, p).unwrap();
        assert_eq!(resource["active"], false);
        let p:Patch=serde_json::from_value(json!({"schemas":["urn:ietf:params:scim:api:messages:2.0:PatchOp"],"Operations":[{"op":"add","path":"roles","value":["admin"]}]})).unwrap();
        assert!(apply_user(&mut resource, p).is_err());
    }
}
