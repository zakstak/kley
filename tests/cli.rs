use kley::auth::ResolvedAuth;
use kley::compact::CompactConfig;
use kley::events::event_channel;
use kley::runtime::{RuntimeHooks, SessionRuntime};
use kley::store::{Session, SessionStatus, Store, Turn};

mod cli {
    use super::*;

    fn test_auth() -> ResolvedAuth {
        ResolvedAuth {
            provider: "test".to_string(),
            api_key: "test-key".to_string(),
            base_url: "http://unused".to_string(),
            account_id: None,
        }
    }

    #[tokio::test]
    async fn existing_interactive_flow_still_persists_turns() {
        let store = Store::open_memory().unwrap();
        let (emitter, _receiver) = event_channel();

        let mut runtime = SessionRuntime::new(
            &store,
            test_auth(),
            Some("test-model"),
            None,
            emitter,
            CompactConfig::default(),
            kley::tools::default_registry(std::env::current_dir().unwrap()),
            "system".to_string(),
            RuntimeHooks::default(),
        )
        .unwrap();

        let prompts = vec!["one".to_string(), "two".to_string()];
        kley::agent::run_cli_adapter_with_runtime_for_test(&mut runtime, &prompts)
            .await
            .unwrap();

        let session = Session::get(&store, runtime.session_id()).unwrap();
        assert_eq!(session.status, SessionStatus::Completed);

        let turns = Turn::list_for_session(&store, &session.id).unwrap();
        assert_eq!(turns.len(), 4);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[1].role, "assistant");
        assert_eq!(turns[2].role, "user");
        assert_eq!(turns[3].role, "assistant");
    }
}
