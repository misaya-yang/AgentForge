use super::*;

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    fn setup() -> (Arc<MemoryTaskStorage>, Arc<TaskManager>) {
        let storage = Arc::new(MemoryTaskStorage::new());
        let manager = Arc::new(TaskManager::new(
            storage.clone(),
            Arc::new(MemoryTaskQueue::new()),
        ));
        (storage, manager)
    }
    async fn create(manager: &TaskManager) -> Task {
        manager
            .create_task(
                UnifiedRequest::new("request", "service", vec![]),
                vec![],
                None,
            )
            .await
            .unwrap()
    }
    #[test]
    fn terminal_states_have_no_outgoing_transition() {
        for from in [
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Cancelled,
        ] {
            for to in [
                TaskStatus::Pending,
                TaskStatus::Processing,
                TaskStatus::Completed,
                TaskStatus::Failed,
                TaskStatus::Cancelled,
            ] {
                assert!(!from.can_transition_to(to));
            }
        }
        assert!(!TaskStatus::Pending.can_transition_to(TaskStatus::Completed));
        assert!(!TaskStatus::Processing.can_transition_to(TaskStatus::Pending));
    }
    #[tokio::test]
    async fn only_one_claim_wins() {
        let (_, manager) = setup();
        let task = create(&manager).await;
        let (a, b) = tokio::join!(
            manager.transition_status(&task.task_id, TaskStatus::Processing, None, None),
            manager.transition_status(&task.task_id, TaskStatus::Processing, None, None),
        );
        assert_ne!(a.unwrap(), b.unwrap());
        assert!(
            manager
                .get_task(&task.task_id)
                .await
                .unwrap()
                .started_at
                .is_some()
        );
    }
    #[tokio::test]
    async fn stale_completion_cannot_overwrite_cancellation() {
        let (storage, manager) = setup();
        let task = create(&manager).await;
        assert!(
            manager
                .transition_status(&task.task_id, TaskStatus::Processing, None, None)
                .await
                .unwrap()
        );
        let mut stale = storage.get(&task.task_id).await.unwrap();
        stale.status = TaskStatus::Completed;
        stale.result = Some(serde_json::json!({"result": "late"}).into());
        manager.cancel(&task.task_id).await.unwrap();
        assert!(
            !storage
                .compare_status(&stale, TaskStatus::Processing)
                .await
                .unwrap()
        );
        let cancelled = storage.get(&task.task_id).await.unwrap();
        assert_eq!(cancelled.status, TaskStatus::Cancelled);
        assert_eq!(cancelled.result, None);
        assert!(cancelled.completed_at.is_some());
        manager.cancel(&task.task_id).await.unwrap();
    }
    #[tokio::test]
    async fn completion_wins_cancel_and_failed_fallback_cannot_replace_it() {
        let (_, manager) = setup();
        let task = create(&manager).await;
        manager
            .update_status(&task.task_id, TaskStatus::Processing, None, None)
            .await
            .unwrap();
        manager
            .update_status(
                &task.task_id,
                TaskStatus::Completed,
                Some(serde_json::json!({"answer": 42}).into()),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            manager.cancel(&task.task_id).await,
            Err(TaskError::StateConflict)
        );
        assert!(
            !manager
                .transition_status(
                    &task.task_id,
                    TaskStatus::Failed,
                    None,
                    Some("uncertain".into())
                )
                .await
                .unwrap()
        );
        assert_eq!(
            manager.get_task(&task.task_id).await.unwrap().status,
            TaskStatus::Completed
        );
    }
    #[tokio::test]
    async fn compare_rejects_identity_change_and_missing_rows() {
        let (storage, manager) = setup();
        let mut task = create(&manager).await;
        task.status = TaskStatus::Processing;
        task.tenant_id = "different".into();
        assert!(
            !storage
                .compare_status(&task, TaskStatus::Pending)
                .await
                .unwrap()
        );
        task.task_id = "missing".into();
        assert!(
            !storage
                .compare_status(&task, TaskStatus::Pending)
                .await
                .unwrap()
        );
    }
}

#[cfg(test)]
mod typed_callback_tests {
    use super::*;
    use crate::protocol::ContentItem;
    #[test]
    fn typed_callback_uses_canonical_binary_wire_projection() {
        let mut output = ContentItem::text("binary");
        output.data = Some(ContentData::Bytes(vec![0,255,128]));
        let response = UnifiedResponse::new("request", "completed", vec![output]);
        let payload = CallbackPayload {
            task_id: "task".into(), status: TaskStatus::Completed,
            result: Some(response.into()), error: None,
        };
        assert_eq!(payload.to_json().unwrap()["result"]["outputs"][0]["data"], "AP+A");
    }
}

#[cfg(test)]
mod admission_binding_tests {
    use super::*;
    #[tokio::test]
    async fn each_queue_identity_mismatch_is_rejected_before_claim() {
        let storage = Arc::new(MemoryTaskStorage::new());
        let queue = Arc::new(MemoryTaskQueue::new());
        let manager = TaskManager::new(storage.clone(), queue.clone());
        let mut request = UnifiedRequest::new("request", "service", vec![]);
        request.user_id = "user".into(); request.tenant_id = "tenant".into();
        let task = manager.create_task(request, vec![], None).await.unwrap();
        let original = queue.dequeue().await.unwrap();
        for field in ["request", "service", "user", "tenant"] {
            let mut envelope = original.clone();
            match field {
                "request" => envelope.request.request_id = "wrong".into(),
                "service" => envelope.request.service_id = "wrong".into(),
                "user" => envelope.request.user_id = "wrong".into(),
                "tenant" => envelope.request.tenant_id = "wrong".into(),
                _ => unreachable!(),
            }
            assert_eq!(manager.claim_task(&envelope).await, Err(TaskError::InvalidEnvelope));
            assert_eq!(storage.get(&task.task_id).await.unwrap().status, TaskStatus::Pending);
        }
        assert!(manager.claim_task(&original).await.unwrap());
    }
    #[tokio::test]
    async fn user_request_parameters_do_not_populate_private_task_metadata() {
        let storage = Arc::new(MemoryTaskStorage::new());
        let manager = TaskManager::new(storage.clone(), Arc::new(MemoryTaskQueue::new()));
        let mut request = UnifiedRequest::new("request", "service", vec![]);
        let poisoned = json!({"gateway.native_task_result.v1":{"output_data_types":[]}}).as_object().unwrap().clone();
        request.parameters = Some(poisoned.clone());
        request.context = Some(poisoned);
        let task = manager.create_task(request, vec![], None).await.unwrap();
        assert_eq!(storage.get(&task.task_id).await.unwrap().metadata, Some(Map::new()));
    }
}

#[cfg(test)]
mod memory_list_page_tests {
    use super::*;

    fn row(id: &str, timestamp: &str, user: &str, tenant: &str) -> Task {
        let mut task = Task::new(id.into(), &UnifiedRequest::new("request", "service", vec![]),
            ProtocolTimestamp::parse_isoformat(timestamp).unwrap().naive_utc());
        task.created_at = ProtocolTimestamp::parse_isoformat(timestamp).unwrap();
        task.user_id = user.into();
        task.tenant_id = tenant.into();
        task.result = Some(json!({"large": "x".repeat(16384), "nested": [null, false, 0]}).into());
        task.request_data = Some(serde_json::from_value(json!({"input": [1, 2, 3]})).unwrap());
        task.metadata = Some(serde_json::from_value(json!({"private": {"retain": true}})).unwrap());
        task
    }

    #[tokio::test]
    async fn list_keeps_complete_detached_page_and_all_filters() {
        let storage = MemoryTaskStorage::new();
        let mut originals = Vec::new();
        for id in ["a", "b", "c", "d"] {
            let task = row(id, "2026-10-12T12:00:00Z", "owner", "tenant");
            storage.save(&task).await.unwrap();
            originals.push(task);
        }
        storage.save(&row("foreign-user", "2026-10-13T00:00:00Z", "other", "tenant")).await.unwrap();
        storage.save(&row("foreign-tenant", "2026-10-13T00:00:00Z", "owner", "other")).await.unwrap();
        let mut other_service = row("other-service", "2026-10-13T00:00:00Z", "owner", "tenant");
        other_service.service_id = "other".into();
        storage.save(&other_service).await.unwrap();
        let mut completed = row("completed", "2026-10-13T00:00:00Z", "owner", "tenant");
        completed.status = TaskStatus::Completed;
        storage.save(&completed).await.unwrap();
        let mut filter = TaskListFilter {
            user_id: Some("owner".into()), tenant_id: Some("tenant".into()),
            service_id: Some("service".into()), status: Some("pending".into()), limit: 2, offset: 1,
        };
        let mut page = storage.list(&filter).await.unwrap();
        assert_eq!(page, vec![originals[2].clone(), originals[1].clone()]);
        page[0].metadata.as_mut().unwrap().clear();
        page[0].request_data.as_mut().unwrap().clear();
        page[0].result = None;
        assert_eq!(storage.get("c").await.unwrap(), originals[2]);
        filter.offset = usize::MAX;
        assert!(storage.list(&filter).await.unwrap().is_empty());
        filter.offset = 0; filter.limit = 0;
        assert!(storage.list(&filter).await.unwrap().is_empty());
        filter.limit = usize::MAX;
        assert_eq!(storage.list(&filter).await.unwrap().len(), 4);
        filter.user_id = Some(String::new()); filter.tenant_id = None;
        filter.service_id = Some(String::new()); filter.status = None;
        assert_eq!(storage.list(&filter).await.unwrap().len(), 8);
    }

    #[tokio::test]
    async fn list_orders_same_instant_by_id_and_preserves_offsets() {
        let storage = MemoryTaskStorage::new();
        let a = row("a", "2026-10-12T12:00:00+00:00", "owner", "tenant");
        let b = row("b", "2026-10-12T14:00:00+02:00", "owner", "tenant");
        let c = row("c", "2026-10-12T07:00:00-05:00", "owner", "tenant");
        for task in [&a, &b, &c] { storage.save(task).await.unwrap(); }
        let page = storage.list(&TaskListFilter { limit: 2, offset: 1, ..Default::default() }).await.unwrap();
        assert_eq!(page[0].created_at.to_isoformat(), "2026-10-12T14:00:00+02:00");
        assert_eq!(page[1].created_at.to_isoformat(), "2026-10-12T12:00:00+00:00");
        assert_eq!(page, vec![b, a]);
    }

    #[tokio::test]
    async fn list_rejects_mixed_timestamps_before_pagination() {
        let storage = MemoryTaskStorage::new();
        storage.save(&row("aware", "2026-10-12T00:00:00Z", "owner", "tenant")).await.unwrap();
        storage.save(&row("naive", "2026-10-11T00:00:00", "other", "tenant")).await.unwrap();
        let mut filter = TaskListFilter { limit: 0, offset: usize::MAX, ..Default::default() };
        assert_eq!(storage.list(&filter).await, Err(TaskError::StorageUnavailable));
        filter.user_id = Some("owner".into()); filter.limit = 1; filter.offset = 0;
        assert_eq!(storage.list(&filter).await.unwrap()[0].task_id, "aware");
    }
}
