use std::future::Future;

const AGENT_RUNTIME_TEST_STACK_SIZE_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn run_with_agent_stack<F>(name: &str, future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    let handle = std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(AGENT_RUNTIME_TEST_STACK_SIZE_BYTES)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("test Tokio runtime should build");
            runtime.block_on(Box::pin(future));
        })
        .expect("Agent test thread should start");
    handle.join().expect("Agent test thread should not panic");
}
