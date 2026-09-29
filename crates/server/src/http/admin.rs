//! Provisioning by the super-admin token: admins and course memberships.

use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::tenancy;
use crate::text::non_blank;

#[derive(Deserialize)]
pub(super) struct CreateAdminRequest {
    username: String,
    password: String,
}

pub(super) async fn create_admin_handler(
    State(state): State<AppState>,
    Json(body): Json<CreateAdminRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let admin = tenancy::create_admin(&state.db, &body.username, &body.password)
        .await
        .map_err(|e| match e {
            tenancy::CreateAdminError::Db(e) => ApiError::already_exists_or_internal(
                e,
                "an admin with that username already exists",
            ),
            e => ApiError::internal(e),
        })?;
    // First admin created: lock down the dashboard.
    state.open_dev.store(false, Ordering::Relaxed);
    Ok(Json(
        serde_json::json!({ "id": admin.id, "username": admin.username }),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CreateCourseRequest {
    slug: String,
    name: String,
    #[serde(default)]
    repo_url: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    term: Option<String>,
    #[serde(default)]
    institution: Option<String>,
    #[serde(default)]
    level: Option<String>,
}

pub(super) async fn create_course_handler(
    State(state): State<AppState>,
    Json(body): Json<CreateCourseRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let repo_url = non_blank(body.repo_url.as_deref());
    let profile = tenancy::CourseProfile {
        description: non_blank(body.description.as_deref()),
        term: non_blank(body.term.as_deref()),
        institution: non_blank(body.institution.as_deref()),
        level: non_blank(body.level.as_deref()),
    };
    let course = tenancy::create_course_with_profile(
        &state.db,
        &body.slug,
        &body.name,
        repo_url.as_deref(),
        &profile,
    )
    .await
    .map_err(|e| {
        ApiError::already_exists_or_internal(e, "a course with that slug already exists")
    })?;
    Ok(Json(serde_json::json!({
        "slug": course.slug,
        "name": course.name,
        "repoUrl": course.repo_url,
        "enrollmentToken": course.enrollment_token,
    })))
}

#[derive(Deserialize)]
pub(super) struct GrantMembershipRequest {
    username: String,
    #[serde(rename = "courseSlug")]
    course_slug: String,
}

pub(super) async fn grant_membership_handler(
    State(state): State<AppState>,
    Json(body): Json<GrantMembershipRequest>,
) -> ApiResult<StatusCode> {
    let admin_id = tenancy::admin_by_username(&state.db, &body.username)
        .await?
        .ok_or_else(|| ApiError::not_found("no such admin"))?;
    let course = tenancy::course_by_slug(&state.db, &body.course_slug)
        .await?
        .ok_or_else(|| ApiError::not_found("no such course"))?;
    tenancy::grant_membership(&state.db, admin_id, course.id).await?;
    Ok(StatusCode::NO_CONTENT)
}
