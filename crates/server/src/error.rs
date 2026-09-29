//! What a handler can fail with, and how each failure reaches the caller.
//!
//! Handlers used to build their error responses by hand — `(StatusCode::
//! INTERNAL_SERVER_ERROR, e.to_string()).into_response()` in a `match` arm, dozens
//! of times — which made every early return four lines long, and sent the
//! database's own error text to whoever made the request. A handler now returns
//! `Result<_, ApiError>` and uses `?`; this type decides what the caller sees.
//!
//! There are two kinds of failure and they are told apart in the type, because
//! they deserve opposite treatment:
//!
//! - [`ApiError::Refused`] is the caller's doing — no such course, not a member,
//!   a bad value — and the message is written for them, so it is sent as is.
//! - [`ApiError::Internal`] is ours — a query failed, a task panicked. Its
//!   detail is for the log, not the response: the caller is told only that it
//!   went wrong.

use std::borrow::Cow;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug)]
pub enum ApiError {
    /// A deliberate answer with a message meant to be read: the caller's mistake
    /// (a 4xx), or a known state of the service such as "not configured" (503).
    /// Never a plain 500 — that is [`Self::Internal`].
    Refused(StatusCode, Cow<'static, str>),
    /// Something broke on our side. Logged, never sent.
    Internal(Box<dyn std::error::Error + Send + Sync>),
}

impl ApiError {
    /// A deliberate answer with a message for the caller. Private, so that the
    /// statuses a refusal can carry are exactly the constructors below — a plain
    /// 500 is a failure of ours, and belongs in [`Self::internal`].
    fn refused(status: StatusCode, message: impl Into<Cow<'static, str>>) -> Self {
        Self::Refused(status, message.into())
    }

    pub fn bad_request(message: impl Into<Cow<'static, str>>) -> Self {
        Self::refused(StatusCode::BAD_REQUEST, message)
    }

    pub fn unauthorized(message: impl Into<Cow<'static, str>>) -> Self {
        Self::refused(StatusCode::UNAUTHORIZED, message)
    }

    pub fn forbidden(message: impl Into<Cow<'static, str>>) -> Self {
        Self::refused(StatusCode::FORBIDDEN, message)
    }

    pub fn not_found(message: impl Into<Cow<'static, str>>) -> Self {
        Self::refused(StatusCode::NOT_FOUND, message)
    }

    pub fn conflict(message: impl Into<Cow<'static, str>>) -> Self {
        Self::refused(StatusCode::CONFLICT, message)
    }

    /// An upstream service we depend on failed or answered badly.
    pub fn bad_gateway(message: impl Into<Cow<'static, str>>) -> Self {
        Self::refused(StatusCode::BAD_GATEWAY, message)
    }

    /// A known state of the service, such as "not configured".
    pub fn unavailable(message: impl Into<Cow<'static, str>>) -> Self {
        Self::refused(StatusCode::SERVICE_UNAVAILABLE, message)
    }

    /// Something of ours failed; `error` goes to the log.
    pub fn internal(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::Internal(error.into())
    }
}

impl From<sea_orm::DbErr> for ApiError {
    fn from(e: sea_orm::DbErr) -> Self {
        Self::internal(e)
    }
}

impl ApiError {
    /// A failed insert of something the caller asked to create: a unique
    /// violation means it already exists (their doing, so `message`), anything
    /// else is ours. Only for inserts the caller chose to make — elsewhere a
    /// unique violation is a race, not their mistake.
    pub fn already_exists_or_internal(e: sea_orm::DbErr, message: &'static str) -> Self {
        match e.sql_err() {
            Some(sea_orm::SqlErr::UniqueConstraintViolation(_)) => Self::conflict(message),
            _ => Self::internal(e),
        }
    }
}

impl From<tokio::task::JoinError> for ApiError {
    fn from(e: tokio::task::JoinError) -> Self {
        Self::internal(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Refused(status, message) => (status, message).into_response(),
            Self::Internal(e) => {
                tracing::error!(error = %e, "request failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}

/// A handler's outcome.
pub type ApiResult<T = Response> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn read(e: ApiError) -> (StatusCode, String) {
        let resp = e.into_response();
        let status = resp.status();
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    #[tokio::test]
    async fn a_refusal_says_what_it_was_written_to_say() {
        assert_eq!(
            read(ApiError::not_found("no such course")).await,
            (StatusCode::NOT_FOUND, "no such course".into())
        );
        assert_eq!(
            read(ApiError::forbidden("not a member of this course")).await,
            (StatusCode::FORBIDDEN, "not a member of this course".into())
        );
        assert_eq!(
            read(ApiError::bad_request(format!("bad {}", 1))).await.1,
            "bad 1"
        );
    }

    #[tokio::test]
    async fn an_internal_failure_never_shows_its_detail() {
        let db = sea_orm::DbErr::Custom("relation \"secret_table\" does not exist".into());
        let (status, body) = read(db.into()).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body, "internal error");
        assert!(!body.contains("secret_table"));
    }

    #[tokio::test]
    async fn a_query_error_without_a_constraint_code_stays_internal() {
        let dup = sea_orm::DbErr::Query(sea_orm::RuntimeErr::Internal(
            "duplicate key value violates unique constraint".into(),
        ));
        // Only a recognised unique violation is the caller's doing (the
        // integration tests cover a real duplicate); anything else is ours.
        let err = ApiError::already_exists_or_internal(dup, "taken");
        assert_eq!(read(err).await.0, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn a_known_state_of_the_service_can_still_be_said() {
        let resp = ApiError::unavailable("identity not configured");
        assert_eq!(
            read(resp).await,
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "identity not configured".into()
            )
        );
    }
}
