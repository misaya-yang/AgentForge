//! Additive native review routes; unsupported hosts fail closed, never fall back.
use super::*;
use crate::model_catalog_review::{ApplySelection, ReviewError};
pub(super) fn routes() -> Router<HttpState> {
    Router::new()
        .route(
            crate::native_contract::CATALOG_PREVIEW,
            any(http_method_fallback).post(preview),
        )
        .route(
            crate::native_contract::CATALOG_APPLY,
            any(http_method_fallback).post(apply),
        )
        .route(
            crate::native_contract::CATALOG_READ,
            any(http_method_fallback).get(read),
        )
}
async fn action(
    state: HttpState,
    provider: String,
    id: Option<String>,
    headers: HeaderMap,
    body: Option<ApplySelection>,
) -> Response {
    let result = async {
        let path = format!("/api/v1/providers/{provider}/models/sync");
        let user = state.identity.resolve(&headers, &path).await?;
        require(&user, Capability::GatewayProviderConfigWrite)?;
        if provider.contains('/') || provider.is_empty() {
            return Err(ProviderAdminError::http(404, "Not Found"));
        }
        let review = state
            .api
            .review
            .as_ref()
            .ok_or_else(|| ProviderAdminError::http(503, ReviewError::Unavailable.message()))?;
        let applying = body.is_some();
        let value = match (id, body) {
            (Some(id), _) => {
                review
                    .read(user.tenant(), user.user_id(), &provider, &id)
                    .await
            }
            (_, Some(selection)) => {
                review
                    .apply(user.tenant(), user.user_id(), &provider, selection)
                    .await
            }
            _ => {
                review
                    .preview(user.tenant(), user.user_id(), &provider)
                    .await
            }
        }
        .map_err(|e| ProviderAdminError::http(e.status(), e.message()))?;
        if applying && value["state"] == "applied" {
            let context = ProviderAdminRequestContext {
                headers: headers.clone(),
                ..Default::default()
            };
            state.api.effects.invalidate(user.tenant(), &provider);
            if let Some(models) = value["result"]["models"].as_array() {
                for model in models {
                    if let Some(id) = model["model_id"].as_str() {
                        state
                            .api
                            .effects
                            .publish(&context, user.tenant(), id, &provider)
                            .await
                            .map_err(backend_error)?;
                    }
                }
            }
            state
                .api
                .effects
                .audit(
                    &context,
                    &user,
                    ProviderAdminChange {
                        resource_id: provider.clone(),
                        action: "catalog_review_apply",
                        before: None,
                        after: Some(value.clone()),
                    },
                )
                .await
                .map_err(backend_error)?;
        }
        Ok::<_, ProviderAdminError>(value)
    }
    .await;
    let mut response = match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => {
            let status = StatusCode::from_u16(error.status().unwrap_or(500))
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (
                status,
                Json(
                    error
                        .response_body(None)
                        .unwrap_or(json!({"detail":"Cancelled"})),
                ),
            )
                .into_response()
        }
    };
    response.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}
async fn preview(
    State(state): State<HttpState>,
    Path(provider): Path<String>,
    headers: HeaderMap,
) -> Response {
    action(state, provider, None, headers, None).await
}
async fn apply(
    State(state): State<HttpState>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ApplySelection>,
) -> Response {
    action(state, provider, None, headers, Some(body)).await
}
async fn read(
    State(state): State<HttpState>,
    Path((provider, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    action(state, provider, Some(id), headers, None).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Identity {
        roles: Vec<String>,
    }
    impl ProviderAdminIdentityResolver for Identity {
        fn resolve<'a>(&'a self, _: &'a HeaderMap, _: &'a str) -> ProviderAdminAuthFuture<'a> {
            Box::pin(async {
                Ok(ProviderAdminUser::from_resolved_identity(
                    "actor".into(),
                    "tenant".into(),
                    self.roles.clone(),
                    true,
                ))
            })
        }
    }
    struct Effects {
        audits: AtomicUsize,
        publications: AtomicUsize,
        fail: bool,
    }
    impl ProviderAdminEffects for Effects {
        fn invalidate(&self, _: &str, _: &str) {}
        fn begin_probe(&self, _: &str, _: &str) -> Option<String> {
            None
        }
        fn record_probe(&self, _: &str, _: &str, _: &Value, _: Option<&str>) {}
        fn audit<'a>(
            &'a self,
            _: &'a ProviderAdminRequestContext,
            _: &'a ProviderAdminUser,
            _: ProviderAdminChange,
        ) -> ProviderAdminFuture<'a, ()> {
            Box::pin(async move {
                self.audits.fetch_add(1, Ordering::SeqCst);
                if self.fail {
                    Err(ProviderAdminBackendError::exception("fixture"))
                } else {
                    Ok(())
                }
            })
        }
        fn publish<'a>(
            &'a self,
            _: &'a ProviderAdminRequestContext,
            _: &'a str,
            _: &'a str,
            _: &'a str,
        ) -> ProviderAdminFuture<'a, ()> {
            Box::pin(async move {
                self.publications.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }
    }
    struct Cancellation;
    impl ProviderAdminCancellation for Cancellation {
        fn cancelled(&self) -> Pin<Box<dyn Future<Output = Response> + Send + '_>> {
            Box::pin(async { StatusCode::INTERNAL_SERVER_ERROR.into_response() })
        }
    }
    fn state(
        admin: bool,
        review: Option<Arc<crate::model_catalog_review::CatalogReview>>,
        effects: Arc<Effects>,
    ) -> HttpState {
        let mut api = ProviderAdminApi::new(None, None, effects);
        if let Some(r) = review {
            api = api.with_review(r);
        }
        HttpState {
            api: Arc::new(api),
            identity: Arc::new(Identity {
                roles: if admin { vec!["admin".into()] } else { vec![] },
            }),
            cancellation: Arc::new(Cancellation),
        }
    }
    fn effects(fail: bool) -> Arc<Effects> {
        Arc::new(Effects {
            audits: AtomicUsize::new(0),
            publications: AtomicUsize::new(0),
            fail,
        })
    }
    #[tokio::test]
    async fn authority_precedes_preview_and_missing_adapter_never_falls_back() {
        let e = effects(false);
        assert_eq!(
            action(
                state(false, None, e.clone()),
                "dashscope".into(),
                None,
                HeaderMap::new(),
                None
            )
            .await
            .status(),
            403
        );
        assert_eq!(
            action(
                state(true, None, e.clone()),
                "dashscope".into(),
                None,
                HeaderMap::new(),
                None
            )
            .await
            .status(),
            503
        );
        assert_eq!(e.audits.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn postcommit_audit_failure_is_recoverable_without_reapplying() {
        let (review, _) = crate::model_catalog_review::tests::setup();
        let review = Arc::new(review);
        let e = effects(true);
        let state = state(true, Some(review.clone()), e.clone());
        let response = action(
            state.clone(),
            "dashscope".into(),
            None,
            HeaderMap::new(),
            None,
        )
        .await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
        let preview: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 1000000)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(e.audits.load(Ordering::SeqCst), 0);
        let selection = ApplySelection {
            review_id: preview["review_id"].as_str().unwrap().into(),
            model_ids: vec![preview["changes"][0]["model_id"].as_str().unwrap().into()],
            replace_existing_metadata: false,
        };
        assert_eq!(
            action(
                state.clone(),
                "dashscope".into(),
                None,
                HeaderMap::new(),
                Some(selection.clone())
            )
            .await
            .status(),
            500
        );
        assert_eq!(e.publications.load(Ordering::SeqCst), 1);
        assert_eq!(e.audits.load(Ordering::SeqCst), 1);
        let read = review
            .read("tenant", "actor", "dashscope", &selection.review_id)
            .await
            .unwrap();
        assert_eq!(read["state"], "applied");
        assert_eq!(
            action(
                state,
                "dashscope".into(),
                None,
                HeaderMap::new(),
                Some(selection)
            )
            .await
            .status(),
            503
        );
        assert_eq!(e.audits.load(Ordering::SeqCst), 1);
    }
}
