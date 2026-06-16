use super::*;
use pretty_assertions::assert_eq;

struct MapEnv {
    values: HashMap<String, String>,
}

impl EnvSource for MapEnv {
    fn var(&self, key: &str) -> Option<String> {
        self.values.get(key).cloned()
    }
}

#[test]
fn environment_fallback_reads_injected_proxy_environment() {
    let env = MapEnv {
        values: HashMap::from([("HTTPS_PROXY".to_string(), "://invalid".to_string())]),
    };
    let origin = RequestOrigin::parse("https://auth.openai.com/oauth/token").expect("valid URL");
    let result = configure_env_proxy_handling(
        &env,
        reqwest::Client::builder(),
        Some(&origin),
        ClientRouteClass::Auth,
    );

    assert!(matches!(
        result,
        Err(BuildRouteAwareHttpClientError::InvalidProxyConfig {
            route_class: ClientRouteClass::Auth,
        })
    ));
}

#[test]
fn parses_pac_proxy_tokens() {
    assert_eq!(
        parse_proxy_list("PROXY proxy.internal:8080; DIRECT", "https"),
        ParsedProxyListDecision::Proxy("http://proxy.internal:8080".to_string())
    );
    assert_eq!(
        parse_proxy_list("HTTPS proxy.internal:8443", "https"),
        ParsedProxyListDecision::Proxy("https://proxy.internal:8443".to_string())
    );
}

#[test]
fn parses_static_winhttp_proxy_entries_for_target_scheme() {
    assert_eq!(
        parse_proxy_list("http=web-proxy:8080;https=secure-proxy:8443", "https"),
        ParsedProxyListDecision::Proxy("http://secure-proxy:8443".to_string())
    );
    assert_eq!(
        parse_proxy_list("http=web-proxy:8080 https=secure-proxy:8443", "https"),
        ParsedProxyListDecision::Proxy("http://secure-proxy:8443".to_string())
    );
    assert_eq!(
        parse_proxy_list("proxy.internal:8080", "https"),
        ParsedProxyListDecision::Proxy("http://proxy.internal:8080".to_string())
    );
}

#[test]
fn reports_direct_and_unsupported_proxy_tokens() {
    assert_eq!(
        parse_proxy_list("DIRECT; PROXY proxy.internal:8080", "https"),
        ParsedProxyListDecision::Direct
    );
    assert_eq!(
        parse_proxy_list("DIRECT", "https"),
        ParsedProxyListDecision::Direct
    );
    assert_eq!(
        parse_proxy_list("SOCKS proxy.internal:1080", "https"),
        ParsedProxyListDecision::UnsupportedScheme
    );
}

#[test]
fn no_proxy_matches_exact_suffix_wildcard_and_port() {
    let origin = RequestOrigin {
        scheme: "https".to_string(),
        host: "auth.openai.com".to_string(),
        port: 443,
    };
    assert!(no_proxy_matches_origin("auth.openai.com", &origin));
    assert!(no_proxy_matches_origin(".openai.com", &origin));
    assert!(no_proxy_matches_origin("*.openai.com", &origin));
    assert!(no_proxy_matches_origin("auth.openai.com:443", &origin));
    assert!(!no_proxy_matches_origin("auth.openai.com:8443", &origin));
}

#[test]
fn system_proxy_cache_key_preserves_url_specific_pac_decisions() {
    let request_url = "https://auth.openai.com/oauth/token?access_token=secret";
    let cache_key = system_proxy_cache_key(request_url);

    assert_ne!(
        cache_key,
        system_proxy_cache_key("https://auth.openai.com/oauth/revoke")
    );
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    assert!(!cache_key.contains(request_url));
}
