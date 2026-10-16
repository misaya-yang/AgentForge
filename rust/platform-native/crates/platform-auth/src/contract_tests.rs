use super::*;

fn assert_shape(value: &Value, schema: &Value) {
    if value.is_null() {
        assert!(
            schema["type"]
                .as_array()
                .is_some_and(|types| types.contains(&json!("null")))
        );
        return;
    }
    let kind = schema["type"]
        .as_str()
        .unwrap_or_else(|| schema["type"][0].as_str().unwrap());
    match kind {
        "object" => {
            let fields = value.as_object().unwrap();
            let properties = schema["properties"].as_object().unwrap();
            assert_eq!(fields.len(), properties.len());
            for (name, child) in properties {
                assert_shape(&fields[name], child);
            }
            assert_eq!(schema["required"].as_array().unwrap().len(), fields.len());
        }
        "array" => {
            for item in value.as_array().unwrap() {
                assert_shape(item, &schema["items"]);
            }
        }
        "string" => assert!(value.is_string()),
        "integer" => assert!(value.is_u64()),
        "boolean" => assert!(value.is_boolean()),
        _ => panic!("unsupported fixture schema type"),
    }
    if let Some(constant) = schema.get("const") {
        assert_eq!(value, constant);
    }
}

#[test]
fn real_request_dtos_preserve_required_strings_and_unknown_field_tolerance() {
    let login = json!({"email":"fixture","password":"fixture","ignored":42});
    let input: LoginInput = serde_json::from_value(login.clone()).unwrap();
    assert_eq!(input.email, "fixture");
    assert_eq!(input.password, "fixture");
    for name in ["email", "password"] {
        let mut missing = login.clone();
        missing.as_object_mut().unwrap().remove(name);
        assert!(serde_json::from_value::<LoginInput>(missing).is_err());
        for invalid in [Value::Null, json!(1), json!(false), json!({}), json!([])] {
            let mut body = login.clone();
            body[name] = invalid;
            assert!(serde_json::from_value::<LoginInput>(body).is_err());
        }
    }
    assert!(
        serde_json::from_str::<LoginInput>(r#"{"email":"one","email":"two","password":"fixture"}"#)
            .is_err()
    );
    // Empty strings deserialize; service validation owns their policy.
    assert!(serde_json::from_value::<LoginInput>(json!({"email":"","password":""})).is_ok());
    let password = json!({"current_password":"old","new_password":"new","confirm_password":"new","ignored":true});
    let input: PasswordInput = serde_json::from_value(password.clone()).unwrap();
    assert_eq!(
        (
            input.current_password.as_str(),
            input.new_password.as_str(),
            input.confirm_password.as_str()
        ),
        ("old", "new", "new")
    );
    for name in ["current_password", "new_password", "confirm_password"] {
        let mut missing = password.clone();
        missing.as_object_mut().unwrap().remove(name);
        assert!(serde_json::from_value::<PasswordInput>(missing).is_err());
        let mut null = password.clone();
        null[name] = Value::Null;
        assert!(serde_json::from_value::<PasswordInput>(null).is_err());
    }
    assert_eq!(login_request_schema()["additionalProperties"], true);
    assert_eq!(password_request_schema()["additionalProperties"], true);
}

#[test]
fn actual_serialized_success_types_match_bounded_schemas() {
    let login = LoginResponse {
        access_token: "synthetic-not-a-credential".into(),
        token_type: "bearer",
        expires_in: 3600,
        user: UserProfile {
            user_id: "user".into(),
            email: None,
            display_name: Some("Fixture".into()),
            department: None,
            roles: vec!["user".into()],
            permissions: vec![],
            tier: "normal".into(),
        },
        force_password_change: true,
    };
    assert_shape(
        &serde_json::to_value(login).unwrap(),
        &success_schema(LOGIN),
    );
    let me = CurrentUserResponse {
        user_id: "user".into(),
        tenant_id: "tenant".into(),
        email: None,
        display_name: None,
        department: None,
        roles: vec![],
        permissions: vec![],
        effective_permissions: vec!["read".into()],
        tier: "normal".into(),
        force_password_change: false,
    };
    assert_shape(&serde_json::to_value(me).unwrap(), &success_schema(ME));
    assert_shape(
        &serde_json::to_value(ValidationResponse {
            valid: true,
            user_id: "user".into(),
            roles: vec![],
            tier: "normal".into(),
        })
        .unwrap(),
        &success_schema(VALIDATE),
    );
    assert_shape(&logout_response(false), &success_schema(LOGOUT));
    assert_shape(&logout_response(true), &success_schema(LOGOUT));
    assert_shape(
        &password_changed_response(),
        &success_schema(CHANGE_PASSWORD),
    );
    assert_eq!(logout_response(false)["message"], "Not logged in");
    assert_eq!(logout_response(true)["message"], "Logged out successfully");
    assert_eq!(
        password_changed_response()["message"],
        "Password changed successfully"
    );
}

#[test]
fn route_inventory_exports_exact_public_optional_and_bearer_modes() {
    let mut doc = json!({"paths":{},"components":{"schemas":{}}});
    extend_document(&mut doc);
    assert_eq!(doc["paths"].as_object().unwrap().len(), 6);
    for (method, path, id) in ROUTES {
        let operation = &doc["paths"][path][method.to_ascii_lowercase()];
        assert_eq!(operation["operationId"], *id);
        assert_eq!(
            operation["responses"]["default"]["x-native-undeclared"],
            true
        );
        assert_eq!(
            operation["responses"]["200"]["content"]["application/json"]["schema"],
            success_schema(path)
        );
    }
    assert_eq!(doc["paths"][LOGIN]["post"]["security"], json!([]));
    assert_eq!(doc["paths"][JWKS]["get"]["security"], json!([]));
    assert!(doc["paths"][JWKS]["get"]["responses"].get("401").is_none());
    assert!(doc["paths"][JWKS]["get"]["responses"].get("503").is_some());
    assert_eq!(
        doc["paths"][LOGOUT]["post"]["security"],
        json!([{}, {"NativeBearerAuth":[]} ])
    );
    for (method, path) in [("get", ME), ("post", VALIDATE), ("post", CHANGE_PASSWORD)] {
        assert_eq!(
            doc["paths"][path][method]["security"],
            json!([{"NativeBearerAuth":[]}])
        );
    }
    for path in [LOGIN, CHANGE_PASSWORD] {
        assert_eq!(
            doc["paths"][path]["post"]["x-native-body-limit-bytes"],
            BODY_LIMIT
        );
        assert!(
            doc["paths"][path]["post"]["requestBody"]["required"]
                .as_bool()
                .unwrap()
        );
    }
    assert!(doc["paths"][LOGOUT]["post"].get("requestBody").is_none());
    assert!(doc["paths"][VALIDATE]["post"].get("requestBody").is_none());
    assert!(doc["paths"][JWKS]["get"].get("requestBody").is_none());
}

#[test]
fn login_lock_statuses_are_not_advertised_for_session_authentication() {
    let mut doc = json!({"paths":{},"components":{"schemas":{}}});
    extend_document(&mut doc);
    for status in ["403", "423"] {
        assert!(
            doc["paths"][LOGIN]["post"]["responses"]
                .get(status)
                .is_some()
        );
        for (method, path) in [
            ("post", LOGOUT),
            ("get", ME),
            ("post", VALIDATE),
            ("post", CHANGE_PASSWORD),
        ] {
            assert!(
                doc["paths"][path][method]["responses"]
                    .get(status)
                    .is_none()
            );
        }
    }
    assert!(
        doc["paths"][LOGOUT]["post"]["x-native-authorization"]
            .as_str()
            .unwrap()
            .contains("does not require an existing allowlist")
    );
}
