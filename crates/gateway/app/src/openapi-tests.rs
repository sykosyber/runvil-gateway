//! The document against the router, the committed artifact, and the source.

use std::collections::BTreeSet;

use axum::http::Method;

use super::document;
use super::exclusions::{Reason, active_exclusions, full_feature_build};
use super::operations::operations;
use super::render::{self, diff};
use super::source;
use crate::registry::{self, Tier};

const ARTIFACT: &str = "openapi.json";
const REGENERATE: &str = "BLESS_OPENAPI=1 cargo test -p gateway --lib openapi";

fn artifact_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(ARTIFACT)
}

fn generated() -> serde_json::Value {
    match document() {
        Ok(document) => document,
        Err(errors) => panic!("{}", errors.join("\n")),
    }
}

#[test]
fn every_mounted_route_is_documented_or_excluded() {
    let document = generated();
    let mut documented = BTreeSet::new();
    let paths = document["paths"].as_object().expect("paths");
    for (path, item) in paths {
        let item = item.as_object().expect("a path item");
        for method in item.keys() {
            documented.insert((path.clone(), method.to_ascii_uppercase()));
        }
    }
    let excluded = active_exclusions();
    for route in registry::all() {
        for method in route.methods {
            let pair = (route.path.to_owned(), method.as_str().to_owned());
            let in_document = documented.contains(&pair);
            let in_exclusion = excluded.iter().any(|exclusion| {
                exclusion.path == route.path && exclusion.methods.contains(method)
            });
            assert!(
                in_document ^ in_exclusion,
                "{method} {} must be exactly one of documented ({in_document}) or excluded ({in_exclusion}, tier {:?})",
                route.path,
                route.tier
            );
        }
    }
    for (path, method) in &documented {
        let mounted = registry::all().into_iter().any(|route| {
            route.path == path && route.methods.iter().any(|item| item.as_str() == method)
        });
        assert!(
            mounted,
            "{method} {path} is documented but nothing mounts it"
        );
    }
    for exclusion in &excluded {
        for method in exclusion.methods {
            let mounted = registry::all()
                .into_iter()
                .any(|route| route.path == exclusion.path && route.methods.contains(method));
            assert!(
                mounted,
                "{method} {} is excluded ({}) but nothing mounts it",
                exclusion.path, exclusion.reason
            );
        }
    }
    for exclusion in &excluded {
        let route = registry::all()
            .into_iter()
            .find(|route| route.path == exclusion.path)
            .expect("the exclusion is mounted");
        match exclusion.kind {
            Reason::OpenAdminDeferred => assert!(
                route.tier == Tier::Open && exclusion.path.starts_with("/admin"),
                "{} is deferred open admin ({})",
                exclusion.path,
                exclusion.reason
            ),
            Reason::WalledTier => assert_eq!(
                route.tier,
                Tier::Walled,
                "{} is walled ({})",
                exclusion.path,
                exclusion.reason
            ),
            Reason::ConfigUiAsset => assert!(
                exclusion.path.starts_with("/config/") && route.tier == Tier::Walled,
                "{} is a config UI asset ({})",
                exclusion.path,
                exclusion.reason
            ),
        }
    }
}

#[test]
fn every_operation_is_bound_to_the_handler_it_names() {
    for operation in operations() {
        let mounts = source::mounts(operation.file).unwrap_or_else(|error| panic!("{error}"));
        let method = operation.method.as_str().to_ascii_lowercase();
        let bound = mounts.iter().any(|mount| {
            mount.path == operation.path
                && mount.method == method
                && mount.handler == operation.handler
        });
        assert!(
            bound,
            "{} {} should bind {} in {}, mounts: {mounts:?}",
            operation.method, operation.path, operation.handler, operation.file
        );
    }
}

#[test]
fn the_multipart_fields_match_parse_form() {
    let fields = source::multipart_fields("crates/gateway/stt/api/src/batch.rs")
        .unwrap_or_else(|error| panic!("{error}"));
    let document = generated();
    #[cfg(feature = "stt")]
    {
        let properties = &document["paths"]["/v1/audio/transcriptions"]["post"]["requestBody"]["content"]
            ["multipart/form-data"]["schema"]["properties"];
        let mut documented: Vec<&str> = properties
            .as_object()
            .expect("multipart properties")
            .keys()
            .map(String::as_str)
            .collect();
        documented.sort_unstable();
        let mut parsed = fields.clone();
        parsed.sort();
        assert_eq!(documented, parsed, "a multipart field was added or dropped");
    }
    #[cfg(not(feature = "stt"))]
    let _ = fields;
}

#[test]
fn the_committed_document_matches_the_generated_one() {
    let document = generated();
    let text = render::render(&document);
    let path = artifact_path();
    let bless = std::env::var_os("BLESS_OPENAPI").is_some();
    assert!(
        !bless || full_feature_build(),
        "BLESS_OPENAPI needs local, web-search, config-ui, and stt; this build is missing at least one"
    );
    let committed = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            assert!(
                bless,
                "{} is missing; write it with: {REGENERATE}",
                path.display()
            );
            std::fs::write(&path, &text)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            return;
        }
        Err(error) => panic!("{}: {error}", path.display()),
    };
    if committed == text {
        return;
    }
    if bless {
        std::fs::write(&path, &text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        return;
    }
    let committed_value: serde_json::Value = serde_json::from_str(&committed)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let changes = diff(&committed_value, &document, 50);
    panic!(
        "{} is stale:\n{}\nRewrite it with: {REGENERATE}",
        path.display(),
        changes.join("\n")
    );
}

#[test]
fn head_follows_get_except_on_the_realtime_upgrade() {
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::header::AUTHORIZATION;
    use axum::http::{Request, StatusCode};
    use std::net::SocketAddr;
    use tower::ServiceExt as _;

    // Built before the probe runtime: error sampling starts its own runtime.
    let document = generated();
    let documented = document["paths"].as_object().expect("paths").clone();
    let (_temp, state) = crate::test_support::walled_fixture();
    let peer: SocketAddr = "127.0.0.1:50000".parse().expect("a socket address");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    for route in registry::all() {
        // Excluded routes include streams that never finish (`/admin/progress`).
        // Implicit HEAD is a claim about the routes the document describes.
        if !route.methods.contains(&Method::GET) || !documented.contains_key(route.path) {
            continue;
        }
        let path = route
            .path
            .replace("{owner}", "owner")
            .replace("{name}", "na%20me")
            .replace("{sha256}", "not-a-digest");
        let once = |method: Method| {
            let path = path.clone();
            let state = state.clone();
            async move {
                let mut request = Request::builder()
                    .method(method)
                    .uri(path)
                    .header(AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .expect("request builds");
                request.extensions_mut().insert(ConnectInfo(peer));
                let response = crate::build_router(state, None)
                    .oneshot(request)
                    .await
                    .expect("the router is infallible");
                let status = response.status();
                let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .expect("the body reads");
                (status, bytes)
            }
        };
        let (get_status, _) = runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), once(Method::GET))
                .await
                .unwrap_or_else(|_| panic!("GET {path} did not answer within 5s"))
        });
        let (head_status, head_body) = runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), once(Method::HEAD))
                .await
                .unwrap_or_else(|_| panic!("HEAD {path} did not answer within 5s"))
        });
        if route.path == "/v1/realtime" {
            assert_eq!(
                get_status,
                StatusCode::BAD_REQUEST,
                "GET /v1/realtime without upgrade headers"
            );
            assert_eq!(
                head_status,
                StatusCode::METHOD_NOT_ALLOWED,
                "HEAD /v1/realtime"
            );
            continue;
        }
        assert_eq!(head_status, get_status, "HEAD {path} status");
        assert!(
            head_body.is_empty(),
            "HEAD {path} body is empty, was {head_body:?}"
        );
    }
}
