//! Carry the request PDP into the actual supervised SQL future, including when
//! the HTTP waiter is canceled. Resource facts are loaded in that transaction.
use crate::postgres_operation::{PgOperationError, PgOperationStorage, PgQueryFuture};
use ai_platform_auth::cedar::resources;
pub(super) async fn execute<T, F>(
    operations: &PgOperationStorage,
    f: F,
) -> Result<T, PgOperationError>
where
    T: Send + 'static,
    F: for<'c> FnOnce(&'c mut sqlx::PgConnection) -> PgQueryFuture<'c, T> + Send + 'static,
{
    let scope = resources::capture();
    let mut operation = operations.acquire().await?;
    operation
        .execute(move |connection| Box::pin(resources::scoped(scope, f(connection))))
        .await
}
