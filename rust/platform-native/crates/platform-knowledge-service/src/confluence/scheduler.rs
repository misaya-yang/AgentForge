//! Dormant native equivalent of the legacy binding/page polling scheduler.
//! Construction never schedules work. The active manual-only entrypoint does not
//! initialize this manager. Execution operations are explicit injected authority.
use super::*;
use chrono::{DateTime, Duration as Delta, Utc};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::Semaphore,
    task::{JoinHandle, JoinSet},
};
type Pending<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;
/// The legacy service's actual synchronization interface. No automatic mapping to
/// manual-sync actor authority is permitted by this scheduler.
pub trait SyncOperations: Send + Sync {
    fn incremental_sync<'a>(&'a self, binding: &'a str) -> Pending<'a, Value>;
    fn trigger_sync<'a>(&'a self, binding: &'a str) -> Pending<'a, Value>;
    fn sync_page<'a>(&'a self, page: &'a str) -> Pending<'a, Value>;
}
#[derive(Clone)]
pub struct PollingStore {
    pub pool: PgPool,
}
impl PollingStore {
    pub async fn bindings(&self) -> Result<Vec<Value>> {
        // Exact source fallback: explicit binding polling does not require an
        // active connection; only inherited connection-level polling does.
        Ok(sqlx::query_scalar("SELECT to_jsonb(b) || jsonb_build_object('polling_interval_minutes',CASE WHEN b.sync_mode='polling' THEN COALESCE(NULLIF(b.polling_interval_minutes,0),60) ELSE c.polling_interval_minutes END) FROM (SELECT * FROM confluence_space_bindings ORDER BY created_at DESC LIMIT 100) b LEFT JOIN confluence_connections c ON c.connection_id=b.connection_id WHERE b.sync_enabled IS DISTINCT FROM FALSE AND (b.sync_mode='polling' OR (b.sync_mode IS NULL AND c.sync_mode='polling' AND c.status='active')) ORDER BY b.created_at DESC").fetch_all(ai_platform_knowledge_domain::database::executor(&self.pool)).await?)
    }
    pub async fn pages(&self) -> Result<Vec<Value>> {
        Ok(sqlx::query_scalar("SELECT to_jsonb(p) FROM confluence_pages p WHERE sync_enabled=TRUE AND sync_mode='polling' ORDER BY sync_priority DESC,next_sync_at ASC NULLS FIRST LIMIT 500").fetch_all(ai_platform_knowledge_domain::database::executor(&self.pool)).await?)
    }
    async fn next(&self, id: &str, page: bool, minutes: i64) -> Result<()> {
        let next = Utc::now() + Delta::minutes(minutes);
        if page {
            sqlx::query("UPDATE confluence_pages SET next_sync_at=$2,updated_at=NOW() WHERE id=$1")
                .bind(id)
                .bind(next)
                .execute(ai_platform_knowledge_domain::database::executor(&self.pool))
                .await?;
        } else {
            sqlx::query("UPDATE confluence_space_bindings SET next_sync_at=$2,updated_at=NOW() WHERE binding_id=$1").bind(id).bind(next).execute(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct PollingTask {
    pub interval_minutes: i64,
    pub priority: i64,
    pub last_run: Option<DateTime<Utc>>,
    pub next_run: Option<DateTime<Utc>>,
    pub is_running: bool,
    pub run_count: u64,
    pub error_count: u64,
    pub last_error: Option<String>,
    order: u64,
}
impl PollingTask {
    pub fn new(interval_minutes: i64, priority: i64) -> Self {
        Self {
            interval_minutes,
            priority,
            last_run: None,
            next_run: None,
            is_running: false,
            run_count: 0,
            error_count: 0,
            last_error: None,
            order: 0,
        }
    }
    pub fn interval_seconds(&self) -> i64 {
        self.interval_minutes.saturating_mul(60)
    }
    pub fn schedule_next(&mut self) {
        self.next_run = Some(Utc::now() + Delta::minutes(self.interval_minutes));
    }
    pub fn should_run(&self) -> bool {
        !self.is_running && self.next_run.is_none_or(|t| Utc::now() >= t)
    }
    fn status(&self, id: &str, page: bool) -> Value {
        // Source emits naive UTC timestamps, despite accepting aware DB values.
        let stamp = |v: Option<DateTime<Utc>>| {
            v.map(|v| v.naive_utc().format("%Y-%m-%dT%H:%M:%S%.f").to_string())
        };
        let mut value = json!({"interval_minutes":self.interval_minutes,"is_running":self.is_running,"last_run":stamp(self.last_run),"next_run":stamp(self.next_run),"run_count":self.run_count,"error_count":self.error_count,"last_error":self.last_error});
        if page {
            value["page_record_id"] = json!(id);
            value["priority"] = json!(self.priority);
        } else {
            value["binding_id"] = json!(id);
        }
        value
    }
}
#[derive(Clone, Debug)]
pub struct SchedulerConfig {
    pub max_concurrent: usize,
    pub error_backoff_minutes: i64,
    pub max_error_backoff_minutes: i64,
    pub check_interval_seconds: u64,
    pub test_mode: bool,
    pub test_interval_seconds: u64,
}
impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 3,
            error_backoff_minutes: 5,
            max_error_backoff_minutes: 60,
            check_interval_seconds: 30,
            test_mode: false,
            test_interval_seconds: 10,
        }
    }
}
#[derive(Default)]
struct State {
    bindings: BTreeMap<String, Arc<Mutex<PollingTask>>>,
    pages: BTreeMap<String, Arc<Mutex<PollingTask>>>,
    active: BTreeSet<(bool, String)>,
    sequence: u64,
}
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
pub struct ConfluenceScheduler {
    store: PollingStore,
    operations: Arc<dyn SyncOperations>,
    pub config: SchedulerConfig,
    state: Mutex<State>,
    running: AtomicBool,
    semaphore: Arc<Semaphore>,
    handle: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}
impl ConfluenceScheduler {
    pub fn new(
        store: PollingStore,
        operations: Arc<dyn SyncOperations>,
        config: SchedulerConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            semaphore: Arc::new(Semaphore::new(config.max_concurrent)),
            store,
            operations,
            config,
            state: Mutex::new(State::default()),
            running: AtomicBool::new(false),
            handle: tokio::sync::Mutex::new(None),
        })
    }
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }
    fn tasks(state: &mut State, page: bool) -> &mut BTreeMap<String, Arc<Mutex<PollingTask>>> {
        if page {
            &mut state.pages
        } else {
            &mut state.bindings
        }
    }
    fn add(&self, id: &str, minutes: i64, priority: i64, immediate: bool, page: bool) {
        let mut state = lock(&self.state);
        if let Some(task) = Self::tasks(&mut state, page).get(id) {
            let mut task = lock(task);
            task.interval_minutes = minutes;
            if page {
                task.priority = priority;
            }
            return;
        }
        let mut task = PollingTask::new(minutes, priority);
        state.sequence += 1;
        task.order = state.sequence;
        if immediate {
            task.next_run = Some(Utc::now());
        } else {
            task.schedule_next();
        }
        Self::tasks(&mut state, page).insert(id.into(), Arc::new(Mutex::new(task)));
    }
    pub fn add_task(&self, id: &str, minutes: i64, immediate: bool) {
        self.add(id, minutes, 0, immediate, false)
    }
    pub fn add_page_task(&self, id: &str, minutes: i64, priority: i64, immediate: bool) {
        self.add(id, minutes, priority, immediate, true)
    }
    pub fn remove_task(&self, id: &str) {
        lock(&self.state).bindings.remove(id);
    }
    pub fn remove_page_task(&self, id: &str) {
        lock(&self.state).pages.remove(id);
    }
    pub fn disable_binding(&self, id: &str) {
        self.remove_task(id);
    }
    pub fn disable_page(&self, id: &str) {
        self.remove_page_task(id);
    }
    pub fn trigger_now(&self, id: &str) -> bool {
        let mut state = lock(&self.state);
        if state.active.contains(&(false, id.into())) {
            return false;
        }
        if !state.bindings.contains_key(id) {
            state.sequence += 1;
            let mut task = PollingTask::new(0, 0);
            task.order = state.sequence;
            state.bindings.insert(id.into(), Arc::new(Mutex::new(task)));
        }
        lock(&state.bindings[id]).next_run = Some(Utc::now());
        true
    }
    fn reschedule(&self, id: &str, minutes: i64, priority: Option<i64>, page: bool) {
        let mut state = lock(&self.state);
        if let Some(task) = Self::tasks(&mut state, page).get(id) {
            let mut task = lock(task);
            task.interval_minutes = minutes;
            if let Some(priority) = priority {
                task.priority = priority;
            }
            task.schedule_next();
            return;
        }
        drop(state);
        self.add(id, minutes, priority.unwrap_or(0), false, page);
    }
    pub fn reschedule_binding(&self, id: &str, minutes: i64) {
        self.reschedule(id, minutes, None, false);
    }
    pub fn reschedule_page(&self, id: &str, minutes: i64, priority: Option<i64>) {
        self.reschedule(id, minutes, priority, true);
    }
    pub fn status(&self) -> Value {
        let state = lock(&self.state);
        let statuses = |map: &BTreeMap<String, Arc<Mutex<PollingTask>>>, page| {
            let mut rows = map
                .iter()
                .map(|(id, t)| {
                    let t = lock(t);
                    (t.order, t.status(id, page))
                })
                .collect::<Vec<_>>();
            rows.sort_by_key(|(order, _)| *order);
            rows.into_iter().map(|(_, v)| v).collect::<Vec<_>>()
        };
        json!({"is_running":self.is_running(),"task_count":state.bindings.len(),"page_task_count":state.pages.len(),"active_sync_count":state.active.iter().filter(|(p,_)|!*p).count(),"active_page_sync_count":state.active.iter().filter(|(p,_)|*p).count(),"max_concurrent":self.config.max_concurrent,"binding_tasks":statuses(&state.bindings,false),"page_tasks":statuses(&state.pages,true)})
    }
    async fn load(&self, page: bool, reload: bool) -> Result<()> {
        let rows = if page {
            self.store.pages().await?
        } else {
            self.store.bindings().await?
        };
        let key = if page { "id" } else { "binding_id" };
        if reload {
            let ids = rows
                .iter()
                .filter_map(|r| r[key].as_str())
                .collect::<BTreeSet<_>>();
            Self::tasks(&mut lock(&self.state), page).retain(|id, _| ids.contains(id.as_str()));
        }
        for row in rows {
            let id = text(&row, key);
            if id.is_empty() {
                continue;
            }
            if !page
                && !reload
                && !["completed", "pending", "syncing"].contains(&text(&row, "status"))
            {
                continue;
            }
            let interval = row["polling_interval_minutes"]
                .as_i64()
                .filter(|n| *n != 0)
                .unwrap_or(60);
            let priority = row["sync_priority"].as_i64().unwrap_or(0);
            let existing = { Self::tasks(&mut lock(&self.state), page).get(id).cloned() };
            if let Some(existing) = existing {
                if reload {
                    let changed = {
                        let task = lock(&existing);
                        task.interval_minutes != interval || (page && task.priority != priority)
                    };
                    if changed {
                        self.reschedule(id, interval, page.then_some(priority), page);
                    }
                } else if !page {
                    lock(&existing).interval_minutes = interval;
                }
            } else {
                self.add(id, interval, priority, page, page);
                if page {
                    if let Some(raw) = row["next_sync_at"].as_str() {
                        let parsed = DateTime::parse_from_rfc3339(raw)
                            .map(|t| t.with_timezone(&Utc))
                            .ok()
                            .or_else(|| {
                                chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f")
                                    .ok()
                                    .map(|t| t.and_utc())
                            });
                        if let Some(parsed) = parsed {
                            if let Some(task) = lock(&self.state).pages.get(id) {
                                lock(task).next_run = Some(parsed);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
    pub async fn reload_bindings(&self) -> Result<()> {
        self.load(false, true).await
    }
    pub async fn reload_pages(&self) -> Result<()> {
        self.load(true, true).await
    }
    pub async fn start(self: &Arc<Self>) {
        let mut handle = self.handle.lock().await;
        if self.running.swap(true, Ordering::AcqRel) {
            return;
        }
        if self.load(false, false).await.is_err() {
            eprintln!("Confluence polling bindings could not be loaded");
        }
        if self.load(true, false).await.is_err() {
            eprintln!("Confluence polling pages could not be loaded");
        }
        let this = self.clone();
        *handle = Some(tokio::spawn(async move { this.run_loop().await }));
    }
    pub async fn stop(&self, timeout: Duration) {
        if !self.running.swap(false, Ordering::AcqRel) {
            return;
        }
        if let Some(handle) = self.handle.lock().await.take() {
            handle.abort();
            let _ = handle.await;
        }
        let wait = async {
            while !lock(&self.state).active.is_empty() {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        };
        let _ = tokio::time::timeout(timeout, wait).await;
        let mut state = lock(&self.state);
        state.bindings.clear();
        state.pages.clear();
        state.active.clear();
    }
    async fn run_loop(self: Arc<Self>) {
        let interval = if self.config.test_mode {
            self.config.check_interval_seconds.min(5)
        } else {
            self.config.check_interval_seconds
        };
        while self.is_running() {
            let mut due = {
                let state = lock(&self.state);
                let mut rows = Vec::new();
                for (page, map) in [(false, &state.bindings), (true, &state.pages)] {
                    for (id, task) in map {
                        let task = lock(task);
                        if task.should_run() {
                            rows.push((page, task.priority, task.order, id.clone()));
                        }
                    }
                }
                rows
            };
            due.sort_by(|a, b| {
                a.0.cmp(&b.0)
                    .then_with(|| {
                        if a.0 {
                            b.1.cmp(&a.1)
                        } else {
                            std::cmp::Ordering::Equal
                        }
                    })
                    .then(a.2.cmp(&b.2))
            });
            let mut pending = JoinSet::new();
            for (page, _, _, id) in due {
                let this = self.clone();
                pending.spawn(async move {
                    this.execute(id, page).await;
                });
            }
            while pending.join_next().await.is_some() {}
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    }
    async fn execute(self: Arc<Self>, id: String, page: bool) {
        let task = {
            let mut state = lock(&self.state);
            let Some(task) = Self::tasks(&mut state, page).get(&id).cloned() else {
                return;
            };
            if lock(&task).is_running || state.active.contains(&(page, id.clone())) {
                return;
            }
            task
        };
        let Ok(_permit) = self.semaphore.acquire().await else {
            return;
        };
        lock(&task).is_running = true;
        lock(&self.state).active.insert((page, id.clone()));
        struct Active {
            scheduler: Arc<ConfluenceScheduler>,
            task: Arc<Mutex<PollingTask>>,
            id: String,
            page: bool,
        }
        impl Drop for Active {
            fn drop(&mut self) {
                lock(&self.task).is_running = false;
                lock(&self.scheduler.state)
                    .active
                    .remove(&(self.page, self.id.clone()));
            }
        }
        let _active = Active {
            scheduler: self.clone(),
            task: task.clone(),
            id: id.clone(),
            page,
        };
        let result = if page {
            self.operations.sync_page(&id).await.and_then(|v| {
                if v["status"] == "error" {
                    Err(Error::unavailable("sync_page returned error"))
                } else {
                    Ok(v)
                }
            })
        } else {
            match self.operations.incremental_sync(&id).await {
                Ok(v) => Ok(v),
                Err(_) => self.operations.trigger_sync(&id).await,
            }
        };
        let minutes = {
            let mut task = lock(&task);
            match result {
                Ok(_) => {
                    task.run_count += 1;
                    task.error_count = 0;
                    task.last_error = None;
                    task.last_run = Some(Utc::now());
                    task.schedule_next();
                    task.interval_minutes
                }
                Err(error) => {
                    task.error_count += 1;
                    task.last_error = Some(
                        error
                            .detail
                            .as_str()
                            .unwrap_or("Confluence synchronization failed")
                            .into(),
                    );
                    let backoff = backoff(
                        self.config.error_backoff_minutes,
                        self.config.max_error_backoff_minutes,
                        task.error_count,
                    );
                    task.next_run = Some(Utc::now() + Delta::minutes(backoff));
                    backoff
                }
            }
        };
        if self.store.next(&id, page, minutes).await.is_err() {
            eprintln!("Confluence polling next-sync persistence failed");
        }
    }
}
fn backoff(initial: i64, maximum: i64, errors: u64) -> i64 {
    initial
        .saturating_mul(
            1i64.checked_shl(errors.saturating_sub(1).min(62) as u32)
                .unwrap_or(i64::MAX),
        )
        .min(maximum)
}
static MANAGER: OnceLock<Mutex<Option<Arc<ConfluenceScheduler>>>> = OnceLock::new();
pub struct SchedulerManager;
impl SchedulerManager {
    pub fn get_instance() -> Option<Arc<ConfluenceScheduler>> {
        lock(MANAGER.get_or_init(|| Mutex::new(None))).clone()
    }
    pub fn initialize(
        store: PollingStore,
        operations: Arc<dyn SyncOperations>,
        config: SchedulerConfig,
    ) -> Arc<ConfluenceScheduler> {
        lock(MANAGER.get_or_init(|| Mutex::new(None)))
            .get_or_insert_with(|| ConfluenceScheduler::new(store, operations, config))
            .clone()
    }
    pub async fn start() {
        if let Some(s) = Self::get_instance() {
            s.start().await;
        }
    }
    pub async fn stop() {
        if let Some(s) = Self::get_instance() {
            s.stop(Duration::from_secs(30)).await;
            lock(MANAGER.get_or_init(|| Mutex::new(None))).take();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn due_time_and_exponential_backoff() {
        let mut task = PollingTask::new(60, 0);
        assert!(task.should_run());
        task.schedule_next();
        assert!(!task.should_run());
        task.next_run = Some(Utc::now() - Delta::seconds(1));
        assert!(task.should_run());
        task.is_running = true;
        assert!(!task.should_run());
        assert_eq!(backoff(5, 60, 1), 5);
        assert_eq!(backoff(5, 60, 4), 40);
        assert_eq!(backoff(5, 60, 100), 60);
    }
}
