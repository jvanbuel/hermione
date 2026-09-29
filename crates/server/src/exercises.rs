//! First-class exercises: a teacher defines a course's exercises (title +
//! order); the dashboard then shows them all, even ones no one's started.

use axum::{
    extract::{Extension, Query, State},
    http::StatusCode,
    response::IntoResponse,
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
use crate::error::ApiResult;
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
) -> ApiResult<impl IntoResponse> {
    let course_id = resolve_course(&state, ctx, q.course).await?;
    let rows = list_for_course(&state.db, course_id).await?;
    Ok(Json(
        rows.into_iter()
            .map(|e| ExerciseDto {
                slug: e.slug,
                title: e.title,
                position: e.position,
            })
            .collect::<Vec<_>>(),
    ))
}

#[derive(Deserialize)]
pub struct DefineRequest {
    course: Option<String>,
    exercises: Vec<ExerciseIn>,
    /// When true, exercises not present in `exercises` are deleted — turning the
    /// bulk upsert into "set the course's exercises to exactly this list" (used
    /// by the dashboard editor). Defaults to false (pure upsert).
    #[serde(default)]
    replace: bool,
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
) -> ApiResult<StatusCode> {
    let course_id = resolve_course(&state, ctx, body.course).await?;

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

    upsert(&state.db, course_id, &items).await?;
    if body.replace {
        let keep: Vec<String> = items.iter().map(|(slug, _, _)| slug.clone()).collect();
        delete_missing(&state.db, course_id, &keep).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Deletes a course's exercise definitions whose slug isn't in `keep`. Only the
/// teacher's definitions are removed; recorded activity keeps its `exercise` slug.
pub async fn delete_missing(
    db: &DatabaseConnection,
    course_id: Uuid,
    keep: &[String],
) -> Result<(), sea_orm::DbErr> {
    let mut cond =
        exercises::Entity::delete_many().filter(exercises::Column::CourseId.eq(course_id));
    if !keep.is_empty() {
        cond = cond.filter(exercises::Column::Slug.is_not_in(keep.iter().cloned()));
    }
    cond.exec(db).await?;
    Ok(())
}

/// Upserts a course's exercises by `(course_id, slug)`, updating title/position
/// when one already exists. Shared by the define endpoint and repo seeding.
///
/// One statement, so either all of them are saved or none. A slug listed twice
/// keeps its last title and position — Postgres refuses to update one row twice
/// in a single statement, and the one-at-a-time version this replaced let the
/// last entry win too.
pub async fn upsert(
    db: &DatabaseConnection,
    course_id: Uuid,
    items: &[(String, String, i32)],
) -> Result<(), sea_orm::DbErr> {
    let mut last: Vec<&(String, String, i32)> = Vec::with_capacity(items.len());
    for item in items.iter().rev() {
        if !last.iter().any(|kept| kept.0 == item.0) {
            last.push(item);
        }
    }
    if last.is_empty() {
        return Ok(());
    }
    let now = Utc::now();
    let models = last
        .into_iter()
        .rev()
        .map(|(slug, title, position)| exercises::ActiveModel {
            id: Set(Uuid::new_v4()),
            course_id: Set(course_id),
            slug: Set(slug.clone()),
            title: Set(title.clone()),
            position: Set(*position),
            created_at: Set(now.into()),
        });
    exercises::Entity::insert_many(models)
        .on_conflict(
            OnConflict::columns([exercises::Column::CourseId, exercises::Column::Slug])
                .update_columns([exercises::Column::Title, exercises::Column::Position])
                .to_owned(),
        )
        .exec(db)
        .await?;
    Ok(())
}
