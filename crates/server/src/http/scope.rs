//! Which course a request is about, and whether the caller may see it.

use crate::auth::AuthCtx;
use crate::state::AppState;
use crate::tenancy;
use crate::tenancy::DEFAULT_COURSE_ID;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use hermione_entity::sessions;
use sea_orm::EntityTrait;
use uuid::Uuid;

/// Resolves the selected course (by slug, defaulting to "default") and checks
/// the caller may access it. Returns the course id or an error response.
pub async fn resolve_course(
    state: &AppState,
    ctx: AuthCtx,
    slug: Option<String>,
) -> Result<Uuid, Response> {
    let slug = slug.unwrap_or_else(|| "default".to_string());
    authorized_course(state, ctx, &slug).await.map(|c| c.id)
}

/// Loads a session and checks the caller may access its course.
pub(super) async fn authorized_session(
    state: &AppState,
    ctx: AuthCtx,
    session_id: Uuid,
) -> Result<sessions::Model, Response> {
    let Some(session) = sessions::Entity::find_by_id(session_id)
        .one(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response())?
    else {
        return Err((StatusCode::NOT_FOUND, "no such session").into_response());
    };
    let course_id = session.course_id.unwrap_or(DEFAULT_COURSE_ID);
    match ctx {
        AuthCtx::OpenDev => Ok(session),
        AuthCtx::Admin(admin_id) if tenancy::is_member(&state.db, admin_id, course_id).await => {
            Ok(session)
        }
        AuthCtx::Admin(_) => {
            Err((StatusCode::FORBIDDEN, "not a member of this course").into_response())
        }
    }
}

/// Resolves a course by slug and checks the caller may access it, returning the
/// full model (unlike `resolve_course`, which returns just the id).
pub(crate) async fn authorized_course(
    state: &AppState,
    ctx: AuthCtx,
    slug: &str,
) -> Result<hermione_entity::courses::Model, Response> {
    let Some(course) = tenancy::course_by_slug(&state.db, slug).await else {
        return Err((StatusCode::NOT_FOUND, "no such course").into_response());
    };
    match ctx {
        AuthCtx::OpenDev => Ok(course),
        AuthCtx::Admin(admin_id) if tenancy::is_member(&state.db, admin_id, course.id).await => {
            Ok(course)
        }
        AuthCtx::Admin(_) => {
            Err((StatusCode::FORBIDDEN, "not a member of this course").into_response())
        }
    }
}
