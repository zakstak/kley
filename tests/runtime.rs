use kley::auth::ResolvedAuth;
use kley::compact::CompactConfig;
use kley::events::{AgentEvent, Transport, event_channel};
use kley::runtime::{AbortResult, RuntimeHooks, SessionRuntime, SubmitResult};
use kley::store::{Session, SessionStatus, Store, Turn};

mod runtime {
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
    async fn submit_prompt_persists_messages() {
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

        let result = runtime.submit_prompt("hello runtime".to_string()).await.unwrap();
        assert!(matches!(result, SubmitResult::Completed { .. }));

        let session = Session::get(&store, runtime.session_id()).unwrap();
        let turns = Turn::list_for_session(&store, &session.id).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].kind, "message");
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[0].content, "hello runtime");
        assert_eq!(turns[1].kind, "message");
        assert_eq!(turns[1].role, "assistant");
        assert!(turns[1].content.contains("hello runtime"));
    }

    #[tokio::test]
    async fn abort_returns_typed_result() {
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

        let abort = runtime.abort_turn().unwrap();
        assert!(matches!(abort, AbortResult::NoActiveTurn { .. }));

        let session = Session::get(&store, runtime.session_id()).unwrap();
        assert_eq!(session.status, SessionStatus::Active);

        let submit = runtime.submit_prompt("still usable".to_string()).await.unwrap();
        assert!(matches!(submit, SubmitResult::Completed { .. }));

        let turns = Turn::list_for_session(&store, runtime.session_id()).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[0].content, "still usable");
        assert_eq!(turns[1].role, "assistant");
        assert!(turns[1].content.contains("still usable"));
    }

    #[tokio::test]
    async fn transport_and_auth_events_are_exposed() {
        let store = Store::open_memory().unwrap();
        let (emitter, receiver) = event_channel();

        emitter.emit(AgentEvent::TokenRefreshed {
            session_id: Some("runtime-test-session".to_string()),
            provider: "openai".to_string(),
        });

        let mut runtime = SessionRuntime::new(
            &store,
            ResolvedAuth {
                provider: "openai".to_string(),
                api_key: "test-key".to_string(),
                base_url: "http://127.0.0.1:9".to_string(),
                account_id: None,
            },
            Some("test-model"),
            None,
            emitter,
            CompactConfig::default(),
            kley::tools::default_registry(std::env::current_dir().unwrap()),
            "system".to_string(),
            RuntimeHooks::default(),
        )
        .unwrap();

        let _ = runtime.submit_prompt("hello transport".to_string()).await.unwrap();

        let events = receiver.drain();
        assert!(events.iter().any(|event| {
            matches!(
                event,
                AgentEvent::TransportSelected {
                    transport: Transport::WebSocket,
                    ..
                }
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                AgentEvent::TransportFallback {
                    from: Transport::WebSocket,
                    to: Transport::Sse,
                    ..
                }
            )
        }));
        assert!(events
            .iter()
            .any(|event| matches!(event, AgentEvent::TokenRefreshed { .. })));
    }
}
