use agent_server::config::{AgentServerConfig, AgentServerStorage};
use agent_server::http::HttpAuth;
use std::collections::HashMap;
use std::path::PathBuf;

fn env(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn loopback_no_token_env() -> HashMap<String, String> {
    env(&[
        ("AGENT_SERVER_GRPC_ENDPOINT", "http://127.0.0.1:9000"),
        ("AGENT_SERVER_AUTH_MODE", "local-no-token"),
    ])
}

fn external_env() -> HashMap<String, String> {
    env(&[
        ("AGENT_SERVER_ADDR", "0.0.0.0:8181"),
        ("AGENT_SERVER_GRPC_ENDPOINT", "http://127.0.0.1:9000"),
        ("AGENT_SERVER_AUTH_MODE", "external"),
        ("AGENT_SERVER_CHAT_TOKEN", "chat-secret-token"),
        ("AGENT_SERVER_ADMIN_TOKEN", "admin-secret-token"),
        ("AGENT_SERVER_ALLOWED_HOSTS", "agent.example.com"),
        (
            "AGENT_SERVER_ALLOWED_ORIGINS",
            "https://console.example.com",
        ),
    ])
}

#[test]
fn loopback_defaults_to_local_address_and_allows_explicit_no_token_mode() {
    let config = AgentServerConfig::parse(&loopback_no_token_env()).expect("valid local config");
    let http = config.http_config();

    assert_eq!(config.grpc_endpoint, "http://127.0.0.1:9000");
    assert!(config.approval_ttl > std::time::Duration::ZERO);
    assert!(config.approval_ttl <= std::time::Duration::from_secs(24 * 60 * 60));
    assert_eq!(
        http.bind_addr,
        "127.0.0.1:8181".parse().expect("socket address")
    );
    assert!(matches!(http.auth, HttpAuth::LocalNoToken));
    assert!(http.allowed_hosts.iter().any(|host| host == "127.0.0.1"));
}

#[test]
fn loopback_no_token_mode_must_be_explicit() {
    let env = env(&[("AGENT_SERVER_GRPC_ENDPOINT", "http://127.0.0.1:9000")]);

    assert!(AgentServerConfig::parse(&env).is_err());
}

#[test]
fn loopback_can_use_one_shared_chat_and_admin_token() {
    let env = env(&[
        ("AGENT_SERVER_GRPC_ENDPOINT", "http://127.0.0.1:9000"),
        ("AGENT_SERVER_AUTH_MODE", "local-shared-token"),
        ("AGENT_SERVER_TOKEN", "one-local-token"),
    ]);

    let config = AgentServerConfig::parse(&env).expect("valid shared-token config");
    assert!(matches!(
        config.http_config().auth,
        HttpAuth::LocalSharedToken(token) if token == "one-local-token"
    ));
}

#[test]
fn external_bind_rejects_missing_role_token_without_echoing_secrets() {
    let mut env = external_env();
    env.insert(
        "AGENT_SERVER_CHAT_TOKEN".to_owned(),
        "sensitive-chat-value".to_owned(),
    );
    env.remove("AGENT_SERVER_ADMIN_TOKEN");

    let error = AgentServerConfig::parse(&env)
        .err()
        .expect("admin token is required");
    assert!(!error.to_string().contains("sensitive-chat-value"));
}

#[test]
fn external_bind_rejects_identical_chat_and_admin_tokens() {
    let mut env = external_env();
    env.insert(
        "AGENT_SERVER_ADMIN_TOKEN".to_owned(),
        "chat-secret-token".to_owned(),
    );

    let error = AgentServerConfig::parse(&env)
        .err()
        .expect("role tokens must be distinct");
    assert!(!error.to_string().contains("chat-secret-token"));
}

#[test]
fn local_no_token_mode_cannot_bind_externally() {
    let env = env(&[
        ("AGENT_SERVER_ADDR", "0.0.0.0:8181"),
        ("AGENT_SERVER_GRPC_ENDPOINT", "http://127.0.0.1:9000"),
        ("AGENT_SERVER_AUTH_MODE", "local-no-token"),
    ]);

    assert!(AgentServerConfig::parse(&env).is_err());
}

#[test]
fn scalable_storage_requires_a_redis_endpoint() {
    let mut env = loopback_no_token_env();
    env.insert("STORAGE_TYPE".to_owned(), "Scalable".to_owned());

    let error = AgentServerConfig::parse(&env)
        .err()
        .expect("Scalable requires Redis");
    assert!(!error.to_string().contains("redis://"));
}

#[test]
fn scalable_storage_uses_the_infra_utils_redis_config() {
    let mut env = loopback_no_token_env();
    env.insert("STORAGE_TYPE".to_owned(), "Scalable".to_owned());
    env.insert("REDIS_URL".to_owned(), "redis://127.0.0.1:6379".to_owned());

    let config = AgentServerConfig::parse(&env).expect("valid scalable config");
    match config.storage {
        AgentServerStorage::Scalable { redis } => {
            assert_eq!(redis.url, "redis://127.0.0.1:6379");
        }
        AgentServerStorage::Standalone => panic!("expected scalable storage"),
    }
}

#[test]
fn approval_ttl_must_be_positive_and_bounded() {
    for ttl in ["0", "86401", "999999999999999999999999"] {
        let mut env = loopback_no_token_env();
        env.insert("AGENT_SERVER_APPROVAL_TTL_SECS".to_owned(), ttl.to_owned());
        assert!(
            AgentServerConfig::parse(&env).is_err(),
            "TTL {ttl} should be rejected"
        );
    }
}

#[test]
fn external_mode_requires_and_preserves_explicit_host_and_origin_allowlists() {
    let env = external_env();
    let config = AgentServerConfig::parse(&env).expect("valid external config");
    let http = config.http_config();

    assert_eq!(http.allowed_hosts, ["agent.example.com"]);
    assert_eq!(http.allowed_origins, ["https://console.example.com"]);
}

#[test]
fn external_mode_rejects_missing_allowlists() {
    let mut env = external_env();
    env.remove("AGENT_SERVER_ALLOWED_ORIGINS");

    assert!(AgentServerConfig::parse(&env).is_err());
}

#[test]
fn skills_roots_and_registry_path_are_parsed_with_bounded_inputs() {
    let mut env = loopback_no_token_env();
    env.insert(
        "AGENT_SERVER_SKILLS_ROOTS".to_owned(),
        "/opt/skills,/srv/agent/skills".to_owned(),
    );
    env.insert(
        "AGENT_SERVER_TOOL_REGISTRY_PATH".to_owned(),
        "/etc/agent-server/tools.json".to_owned(),
    );

    let config = AgentServerConfig::parse(&env).expect("valid paths");
    assert_eq!(
        config.skills_roots,
        vec![
            PathBuf::from("/opt/skills"),
            PathBuf::from("/srv/agent/skills")
        ]
    );
    assert_eq!(
        config.tool_registry_path,
        PathBuf::from("/etc/agent-server/tools.json")
    );

    env.insert(
        "AGENT_SERVER_SKILLS_ROOTS".to_owned(),
        (0..17)
            .map(|index| format!("/skills/{index}"))
            .collect::<Vec<_>>()
            .join(","),
    );
    assert!(AgentServerConfig::parse(&env).is_err());
}

#[test]
fn grpc_endpoint_is_required_and_must_be_valid() {
    let mut env = loopback_no_token_env();
    env.remove("AGENT_SERVER_GRPC_ENDPOINT");
    assert!(AgentServerConfig::parse(&env).is_err());

    env.insert(
        "AGENT_SERVER_GRPC_ENDPOINT".to_owned(),
        "not a grpc endpoint".to_owned(),
    );
    assert!(AgentServerConfig::parse(&env).is_err());

    for endpoint in [
        "unix:///tmp/jobworkerp.sock",
        "ftp://example.com:80",
        "https://",
        "http://user:password@example.com:9000",
        "http://127.0.0.1:9000/another/service",
    ] {
        env.insert("AGENT_SERVER_GRPC_ENDPOINT".to_owned(), endpoint.to_owned());
        assert!(
            AgentServerConfig::parse(&env).is_err(),
            "accepted {endpoint}"
        );
    }
}

#[test]
fn grpc_auth_token_is_optional_but_nonempty_bounded_and_redacted() {
    let mut environment = loopback_no_token_env();
    let config = AgentServerConfig::parse(&environment).expect("auth token is optional");
    assert!(config.grpc_auth_token.is_none());

    environment.insert(
        "AGENT_SERVER_GRPC_AUTH_TOKEN".to_owned(),
        "jobworkerp-secret-token".to_owned(),
    );
    let config = AgentServerConfig::parse(&environment).expect("valid gRPC token");
    assert_eq!(
        config.grpc_auth_token.as_deref(),
        Some("jobworkerp-secret-token")
    );

    for invalid in ["", " ", " token", "token ", "contains whitespace"] {
        environment.insert(
            "AGENT_SERVER_GRPC_AUTH_TOKEN".to_owned(),
            invalid.to_owned(),
        );
        assert!(
            AgentServerConfig::parse(&environment).is_err(),
            "accepted an invalid gRPC auth token"
        );
    }

    environment.insert(
        "AGENT_SERVER_GRPC_AUTH_TOKEN".to_owned(),
        "sensitive-grpc-auth-token ".to_owned(),
    );
    let error = AgentServerConfig::parse(&environment)
        .err()
        .expect("whitespace-containing secret must be rejected");
    assert!(!error.to_string().contains("sensitive-grpc-auth-token"));

    environment.insert("AGENT_SERVER_GRPC_AUTH_TOKEN".to_owned(), "x".repeat(4097));
    let error = AgentServerConfig::parse(&environment)
        .err()
        .expect("oversized gRPC token must be rejected");
    assert!(!error.to_string().contains("xxxx"));

    environment.insert("AGENT_SERVER_GRPC_AUTH_TOKEN".to_owned(), "x".repeat(4096));
    let config = AgentServerConfig::parse(&environment).expect("maximum token length is accepted");
    assert_eq!(config.grpc_auth_token.as_deref().unwrap().len(), 4096);
}

#[test]
fn separate_redis_credentials_are_applied_without_exposing_them_in_errors() {
    let mut env = loopback_no_token_env();
    env.insert("STORAGE_TYPE".to_owned(), "Scalable".to_owned());
    env.insert("REDIS_URL".to_owned(), "redis://127.0.0.1:6379".to_owned());
    env.insert("REDIS_USERNAME".to_owned(), "appuser".to_owned());
    env.insert("REDIS_PASSWORD".to_owned(), "private-password".to_owned());
    let config = AgentServerConfig::parse(&env).unwrap();
    let client = config.storage.redis_client().unwrap();
    let settings = client.get_connection_info().redis_settings();
    assert_eq!(settings.username(), Some("appuser"));
    assert_eq!(settings.password(), Some("private-password"));

    env.insert(
        "REDIS_URL".to_owned(),
        "redis://bad host/private-password".to_owned(),
    );
    let error = AgentServerConfig::parse(&env).err().unwrap();
    assert!(!error.to_string().contains("private-password"));
}

#[test]
fn unsupported_redis_pool_options_fail_instead_of_being_silently_ignored() {
    let mut env = loopback_no_token_env();
    env.insert("STORAGE_TYPE".to_owned(), "Scalable".to_owned());
    env.insert("REDIS_URL".to_owned(), "redis://127.0.0.1:6379".to_owned());
    for name in [
        "REDIS_POOL_SIZE",
        "REDIS_POOL_MIN_IDLE",
        "REDIS_POOL_CONNECTION_TIMEOUT_MSEC",
        "REDIS_POOL_IDLE_TIMEOUT_MSEC",
        "REDIS_POOL_MAX_LIFETIME_MSEC",
    ] {
        env.insert(name.to_owned(), "1".to_owned());
        let error = AgentServerConfig::parse(&env)
            .err()
            .expect("unsupported pool option");
        assert!(error.to_string().contains("not supported"));
        env.remove(name);
    }
}
