//! First-class exercises: a teacher defines a course's exercises (title +
//! order); the dashboard then shows them all, even ones no one's started.

use axum::{
    extract::{Extension, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use hermione_entity::exercises;
use sea_orm::{
    sea_query::OnConflict, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
    QueryFilter, QueryOrder,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::AuthCtx;
use crate::http::{resolve_course, CourseQuery};
use crate::state::AppState;

/// Exercises defined for a course, in display order.
pub async fn list_for_course(
    db: &DatabaseConnection,
    course_id: Uuid,
) -> Result<Vec<exercises::Model>, sea_orm::DbErr> {
    exercises::Entity::find()
        .filter(exercises::Column::CourseId.eq(course_id))
        .order_by_asc(exercises::Column::Position)
        .order_by_asc(exercises::Column::Slug)
        .all(db)
        .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExerciseDto {
    slug: String,
    title: String,
    position: i32,
}

/// GET /api/exercises?course=… — list a course's defined exercises.
pub async fn list(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Query(q): Query<CourseQuery>,
) -> Response {
    let course_id = match resolve_course(&state, ctx, q.course).await {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    match list_for_course(&state.db, course_id).await {
        Ok(rows) => {
            let dtos: Vec<ExerciseDto> = rows
                .into_iter()
                .map(|e| ExerciseDto {
                    slug: e.slug,
                    title: e.title,
                    position: e.position,
                })
                .collect();
            Json(dtos).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
pub struct DefineRequest {
    course: Option<String>,
    exercises: Vec<ExerciseIn>,
}

#[derive(Deserialize)]
struct ExerciseIn {
    slug: String,
    title: Option<String>,
    position: Option<i32>,
}

/// POST /api/exercises — define (upsert) a course's exercises in bulk.
pub async fn define(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Json(body): Json<DefineRequest>,
) -> Response {
    let course_id = match resolve_course(&state, ctx, body.course).await {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    let items: Vec<(String, String, i32)> = body
        .exercises
        .iter()
        .enumerate()
        .filter_map(|(idx, ex)| {
            let slug = ex.slug.trim();
            (!slug.is_empty()).then(|| {
                (
                    slug.to_string(),
                    ex.title.clone().unwrap_or_else(|| slug.to_string()),
                    ex.position.unwrap_or(idx as i32),
                )
            })
        })
        .collect();

    match upsert(&state.db, course_id, &items).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Upserts a course's exercises by `(course_id, slug)`, updating title/position
/// when one already exists. Shared by the define endpoint and repo seeding.
pub async fn upsert(
    db: &DatabaseConnection,
    course_id: Uuid,
    items: &[(String, String, i32)],
) -> Result<(), sea_orm::DbErr> {
    for (slug, title, position) in items {
        let model = exercises::ActiveModel {
            id: Set(Uuid::new_v4()),
            course_id: Set(course_id),
            slug: Set(slug.clone()),
            title: Set(title.clone()),
            position: Set(*position),
            created_at: Set(Utc::now().into()),
        };
        exercises::Entity::insert(model)
            .on_conflict(
                OnConflict::columns([exercises::Column::CourseId, exercises::Column::Slug])
                    .update_columns([exercises::Column::Title, exercises::Column::Position])
                    .to_owned(),
            )
            .exec(db)
            .await?;
    }
    Ok(())
}
