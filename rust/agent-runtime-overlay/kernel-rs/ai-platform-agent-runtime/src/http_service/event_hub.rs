//! Subscriber-owned per-thread channels. No subscriber means no allocated hub.
use super::{RuntimeBroadcastEvent, RuntimeHttpState};
use crate::postgres_store::event_notifications::{
    CONTROL_CHANNEL, EVENT_CHANNEL, NotificationHint,
};
use codex_protocol::ThreadId;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::sync::broadcast;

type Channels = Mutex<HashMap<ThreadId, broadcast::Sender<HubSignal>>>;
#[derive(Clone)]
pub(super) struct RuntimeEventHub {
    channels: Arc<Channels>,
    capacity: usize,
}
#[derive(Clone, Debug)]
pub(super) enum HubSignal {
    Committed(RuntimeBroadcastEvent),
    Wake,
}
pub(super) struct ThreadEventSubscription {
    channels: Weak<Channels>,
    thread_id: ThreadId,
    receiver: Option<broadcast::Receiver<HubSignal>>,
}
impl RuntimeEventHub {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            channels: Arc::new(Mutex::new(HashMap::new())),
            capacity,
        }
    }
    pub(super) fn subscribe(&self, thread_id: ThreadId) -> ThreadEventSubscription {
        let mut channels = self
            .channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let sender = channels
            .entry(thread_id)
            .or_insert_with(|| broadcast::channel(self.capacity).0);
        ThreadEventSubscription {
            channels: Arc::downgrade(&self.channels),
            thread_id,
            receiver: Some(sender.subscribe()),
        }
    }
    pub(super) fn send(&self, event: RuntimeBroadcastEvent) -> Result<usize, ()> {
        let sender = self
            .channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&event.root_thread_id)
            .cloned();
        sender
            .ok_or(())?
            .send(HubSignal::Committed(event))
            .map_err(|_| ())
    }
    pub(super) fn wake(&self, thread_id: ThreadId) {
        if let Some(sender) = self
            .channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&thread_id)
        {
            let _ = sender.send(HubSignal::Wake);
        }
    }
    pub(super) fn wake_all(&self) {
        for sender in self
            .channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
        {
            let _ = sender.send(HubSignal::Wake);
        }
    }
}
impl ThreadEventSubscription {
    pub(super) async fn recv(&mut self) -> Result<HubSignal, broadcast::error::RecvError> {
        self.receiver
            .as_mut()
            .expect("live subscription receiver")
            .recv()
            .await
    }
}
impl Drop for ThreadEventSubscription {
    fn drop(&mut self) {
        // Drop the receiver before counting, under the same map lock used by
        // subscribe. A concurrent final drop/new subscribe cannot remove a live hub.
        if let Some(channels) = self.channels.upgrade() {
            let mut channels = channels
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.receiver.take();
            if channels
                .get(&self.thread_id)
                .is_some_and(|sender| sender.receiver_count() == 0)
            {
                channels.remove(&self.thread_id);
            }
        }
    }
}

pub(super) fn prune_idle_gates(gates: &mut HashMap<ThreadId, Arc<tokio::sync::Mutex<()>>>) {
    // Every returned gate/owned lock carries an Arc. Count one means only this
    // map retains it; removing it cannot split an active critical section.
    gates.retain(|_, gate| Arc::strong_count(gate) > 1);
}

/// One dedicated LISTEN connection per Runtime process, not per SSE subscriber.
/// It uses the same scoped principal/options, never holds an application pool slot.
pub(super) async fn notification_loop(state: RuntimeHttpState) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .max_lifetime(None)
        .idle_timeout(None)
        .acquire_timeout(Duration::from_secs(5))
        .connect_lazy_with((*state.store.pool.connect_options()).clone());
    let mut maintenance = tokio::time::interval(Duration::from_secs(30));
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    'connect: loop {
        state.prune_thread_gates();
        let connected = tokio::select! {
            () = state.recovery_shutdown.cancelled() => break,
            result = sqlx::postgres::PgListener::connect_with(&pool) => result,
        };
        if let Ok(mut listener) = connected {
            let listening = tokio::select! {
                () = state.recovery_shutdown.cancelled() => break,
                result = tokio::time::timeout(Duration::from_secs(5), listener.listen(EVENT_CHANNEL)) => matches!(result, Ok(Ok(()))),
            };
            let listening = listening
                && matches!(
                    tokio::time::timeout(Duration::from_secs(5), listener.listen(CONTROL_CHANNEL))
                        .await,
                    Ok(Ok(()))
                );
            if listening {
                // The initial LISTEN/reconnect boundary may have lost hints.
                state.events.wake_all();
                loop {
                    tokio::select! {
                        () = state.recovery_shutdown.cancelled() => break 'connect,
                        _ = maintenance.tick() => state.prune_thread_gates(),
                        notification = listener.try_recv() => match notification {
                            Ok(Some(notification)) => {
                                if notification.channel() == CONTROL_CHANNEL {
                                    // Wake a durable ownership/authority scan; a hint
                                    // itself may never cancel or grant execution.
                                    state.recovery_notify.notify_one();
                                } else if notification.channel() == EVENT_CHANNEL {
                                    if let Some(hint) = NotificationHint::parse(notification.payload()) {
                                        // Unknown/unsubscribed UUIDs never allocate a channel,
                                        // query data or acquire an execution/thread claim.
                                        state.events.wake(hint.thread_id());
                                    }
                                }
                            }
                            Ok(None) => state.events.wake_all(), // SQLx reconnected; replay missed hints.
                            Err(_) => break,
                        },
                    }
                }
            }
        }
        tracing::warn!("Runtime event LISTEN unavailable; durable SSE backstop remains active");
        tokio::select! {
            () = state.recovery_shutdown.cancelled() => break,
            () = tokio::time::sleep(Duration::from_secs(5)) => {},
        }
    }
    pool.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(thread: ThreadId, sequence: i64) -> RuntimeBroadcastEvent {
        RuntimeBroadcastEvent {
            root_thread_id: thread,
            event: crate::SequencedAssistantTurnEventV1 {
                sequence,
                event: crate::AssistantTurnEventV1::new(
                    "text_delta",
                    serde_json::json!({"content":"probe"}),
                ),
            },
        }
    }
    #[tokio::test]
    async fn thread_flood_cannot_lag_another_thread_and_last_drop_prunes() {
        let hub = RuntimeEventHub::new(2);
        let a = ThreadId::from_u128(1);
        let b = ThreadId::from_u128(2);
        let mut first = hub.subscribe(a);
        let mut second = hub.subscribe(b);
        for sequence in 1..20 {
            hub.send(event(a, sequence)).unwrap();
        }
        hub.send(event(b, 1)).unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), first.recv())
                .await
                .unwrap(),
            Err(broadcast::error::RecvError::Lagged(_))
        ));
        assert!(
            matches!(tokio::time::timeout(Duration::from_secs(1), second.recv()).await.unwrap(), Ok(HubSignal::Committed(v)) if v.root_thread_id == b && v.event.sequence == 1)
        );
        assert_eq!(hub.channels.lock().unwrap().len(), 2);
        drop(first);
        assert_eq!(hub.channels.lock().unwrap().len(), 1);
        let other = hub.subscribe(b);
        drop(second);
        assert_eq!(hub.channels.lock().unwrap().len(), 1);
        drop(other);
        assert!(hub.channels.lock().unwrap().is_empty());
        hub.wake(a);
        assert!(hub.send(event(a, 20)).is_err());
        assert!(hub.channels.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn dropping_all_hub_owners_closes_subscription() {
        let hub = RuntimeEventHub::new(2);
        let mut subscription = hub.subscribe(ThreadId::from_u128(1));
        drop(hub);
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), subscription.recv())
                .await
                .unwrap(),
            Err(broadcast::error::RecvError::Closed)
        ));
    }
    #[tokio::test]
    async fn active_and_waiting_gate_references_survive_pruning() {
        let thread = ThreadId::from_u128(1);
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let mut gates = HashMap::from([(thread, gate.clone())]);
        let guard = gate.clone().lock_owned().await;
        let waiter = tokio::spawn(async move { gate.lock_owned().await });
        tokio::task::yield_now().await;
        prune_idle_gates(&mut gates);
        assert_eq!(gates.len(), 1);
        drop(guard);
        let waiting_guard = waiter.await.unwrap();
        prune_idle_gates(&mut gates);
        assert_eq!(gates.len(), 1);
        drop(waiting_guard);
        prune_idle_gates(&mut gates);
        assert!(gates.is_empty());
    }
}
