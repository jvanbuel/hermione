//! Which course a request is about, and whether the caller may see it.

use hermione_entity::{courses, sessions};
use sea_orm::EntityTrait;
use uuid::Uuid;

use crate::auth::AuthCtx;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::tenancy::{self, DEFAULT_COURSE_ID};

/// Whether `ctx` may see `course_id`: everyone may in open dev mode, otherwise
/// only the course's teachers.
async fn ensure_member(state: &AppState, ctx: AuthCtx, course_id: Uuid) -> ApiResult<()> {
    match ctx {
        AuthCtx::OpenDev => Ok(()),
        AuthCtx::Admin(admin_id) if tenancy::is_member(&state.db, admin_id, course_id).await => {
            Ok(())
        }
        AuthCtx::Admin(_) => Err(ApiError::forbidden("not a member of this course")),
    }
}

/// Resolves a course by slug and checks the caller may access it, returning the
/// full model.
pub(crate) async fn authorized_course(
    state: &AppState,
    ctx: AuthCtx,
    slug: &str,
) -> ApiResult<courses::Model> {
    let course = tenancy::course_by_slug(&state.db, slug)
        .await
        .ok_or_else(|| ApiError::not_found("no such course"))?;
    ensure_member(state, ctx, course.id).await?;
    Ok(course)
}

/// The selected course's id (by slug, defaulting to "default"), once the caller
/// is known to be allowed it.
pub async fn resolve_course(
    state: &AppState,
    ctx: AuthCtx,
    slug: Option<String>,
) -> ApiResult<Uuid> {
    let slug = slug.unwrap_or_else(|| "default".to_string());
    authorized_course(state, ctx, &slug).await.map(|c| c.id)
}

/// Loads a session and checks the caller may access its course.
pub(super) async fn authorized_session(
    state: &AppState,
    ctx: AuthCtx,
    session_id: Uuid,
) -> ApiResult<sessions::Model> {
    let session = sessions::Entity::find_by_id(session_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("no such session"))?;
    ensure_member(state, ctx, session.course_id.unwrap_or(DEFAULT_COURSE_ID)).await?;
    Ok(session)
}
