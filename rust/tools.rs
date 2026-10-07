#![forbid(unsafe_code)]
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Query, State},
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use paperboy::conversion;
use serde::Deserialize;
use std::{path::PathBuf, sync::Arc};
use tokio::{net::UnixListener, sync::Semaphore};

#[derive(Deserialize)]
struct Options {
    extension: String,
    #[serde(default = "default_paper")]
    paper: String,
    #[serde(default = "default_pages")]
    max_pages: usize,
}
fn default_paper() -> String {
    "Letter".into()
}
fn default_pages() -> usize {
    50
}
fn error(code: StatusCode, message: &str) -> Response {
    (code, axum::Json(serde_json::json!({"detail":message}))).into_response()
}
async fn convert(
    State(gate): State<Arc<Semaphore>>,
    options: Result<Query<Options>, axum::extract::rejection::QueryRejection>,
    request: Request<Body>,
) -> Response {
    let Ok(Query(options)) = options else {
        return error(StatusCode::BAD_REQUEST, "Invalid conversion options.");
    };
    if !conversion::SUPPORTED.contains(&options.extension.as_str())
        || !["Letter", "A4"].contains(&options.paper.as_str())
        || !(1..=100).contains(&options.max_pages)
    {
        return error(StatusCode::BAD_REQUEST, "Invalid conversion options.");
    }
    let Ok(_permit) = gate.try_acquire_owned() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Another file is being converted. Try again shortly.",
        );
    };
    let Ok(body) = to_bytes(request.into_body(), 25 * 1024 * 1024).await else {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "The file exceeds the size limit.",
        );
    };
    let result = async {
        let work = tempfile::tempdir()?;
        let source = work.path().join(format!("source{}", options.extension));
        let output = work.path().join("print.pdf");
        tokio::fs::write(&source, body).await?;
        let pages =
            conversion::convert(&source, &output, &options.paper, options.max_pages).await?;
        let bytes = tokio::fs::read(output).await?;
        Ok::<_, anyhow::Error>((pages, bytes))
    }
    .await;
    match result {
        Ok((pages, bytes)) => (
            [
                ("Content-Type", "application/pdf"),
                ("X-Paperboy-Pages", &pages.to_string()),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "This file could not be converted. Send an unlocked PDF or a different copy.",
        ),
    }
}

#[tokio::main(worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("convert") {
        if args.len() != 5 {
            anyhow::bail!("Usage: paperboy-tools convert SOURCE OUTPUT Letter|A4 MAX_PAGES");
        }
        let pages = conversion::convert(
            &PathBuf::from(&args[1]),
            &PathBuf::from(&args[2]),
            &args[3],
            args[4].parse()?,
        )
        .await?;
        println!("{pages}");
        return Ok(());
    }
    let socket = std::env::var("PAPERBOY_CONVERTER_SOCKET")
        .unwrap_or_else(|_| "/run/paperboy/convert.sock".into());
    if args.first().map(String::as_str) == Some("health") {
        let _connection = tokio::net::UnixStream::connect(socket).await?;
        return Ok(());
    }
    if !args.is_empty() {
        anyhow::bail!("Usage: paperboy-tools [convert SOURCE OUTPUT Letter|A4 MAX_PAGES | health]");
    }
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)?;
    let app = Router::new()
        .route("/convert", post(convert))
        .with_state(Arc::new(Semaphore::new(1)));
    axum::serve(listener, app)
        .with_graceful_shutdown(paperboy::process::shutdown_signal())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;
    fn app(gate: Arc<Semaphore>) -> Router {
        Router::new()
            .route("/convert", post(convert))
            .with_state(gate)
    }
    #[tokio::test]
    async fn rejects_bad_options_before_touching_files() {
        for query in [
            "extension=.exe",
            "extension=.png&paper=Bad",
            "extension=.png&max_pages=0",
            "extension=../.png",
        ] {
            let response = app(Arc::new(Semaphore::new(1)))
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/convert?{query}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
    }
    #[tokio::test]
    async fn bounds_request_body_without_content_length() {
        let response = app(Arc::new(Semaphore::new(1)))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/convert?extension=.png")
                    .body(Body::from(vec![0_u8; 25 * 1024 * 1024 + 1]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
    #[tokio::test]
    async fn refuses_concurrent_conversion_before_buffering() {
        let gate = Arc::new(Semaphore::new(1));
        let _permit = gate.acquire().await.unwrap();
        let response = app(gate.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/convert?extension=.png")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
