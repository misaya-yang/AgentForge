#[test]
fn spaces_collab_two_peers_converge() {
    ai_platform_knowledge_service::spaces_collab::two_peers_converge().expect("peers converge");
}

#[tokio::test]
#[ignore = "requires AI_GATEWAY_TEST_DATABASE_OWNER_URL and AI_GATEWAY_TEST_DATABASE_URL"]
async fn spaces_collab_database() {
    let _owner = std::env::var("AI_GATEWAY_TEST_DATABASE_OWNER_URL")
        .expect("AI_GATEWAY_TEST_DATABASE_OWNER_URL is unset");
    let _api = std::env::var("AI_GATEWAY_TEST_DATABASE_URL").expect("AI_GATEWAY_TEST_DATABASE_URL is unset");
    spaces_collab_two_peers_converge();
}
