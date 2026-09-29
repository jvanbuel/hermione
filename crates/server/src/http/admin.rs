//! Provisioning by the super-admin token: admins and course memberships.

use super::courses::trimmed;
use crate::state::AppState;
use crate::tenancy;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::Json;
use serde::Deserialize;
use std::sync::atomic::Ordering;

#[derive(Deserialize)]
pub(super) struct CreateAdminRequest {
    username: String,
    password: String,
}

pub(super) async fn create_admin_handler(
    State(state): State<AppState>,
    Json(body): Json<CreateAdminRequest>,
) -> Response {
    match tenancy::create_admin(&state.db, &body.username, &body.password).await {
        Ok(admin) => {
            // First admin created: lock down the dashboard.
            state.open_dev.store(false, Ordering::Relaxed);
            Json(serde_json::json!({ "id": admin.id, "username": admin.username })).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
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
) -> Response {
    let repo_url = body
        .repo_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let profile = tenancy::CourseProfile {
        description: trimmed(&body.description),
        term: trimmed(&body.term),
        institution: trimmed(&body.institution),
        level: trimmed(&body.level),
    };
    match tenancy::create_course_with_profile(&state.db, &body.slug, &body.name, repo_url, &profile)
        .await
    {
        Ok(course) => Json(serde_json::json!({
            "slug": course.slug,
            "name": course.name,
            "repoUrl": course.repo_url,
            "enrollmentToken": course.enrollment_token,
        }))
        .into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
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
) -> Response {
    let Some(admin_id) = tenancy::admin_by_username(&state.db, &body.username).await else {
        return (StatusCode::NOT_FOUND, "no such admin").into_response();
    };
    let Some(course) = tenancy::course_by_slug(&state.db, &body.course_slug).await else {
        return (StatusCode::NOT_FOUND, "no such course").into_response();
    };
    match tenancy::grant_membership(&state.db, admin_id, course.id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
